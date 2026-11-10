//! Independent, science-backed nebula surveys. MDW fields retain their TAN-SIP
//! astrometry; Spitzer coverage and HiPS pixels use the archive's Galactic frame.
use super::*;
use std::io::Read;

const HEADER_LIMIT: usize = 1024 * 1024;
const MAX_FIELDS: usize = 16;

struct MdwCandidate {
    name: String,
    center: [f64; 2],
    url: String,
}

struct MdwField {
    name: String,
    url: String,
    width: u32,
    height: u32,
    header: Value,
    size: u64,
}

impl MdwField {
    fn intersects(&self, target: &SkyWcs, width: u32, height: u32) -> Result<bool> {
        let source = super::super::wcs::SourceWcs::new(&self.header, self.width, self.height)?;
        let corners = [
            [-0.5, -0.5],
            [self.width as f64 - 0.5, -0.5],
            [self.width as f64 - 0.5, self.height as f64 - 0.5],
            [-0.5, self.height as f64 - 0.5],
        ];
        let mut points = Vec::new();
        for edge in 0..4 {
            let a = corners[edge];
            let b = corners[(edge + 1) % 4];
            for step in 0..4 {
                let t = step as f64 / 4.;
                points.push(
                    source
                        .pixel_to_sky([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t])
                        .ok_or_else(|| anyhow::anyhow!("MDW returned invalid field astrometry."))?,
                );
            }
        }
        let region = format!(
            "POLYGON ICRS {}",
            points
                .iter()
                .map(|p| format!("{} {}", p[0], p[1]))
                .collect::<Vec<_>>()
                .join(" ")
        );
        Ok(project(&region, target, height)
            .iter()
            .any(|polygon| intersects_rect(polygon, width, height)))
    }
}

impl Archive {
    pub(super) async fn nebula_surveys(
        &self,
        view: &Value,
        header: &Value,
        job: Option<&JobHandle>,
    ) -> Result<(Vec<Band>, Option<String>)> {
        let (width, height) = dimensions(view, header)?;
        let probe_grid = crate::grid::view_grid(view, 1., 64)?;
        let (pw, ph, probe_header) = grid_header(&probe_grid)?;
        let center = pair(&view["center_icrs_deg"])?;
        let radius = search_radius(view)? / 60.;
        let mut tasks = tokio::task::JoinSet::new();
        for band in registry()
            .into_iter()
            .filter(|b| ["MDW", "Spitzer"].contains(&b.survey))
        {
            let archive = self.clone();
            let target_header = header.clone();
            let probe_grid = probe_grid.clone();
            let probe_header = probe_header.clone();
            let job = job.cloned();
            tasks.spawn(async move {
                let result: Result<bool> = async {
                    if band.survey == "MDW" {
                        let target = SkyWcs::new(&target_header, width, height)?;
                        return Ok(!archive
                            .mdw_fields(&target, width, height, job.as_ref())
                            .await?
                            .is_empty());
                    }
                    let path = band
                        .irsa_path()
                        .ok_or_else(|| anyhow::anyhow!("Missing Spitzer archive."))?;
                    if !archive
                        .irsa_coverage(&path, center, radius, job.as_ref())
                        .await?
                    {
                        return Ok(false);
                    }
                    let handle = job.clone().unwrap_or_else(JobHandle::untracked);
                    let image = archive
                        .irsa_cutout(&band, pw, ph, &probe_header, &probe_grid, &handle)
                        .await?;
                    Ok(image.pixels.iter().any(|v| v.is_finite() && *v != 0.))
                }
                .await;
                let label = format!("{} {}", band.survey, band.band);
                (band, label, result)
            });
        }
        let mut bands = Vec::new();
        let mut warnings = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(22);
        while !tasks.is_empty() {
            let task = tokio::select! {
                task = tasks.join_next() => task,
                _ = tokio::time::sleep_until(deadline) => {
                    tasks.abort_all();
                    warnings.push("Some MDW/Spitzer checks timed out; their coverage is unknown.".into());
                    break;
                }
            };
            check(job)?;
            match task {
                Some(Ok((band, _, Ok(true)))) => bands.push(band),
                Some(Ok((_, _, Ok(false)))) => {}
                Some(Ok((_, label, Err(error)))) => warnings.push(format!("{label}: {error}")),
                Some(Err(error)) => {
                    warnings.push(format!("A nebula archive check failed: {error}"))
                }
                None => break,
            }
        }
        bands.sort_by(|a, b| a.id.cmp(&b.id));
        check(job)?;
        Ok((
            bands,
            (!warnings.is_empty()).then(|| {
                format!(
                    "Some independent nebula archives could not be checked: {}",
                    warnings.join("; ")
                )
            }),
        ))
    }

    async fn mdw_fields(
        &self,
        target: &SkyWcs,
        width: u32,
        height: u32,
        job: Option<&JobHandle>,
    ) -> Result<Vec<MdwField>> {
        let center = target
            .pixel_to_sky([(width - 1) as f64 / 2., (height - 1) as f64 / 2.])
            .ok_or_else(|| anyhow::anyhow!("Invalid MDW target grid."))?;
        let radius = sample_wcs(target, width, height)
            .into_iter()
            .map(|p| separation(center, p))
            .fold(0., f64::max);
        let mut candidates = BTreeMap::new();
        for name in ["MDW_DR0_web_v2.csv", "MDW_DR1_web_v2.1.csv"] {
            let url = format!("{}/js/{name}", self.endpoints.mdw);
            let cache = self
                .root
                .join("cache/survey-mdw")
                .join(format!("{}.csv", hash(&url)));
            let bytes = if fresh(&cache, 86400) {
                std::fs::read(&cache)?
            } else {
                let bytes = self
                    .http_bytes(
                        self.client.get(&url).timeout(Duration::from_secs(8)),
                        JSON_LIMIT,
                        job,
                        "MDW field index",
                    )
                    .await?;
                parse_mdw_index(&bytes, &self.endpoints.mdw)?;
                cache_bytes(&cache, &bytes, job)?;
                bytes
            };
            for field in parse_mdw_index(&bytes, &self.endpoints.mdw)? {
                // A full 3.6-degree side is a conservative circumbound; actual
                // overlap is checked below using each field's real SIP header.
                if separation(center, field.center) <= radius + 3.6 {
                    candidates.insert(
                        (field.center[0].to_bits(), field.center[1].to_bits()),
                        field,
                    );
                }
            }
        }
        ensure!(
            candidates.len() <= MAX_FIELDS * 4,
            "MDW coverage needs too many fields. Narrow the frame."
        );
        let mut result = Vec::new();
        for field in candidates.into_values() {
            check(job)?;
            let raw = self.mdw_raw_path(&field.url);
            let cache = self
                .root
                .join("cache/survey-mdw")
                .join(format!("{}.header", hash(&field.url)));
            let bytes = if raw.is_file() {
                let mut bytes = Vec::new();
                std::fs::File::open(&raw)?
                    .take(HEADER_LIMIT as u64)
                    .read_to_end(&mut bytes)?;
                bytes
            } else if fresh(&cache, 86400) {
                std::fs::read(&cache)?
            } else {
                let bytes = self
                    .http_bytes(
                        self.client
                            .get(&field.url)
                            .header(
                                reqwest::header::RANGE,
                                format!("bytes=0-{}", HEADER_LIMIT - 1),
                            )
                            .timeout(Duration::from_secs(10)),
                        HEADER_LIMIT,
                        job,
                        "MDW science field header",
                    )
                    .await?;
                fits::image_header(&bytes)?;
                cache_bytes(&cache, &bytes, job)?;
                bytes
            };
            let (w, h, header) = fits::image_header(&bytes)?;
            let field = MdwField {
                name: field.name,
                url: field.url,
                width: w,
                height: h,
                header,
                size: fits::image_file_size(&bytes)?,
            };
            if field.intersects(target, width, height)? {
                result.push(field);
            }
        }
        ensure!(
            result.len() <= MAX_FIELDS,
            "MDW cutout exceeds the safe field limit. Narrow the frame."
        );
        Ok(result)
    }

    fn mdw_raw_path(&self, url: &str) -> PathBuf {
        self.root
            .join("downloads/surveys/MDW-Halpha/fields-v1")
            .join(format!("{}.fits", hash(url)))
    }

    pub(super) async fn mdw_transfer(
        &self,
        width: u32,
        height: u32,
        header: &Value,
    ) -> Result<Value> {
        let target = SkyWcs::new(header, width, height)?;
        let fields = self.mdw_fields(&target, width, height, None).await?;
        ensure!(
            !fields.is_empty(),
            "MDW has no science coverage in this frame."
        );
        let missing: Vec<_> = fields
            .iter()
            .filter(|field| !self.mdw_raw_path(&field.url).is_file())
            .collect();
        Ok(json!({
            "transfer_bytes":missing.iter().map(|field| field.size).sum::<u64>(),
            "transfer_estimated":!missing.is_empty(),
            "transfer_note":format!(
                "{} full MDW fields to fetch; {} fields already cached. Based on source FITS headers, not the output crop.",
                missing.len(),
                fields.len() - missing.len()
            ),
            "source_fields":fields.iter().map(|field| json!({
                "name":field.name,"size_bytes":field.size,"cached":self.mdw_raw_path(&field.url).is_file()
            })).collect::<Vec<_>>()
        }))
    }

    pub(super) async fn mdw_cutout(
        &self,
        width: u32,
        height: u32,
        header: &Value,
        _grid: &Value,
        job: &JobHandle,
    ) -> Result<fits::Image> {
        let target = SkyWcs::new(header, width, height)?;
        let fields = self.mdw_fields(&target, width, height, Some(job)).await?;
        ensure!(
            !fields.is_empty(),
            "MDW has no science coverage in this frame."
        );
        let count = fields.len();
        let archive = self.clone();
        let fields = parallel_downloads(
            fields.into_iter().enumerate(),
            job,
            move |(index, field), job| {
                let archive = archive.clone();
                async move {
                    let path = archive.mdw_raw_path(&field.url);
                    if !path.is_file() {
                        job.stage(
                            "download",
                            0.,
                            &format!("Downloading MDW Hα field {}/{}", index + 1, count),
                        )?;
                        std::fs::create_dir_all(path.parent().unwrap())?;
                        let mut staging = tempfile::Builder::new()
                            .suffix(".part")
                            .tempfile_in(path.parent().unwrap())?;
                        archive
                            .http_to(
                                archive
                                    .client
                                    .get(&field.url)
                                    .timeout(Duration::from_secs(90)),
                                SURVEY_LIMIT,
                                Some(&job),
                                "MDW calibrated science FITS",
                                |bytes, _| {
                                    staging.write_all(bytes)?;
                                    Ok(())
                                },
                            )
                            .await?;
                        staging.flush()?;
                        let mut bytes = Vec::new();
                        std::fs::File::open(staging.path())?
                            .take(HEADER_LIMIT as u64)
                            .read_to_end(&mut bytes)?;
                        let (w, h, _) = fits::image_header(&bytes)?;
                        ensure!(
                            w == field.width && h == field.height,
                            "MDW changed its science image dimensions."
                        );
                        fits::validate(staging.path())?;
                        check(Some(&job))?;
                        publish(staging, &path)?;
                    } else {
                        fits::validate(&path)?;
                    }
                    Ok(field)
                }
            },
        )
        .await?;
        let mut output = fits::Image::new(
            width,
            height,
            vec![f32::NAN; width as usize * height as usize],
            header.clone(),
        )?;
        let mut weights = vec![0f32; output.pixels.len()];
        for (index, field) in fields.iter().enumerate() {
            job.assembling(
                index as f64 / fields.len() as f64,
                &format!(
                    "Assembling cached MDW Hα field {}/{}",
                    index + 1,
                    fields.len()
                ),
            )?;
            let path = self.mdw_raw_path(&field.url);
            let header = header.clone();
            let handle = job.clone();
            (output, weights) = tokio::task::spawn_blocking(move || -> Result<_> {
                let image = fits::Image::decode(&std::fs::read(&path)?)?;
                let source =
                    super::super::wcs::SourceWcs::new(&image.header, image.width, image.height)?;
                let target = SkyWcs::new(&header, width, height)?;
                for y in 0..height {
                    if y % 16 == 0 {
                        check(Some(&handle))?;
                    }
                    for x in 0..width {
                        if let Some(p) = target
                            .pixel_to_sky([x as f64, y as f64])
                            .and_then(|p| source.sky_to_pixel(p))
                        {
                            let value = bilinear(&image, p);
                            if !value.is_finite() {
                                continue;
                            }
                            let edge = (p[0] + 0.5)
                                .min(p[1] + 0.5)
                                .min(image.width as f64 - 0.5 - p[0])
                                .min(image.height as f64 - 0.5 - p[1]);
                            let weight = (edge / 64.).clamp(0., 1.) as f32;
                            if weight == 0. {
                                continue;
                            }
                            let i = y as usize * width as usize + x as usize;
                            if weights[i] == 0. {
                                output.pixels[i] = 0.;
                            }
                            output.pixels[i] += value * weight;
                            weights[i] += weight;
                        }
                    }
                }
                Ok((output, weights))
            })
            .await??;
        }
        for (value, weight) in output.pixels.iter_mut().zip(weights) {
            if weight > 0. {
                *value /= weight;
            }
        }
        ensure!(
            output.pixels.iter().any(|v| v.is_finite() && *v != 0.),
            "MDW returned no usable science pixels in this frame."
        );
        Ok(output)
    }
}

fn parse_mdw_index(bytes: &[u8], base: &str) -> Result<Vec<MdwCandidate>> {
    let text = std::str::from_utf8(bytes)?;
    let mut lines = text.trim_start_matches('\u{feff}').lines();
    let header: Vec<_> = lines.next().unwrap_or("").split(',').collect();
    ensure!(
        header.len() >= 7 && header[1] == "Field" && header[5] == "Calibrated",
        "MDW returned an unrecognized science field index."
    );
    let mut result = Vec::new();
    for line in lines.filter(|line| !line.trim().is_empty()) {
        let values: Vec<_> = line.split(',').map(str::trim).collect();
        ensure!(
            values.len() >= 7,
            "MDW returned an incomplete science field index."
        );
        let center = [values[2].parse::<f64>()? * 15., values[3].parse::<f64>()?];
        validate_position(center)?;
        let path = values[5];
        ensure!(
            (path.starts_with("data_release/Fields/") || path.starts_with("downloads2/"))
                && path.ends_with(".fits")
                && path
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"/_.-".contains(&c))
                && path
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != ".."),
            "MDW returned an unsafe calibrated FITS reference."
        );
        result.push(MdwCandidate {
            name: format!("Field {}", values[1]),
            center,
            url: format!("{}/{path}", base.trim_end_matches('/')),
        });
    }
    ensure!(
        !result.is_empty(),
        "MDW returned an empty science field index."
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mdw_index_uses_ra_hours_and_only_safe_calibrated_science_links() {
        let row = |path: &str| {
            format!(
                "Image preview,Field,RA(hours),DEC(degrees),Instrumental,Calibrated,Star-Removed\n\
             preview.png,2142,5.7857,-3.225,instrumental.fits,{path},starless.fits\n"
            )
        };
        let fields = parse_mdw_index(
            row("data_release/Fields/2142/calibrated.fits").as_bytes(),
            "https://mdw.astro.columbia.edu",
        )
        .unwrap();
        assert!((fields[0].center[0] - 86.7855).abs() < 1e-10);
        for unsafe_path in [
            "https://other.invalid/image.fits",
            "downloads2/../secret.fits",
            "downloads2/2142/image.jpg",
        ] {
            assert!(
                parse_mdw_index(
                    row(unsafe_path).as_bytes(),
                    "https://mdw.astro.columbia.edu"
                )
                .is_err()
            );
        }
    }
}
