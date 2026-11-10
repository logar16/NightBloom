//! Survey registry, source-backed availability, and exact-grid science cutouts.
use super::*;

#[path = "dss.rs"]
mod dss;
#[path = "nebula.rs"]
mod nebula;
#[path = "preview.rs"]
mod preview;
#[path = "transfer.rs"]
mod transfer;

#[derive(Clone, Debug)]
pub(super) struct Band {
    id: String,
    survey: &'static str,
    band: &'static str,
    nm: f64,
    order: Option<u8>,
    scale: f64,
    galactic: bool,
    fwhm: f64,
}

impl Band {
    fn new(
        id: &str,
        survey: &'static str,
        band: &'static str,
        nm: f64,
        order: u8,
        fwhm: f64,
    ) -> Self {
        Self {
            id: id.into(),
            survey,
            band,
            nm,
            order: Some(order),
            scale: (std::f64::consts::PI / 3.).sqrt().to_degrees() * 3600.
                / (512. * 2f64.powi(order as i32)),
            galactic: false,
            fwhm,
        }
    }
    fn native_scale(&self) -> f64 {
        self.scale
    }
    fn providers(&self) -> Vec<&'static str> {
        match self.survey {
            "DSS2" => {
                if self.band == "red" {
                    vec!["stsci-dss-v1", "irsa-hips-v1"]
                } else {
                    vec!["stsci-dss-v1"]
                }
            }
            "Pan-STARRS" => vec!["stsci-ps1-v1"],
            "WISE" | "Spitzer" => vec!["irsa-hips-v1"],
            "MDW" => vec!["mdw-v1"],
            _ => vec![],
        }
    }
    fn irsa_path(&self) -> Option<String> {
        match self.survey {
            "WISE" => Some(format!("AllWISE/{}", self.band)),
            "DSS2" if self.band == "red" => Some("DSS2/red".into()),
            "Spitzer" => Some(format!("SPITZER/{}", self.band)),
            _ => None,
        }
    }

    fn sources(&self, observation: &Value) -> Result<Vec<&'static str>> {
        let mut providers = self.providers();
        if self.order.is_some() {
            providers.push("cds");
        }
        if let Some(explicit) = observation["survey_source"].as_str() {
            ensure!(
                providers.contains(&explicit),
                "This source does not supply the selected survey."
            );
            providers.retain(|provider| *provider == explicit);
        }
        Ok(providers)
    }
}

fn registry() -> Vec<Band> {
    let mut bands = vec![
        Band {
            id: "MDW/Halpha".into(),
            survey: "MDW",
            band: "Hα",
            nm: 656.3,
            order: None,
            scale: 3.17,
            galactic: false,
            fwhm: 6.,
        },
        Band::new("CDS/P/IPHAS/DR2/halpha", "IPHAS", "Hα", 656.3, 11, 1.),
        Band::new("CDS/P/IPHAS/DR2/r", "IPHAS", "r", 624., 11, 1.),
        Band::new("CDS/P/IPHAS/DR2/i", "IPHAS", "i", 754., 11, 1.),
        Band::new("CDS/P/VPHAS/DR4/Halpha", "VPHAS+", "Hα", 656.3, 11, 1.),
        Band::new("CDS/P/VPHAS/DR4/u", "VPHAS+", "u", 361., 11, 1.),
        Band::new("CDS/P/VPHAS/DR4/g", "VPHAS+", "g", 474., 11, 1.),
        Band::new("CDS/P/VPHAS/DR4/r", "VPHAS+", "r", 633., 11, 1.),
        Band::new("CDS/P/VPHAS/DR4/i", "VPHAS+", "i", 766., 11, 1.),
    ];
    for (band, nm, fwhm) in [
        ("IRAC1", 3600., 1.66),
        ("IRAC2", 4500., 1.72),
        ("IRAC3", 5800., 1.88),
        ("IRAC4", 8000., 1.98),
    ] {
        let mut entry = Band::new(
            &format!("CDS/P/SPITZER/{band}"),
            "Spitzer",
            band,
            nm,
            9,
            fwhm,
        );
        entry.galactic = true;
        bands.push(entry);
    }
    for (band, nm, fwhm) in [
        ("W1", 3400., 6.1),
        ("W2", 4600., 6.4),
        ("W3", 12000., 6.5),
        ("W4", 22000., 12.),
    ] {
        bands.push(Band::new(
            &format!("CDS/P/allWISE/{band}"),
            "WISE",
            band,
            nm,
            8,
            fwhm,
        ));
    }
    for (band, nm) in [("red", 649.), ("blue", 480.), ("NIR", 825.)] {
        bands.push(Band::new(
            &format!("CDS/P/DSS2/{band}"),
            "DSS2",
            band,
            nm,
            9,
            4.7,
        ));
    }
    for (band, nm) in [("g", 477.), ("r", 621.), ("i", 754.), ("z", 868.)] {
        bands.push(Band::new(
            &format!("CDS/P/PanSTARRS/DR1/{band}"),
            "Pan-STARRS",
            band,
            nm,
            11,
            1.1,
        ));
    }
    bands.push(Band::new("CDS/P/SHASSA/SM", "SHASSA", "Hα", 656.3, 4, 48.));
    bands.push(Band::new("CDS/P/VTSS/HaCC", "VTSS", "Hα", 656.3, 3, 96.));
    for (band, nm, order, fwhm) in [
        ("PACS70", 70000., 9, 5.6),
        ("PACS160", 160000., 8, 11.4),
        ("SPIRE-250", 250000., 7, 18.),
        ("SPIRE-350", 350000., 6, 25.),
        ("SPIRE-500", 500000., 5, 36.),
    ] {
        bands.push(Band::new(
            &format!("ESAVO/P/HERSCHEL/{band}"),
            "Herschel",
            band,
            nm,
            order,
            fwhm,
        ));
    }
    for (band, nm) in [("J", 1235.), ("H", 1662.), ("K", 2159.)] {
        bands.push(Band::new(
            &format!("CDS/P/2MASS/{band}"),
            "2MASS",
            band,
            nm,
            9,
            3.3,
        ));
    }
    bands
}

pub(super) fn groups(
    view: &Value,
    wcs: &SkyWcs,
    height: u32,
    bands: &[Band],
) -> Result<Vec<Value>> {
    let center = pair(&view["center_icrs_deg"])?;
    let f = field(view)?;
    let corners = view["corners_icrs_deg"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let region = format!(
        "POLYGON ICRS {}",
        corners
            .iter()
            .filter_map(|p| pair(p).ok())
            .map(|p| format!("{} {}", p[0], p[1]))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let pixels = project(&region, wcs, height);
    let mut result: Vec<Value> = Vec::new();
    for band in bands {
        let index = match result
            .iter()
            .position(|g| text(g, "instrument") == band.survey)
        {
            Some(index) => index,
            None => {
                result.push(json!({"id":format!("S-{}",band.survey),"key":format!("Survey|{}",band.survey),
                    "collection":"Survey","proposal":"","instrument":band.survey,"family":"survey",
                    "field":band.survey,"targets":[],"title":band.survey,"date":"","filters":[],"observations":[]}));
                result.len() - 1
            }
        };
        let mut info = describe(band.band, Some(band.nm));
        if band.band == "Hα" {
            info["line"] = json!("Hα");
            info["shows"] = json!("Ionised hydrogen · diffuse emission");
        }
        if band.survey == "WISE" && ["W3", "W4"].contains(&band.band) {
            info["shows"] = json!("Warm dust · wispy infrared clouds");
        }
        if band.survey == "DSS2" {
            info["shows"] = json!("Photographic sky survey · non-linear intensity");
        }
        if band.survey == "MDW" {
            info["shows"] = json!("Narrowband Hα · faint extended emission");
        }
        if band.survey == "Spitzer" {
            info["shows"] = json!(if ["IRAC3", "IRAC4"].contains(&band.band) {
                "Infrared dust · star-forming clouds"
            } else {
                "Infrared stars · gas and dust"
            });
        }
        if ["IPHAS", "VPHAS+"].contains(&band.survey) {
            info["shows"] = json!(format!(
                "{} · raw frame seams possible",
                info["shows"].as_str().unwrap_or("Optical sky")
            ));
        }
        let obs_id = format!("survey-{}", band.id.replace('/', "-"));
        let soft = band.fwhm > 4. && band.fwhm > 2. * f[0].max(f[1]) * 60. / 4096.;
        let soft_reason = if soft {
            format!(
                "~{}″ detail; crisp only for views ≥ {:.1}°",
                band.fwhm,
                band.fwhm / 2. * 4096. / 3600.
            )
        } else {
            String::new()
        };
        result[index]["filters"]
            .as_array_mut()
            .unwrap()
            .push(json!(band.band));
        result[index]["observations"].as_array_mut().unwrap().push(json!({
            "obs_id":obs_id,"obsid":obs_id,"collection":"Survey","instrument":band.survey,
            "filter":band.band,"name":format!("{} {}",band.survey,band.band),"wavelength_nm":band.nm,
            "native_scale_arcsec":band.native_scale(),"survey_id":band.id,
            "hips_id":band.order.map(|_| band.id.as_str()),
            "data_uri":format!("{}:{}",if band.order.is_some() {"hips"} else {"survey"},band.id),
            "preview_uri":format!("survey:{}",band.id),"region":region,"pixels":pixels,
            "ra":center[0],"dec":center[1],"proposal":"","target":"","title":format!("{} {}",band.survey,band.band),
            "date":"","exptime_s":0.,"info":info,"fwhm_arcsec":band.fwhm,"soft":soft,"soft_reason":soft_reason,
        }));
    }
    Ok(result)
}

impl Archive {
    pub(super) async fn available_surveys(
        &self,
        view: &Value,
        header: &Value,
        job: Option<&JobHandle>,
    ) -> Result<(Vec<Band>, Option<String>)> {
        let (legacy, nebula) = tokio::join!(
            self.legacy_surveys(view, job),
            self.nebula_surveys(view, header, job)
        );
        let (mut bands, warning) = legacy?;
        let (additional, extra_warning) = nebula?;
        bands.extend(additional);
        let warnings: Vec<_> = warning.into_iter().chain(extra_warning).collect();
        Ok((bands, (!warnings.is_empty()).then(|| warnings.join(" "))))
    }

    async fn legacy_surveys(
        &self,
        view: &Value,
        job: Option<&JobHandle>,
    ) -> Result<(Vec<Band>, Option<String>)> {
        check(job)?;
        let center = pair(&view["center_icrs_deg"])?;
        let radius = search_radius(view)? / 60.;
        let bands: Vec<_> = registry()
            .into_iter()
            .filter(|band| !["Pan-STARRS", "MDW", "Spitzer"].contains(&band.survey))
            .collect();
        let expression = bands
            .iter()
            .map(|b| format!("ID={}", b.id))
            .collect::<Vec<_>>()
            .join(" || ");
        let params = [
            ("RA", center[0].to_string()),
            ("DEC", center[1].to_string()),
            ("SR", radius.to_string()),
            ("expr", expression),
            ("get", "record".into()),
            ("fmt", "json".into()),
            ("fields", "ID".into()),
        ];
        let response = self
            .http_bytes(
                self.client
                    .get(&self.endpoints.moc)
                    .query(&params)
                    .timeout(Duration::from_secs(7)),
                JSON_LIMIT,
                job,
                "CDS survey coverage",
            )
            .await;
        check(job)?;
        if let Ok(bytes) = response
            && let Ok(Value::Array(rows)) = serde_json::from_slice::<Value>(&bytes)
            && rows.iter().all(Value::is_object)
        {
            let ids: BTreeSet<_> = rows.iter().filter_map(|r| r["ID"].as_str()).collect();
            return Ok((
                bands
                    .into_iter()
                    .filter(|b| ids.contains(b.id.as_str()))
                    .collect(),
                None,
            ));
        }
        // Design decision: coverage must be measured, not inferred from a survey's
        // advertised sky fraction. IRSA MOCs and real DSS FITS
        // are independent evidence. Missing services become warnings, not bands.
        if let Some(job) = job {
            job.stage(
                "search",
                0.,
                "CDS unavailable; checking independent survey archives",
            )?;
        }
        let points = sample_view(view)?;
        let mut tasks = tokio::task::JoinSet::new();
        for band in bands
            .iter()
            .filter(|b| b.irsa_path().is_some() || b.survey == "DSS2")
        {
            let archive = self.clone();
            let band = band.clone();
            let points = points.clone();
            let job = job.cloned();
            tasks.spawn(async move {
                let mut warnings = Vec::new();
                let mut found = false;
                if let Some(path) = band.irsa_path() {
                    match archive
                        .irsa_coverage(&path, center, radius, job.as_ref())
                        .await
                    {
                        Ok(value) => found |= value,
                        Err(error) => {
                            warnings.push(format!("{} {}: {error}", band.survey, band.band))
                        }
                    }
                }
                if band.survey == "DSS2" && !found {
                    match archive.dss_probe(&band, &points, job.as_ref()).await {
                        Ok(value) => found |= value,
                        Err(error) => warnings.push(format!("DSS2 {}: {error}", band.band)),
                    }
                }
                (if found { vec![band.id] } else { vec![] }, warnings)
            });
        }
        let mut found = BTreeSet::new();
        let mut warnings = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(22);
        while !tasks.is_empty() {
            let task = tokio::select! {
                task = tasks.join_next() => task,
                _ = tokio::time::sleep_until(deadline) => {
                    tasks.abort_all();
                    warnings.push("Some independent coverage services timed out; their coverage is unknown.".into());
                    break;
                }
            };
            let Some(task) = task else {
                break;
            };
            check(job)?;
            match task {
                Ok((ids, errors)) => {
                    found.extend(ids);
                    warnings.extend(errors);
                }
                Err(_) => warnings.push("An independent coverage check could not finish.".into()),
            }
        }
        check(job)?;
        let mut warning = "CDS is unavailable. Showing independently checked survey coverage; some CDS-only surveys could not be checked.".to_string();
        if !warnings.is_empty() {
            warning.push_str(" Some independent coverage checks also failed: ");
            warning.push_str(&warnings.join("; "));
        }
        Ok((
            bands
                .into_iter()
                .filter(|b| found.contains(&b.id))
                .collect(),
            Some(warning),
        ))
    }

    pub(super) async fn irsa_coverage(
        &self,
        path: &str,
        center: [f64; 2],
        radius: f64,
        job: Option<&JobHandle>,
    ) -> Result<bool> {
        let url = format!("{}/{path}/Moc.fits", self.endpoints.irsa);
        let path = self
            .root
            .join("cache/survey-coverage")
            .join(format!("{}.fits", hash(&url)));
        let bytes = if fresh(&path, 86400) {
            std::fs::read(&path)?
        } else {
            let bytes = self
                .http_bytes(
                    self.client.get(&url).timeout(Duration::from_secs(8)),
                    64 * 1024 * 1024,
                    job,
                    "IRSA survey coverage",
                )
                .await?;
            fits::moc(&bytes)?;
            cache_bytes(&path, &bytes, job)?;
            bytes
        };
        let coverage = fits::moc(&bytes)?;
        let center = if coverage.galactic {
            icrs_to_galactic(center)
        } else {
            center
        };
        for (index, uniq) in coverage.cells.into_iter().enumerate() {
            if index % 4096 == 0 {
                check(job)?;
            }
            let mut order = 0;
            while order < 29 && uniq >= (16u64 << (2 * order)) {
                order += 1;
            }
            let cell = uniq - (4u64 << (2 * order));
            let nside = 1u32 << order;
            if cell_intersects_cone(nside, cell, center, radius)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn dss_probe(
        &self,
        band: &Band,
        points: &[[f64; 2]],
        job: Option<&JobHandle>,
    ) -> Result<bool> {
        for point in points {
            let image = self.dss_image(band, *point, 0.1, job, 8).await?;
            if image.pixels.iter().any(|n| n.is_finite()) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn dss_image(
        &self,
        band: &Band,
        center: [f64; 2],
        size: f64,
        job: Option<&JobHandle>,
        timeout: u64,
    ) -> Result<fits::Image> {
        fits::Image::decode(
            &self
                .dss_bytes(band, center, size, job, timeout, SURVEY_LIMIT)
                .await?,
        )
    }

    async fn dss_bytes(
        &self,
        band: &Band,
        center: [f64; 2],
        size: f64,
        job: Option<&JobHandle>,
        timeout: u64,
        limit: usize,
    ) -> Result<Vec<u8>> {
        let v = match band.band {
            "red" => "poss2ukstu_red",
            "blue" => "poss2ukstu_blue",
            "NIR" => "poss2ukstu_ir",
            _ => bail!("Unknown DSS2 band."),
        };
        let params = [
            ("v", v.to_string()),
            ("r", center[0].to_string()),
            ("d", center[1].to_string()),
            ("e", "J2000".into()),
            ("h", size.to_string()),
            ("w", size.to_string()),
            ("f", "fits".into()),
            ("c", "none".into()),
        ];
        self.http_bytes(
            self.client
                .get(&self.endpoints.dss)
                .query(&params)
                .timeout(Duration::from_secs(timeout)),
            limit,
            job,
            "STScI DSS2 science FITS",
        )
        .await
    }

    async fn ps1_files(
        &self,
        points: &[[f64; 2]],
        filters: &str,
        job: Option<&JobHandle>,
    ) -> Result<Vec<(String, String)>> {
        let mut files = BTreeSet::new();
        for point in points {
            let params = [
                ("ra", point[0].to_string()),
                ("dec", point[1].to_string()),
                ("filters", filters.into()),
                ("type", "stack".into()),
            ];
            let key = hash(serde_json::to_vec(&json!([self.endpoints.ps1, params]))?);
            let cache = self
                .root
                .join("cache/survey-ps1")
                .join(format!("{key}.txt"));
            let bytes = if fresh(&cache, 86400) {
                std::fs::read(&cache)?
            } else {
                let bytes = self
                    .http_bytes(
                        self.client
                            .get(format!("{}/ps1filenames.py", self.endpoints.ps1))
                            .query(&params)
                            .timeout(Duration::from_secs(8)),
                        JSON_LIMIT,
                        job,
                        "STScI Pan-STARRS stack metadata",
                    )
                    .await?;
                parse_ps1(&bytes)?;
                cache_bytes(&cache, &bytes, job)?;
                bytes
            };
            files.extend(parse_ps1(&bytes)?);
            ensure!(
                files.len() <= 128,
                "Pan-STARRS returned too many stacks. Narrow the frame."
            );
        }
        Ok(files.into_iter().collect())
    }

    pub(super) async fn download_survey(
        &self,
        observation: &Value,
        grid: &Value,
        job: &JobHandle,
    ) -> Result<PathBuf> {
        let band = registry()
            .into_iter()
            .find(|b| Some(b.id.as_str()) == survey_id(observation))
            .ok_or_else(|| anyhow::anyhow!("Unknown FITS survey band."))?;
        let (width, height, header) = grid_header(grid)?;
        let grid_key = hash(serde_json::to_vec(&json!([width, height, header]))?);
        let providers = band.sources(observation)?;
        let folder = self
            .root
            .join("downloads/surveys")
            .join(band.id.replace('/', "-"));
        let mut errors = Vec::new();
        for provider in providers {
            check(Some(job))?;
            let path = if provider == "cds" {
                folder.clone()
            } else {
                folder.join(provider)
            }
            .join(survey_cache_name(&grid_key, provider));
            if path.is_file() {
                fits::validate(&path)?;
                check(Some(job))?;
                return Ok(path);
            }
            job.stage(
                "download",
                0.,
                &format!(
                    "{} {} {} through {provider}",
                    if provider == "mdw-v1" {
                        "Preparing"
                    } else {
                        "Downloading"
                    },
                    band.survey,
                    band.band
                ),
            )?;
            let result = match provider {
                "cds" => self.cds_cutout(&band, width, height, &header, job).await,
                "irsa-hips-v1" => {
                    self.irsa_cutout(&band, width, height, &header, grid, job)
                        .await
                }
                "mdw-v1" => self.mdw_cutout(width, height, &header, grid, job).await,
                _ => {
                    self.native_cutout(&band, width, height, &header, grid, job)
                        .await
                }
            }
            .and_then(|image| {
                ensure!(
                    image.pixels.iter().any(|n| n.is_finite()),
                    "The selected survey has no finite science pixels in this frame."
                );
                Ok(image)
            });
            check(Some(job))?;
            match result {
                Ok(mut image) => {
                    image.header["AIINTERP"] = json!("bilinear");
                    let bytes = image.encode(provider)?;
                    std::fs::create_dir_all(path.parent().unwrap())?;
                    let mut staging = tempfile::Builder::new()
                        .suffix(".part")
                        .tempfile_in(path.parent().unwrap())?;
                    staging.write_all(&bytes)?;
                    staging.flush()?;
                    fits::validate(staging.path())?;
                    check(Some(job))?;
                    publish(staging, &path)?;
                    return Ok(path);
                }
                Err(error) => {
                    errors.push(format!("{provider}: {error}"));
                    job.stage(
                        "download",
                        0.,
                        &format!("{provider} unavailable; trying the next science source"),
                    )?;
                }
            }
        }
        bail!(
            "Couldn't download {} {} science FITS. {}",
            band.survey,
            band.band,
            errors.join("; ")
        )
    }

    pub(super) fn cached_survey(
        &self,
        observation: &Value,
        grid: &Value,
    ) -> Result<Option<PathBuf>> {
        let band = registry()
            .into_iter()
            .find(|b| Some(b.id.as_str()) == survey_id(observation))
            .ok_or_else(|| anyhow::anyhow!("Unknown FITS survey band."))?;
        let (width, height, header) = grid_header(grid)?;
        let grid_key = hash(serde_json::to_vec(&json!([width, height, header]))?);
        let providers = band.sources(observation)?;
        let folder = self
            .root
            .join("downloads/surveys")
            .join(band.id.replace('/', "-"));
        for provider in providers {
            let path = if provider == "cds" {
                folder.clone()
            } else {
                folder.join(provider)
            }
            .join(survey_cache_name(&grid_key, provider));
            if path.is_file() {
                fits::validate(&path)?;
                return Ok(Some(path));
            }
        }
        Ok(None)
    }

    async fn cds_cutout(
        &self,
        band: &Band,
        width: u32,
        height: u32,
        header: &Value,
        job: &JobHandle,
    ) -> Result<fits::Image> {
        let mut image = fits::Image::new(
            width,
            height,
            vec![f32::NAN; width as usize * height as usize],
            header.clone(),
        )?;
        let mut windows = 0;
        for y in (0..height).step_by(4000) {
            for x in (0..width).step_by(4000) {
                windows += 1;
                ensure!(
                    windows <= 256,
                    "The requested survey cutout needs too many tiles."
                );
                let w = 4000.min(width - x);
                let h = 4000.min(height - y);
                let sub = subheader(header, x, y, w, h)?;
                let mut result = None;
                for endpoint in &self.endpoints.hips2fits {
                    let request = self
                        .client
                        .get(endpoint)
                        .query(&[
                            ("hips", band.id.clone()),
                            ("wcs", sub.to_string()),
                            ("format", "fits".into()),
                        ])
                        .timeout(Duration::from_secs(45));
                    match self
                        .http_bytes(request, SURVEY_LIMIT, Some(job), "CDS survey science FITS")
                        .await
                        .and_then(|bytes| fits::Image::decode(&bytes))
                    {
                        Ok(tile) if tile.width == w && tile.height == h => {
                            result = Some(tile);
                            break;
                        }
                        _ => {
                            check(Some(job))?;
                        }
                    }
                }
                let tile = result.ok_or_else(|| {
                    anyhow::anyhow!(
                        "CDS hips2fits is unavailable or returned invalid science FITS."
                    )
                })?;
                // Verify returned astrometry rather than accepting a correct shape
                // with a silently substituted projection or rotation.
                let source = SkyWcs::new(&tile.header, w, h)?;
                let target = SkyWcs::new(&sub, w, h)?;
                for p in [
                    [0., 0.],
                    [(w - 1) as f64, (h - 1) as f64],
                    [(w - 1) as f64 / 2., (h - 1) as f64 / 2.],
                ] {
                    let sky = target
                        .pixel_to_sky(p)
                        .ok_or_else(|| anyhow::anyhow!("Invalid cutout WCS."))?;
                    let projected = source.sky_to_pixel(sky).ok_or_else(|| {
                        anyhow::anyhow!("The survey returned a different sky grid.")
                    })?;
                    ensure!(
                        (projected[0] - p[0]).hypot(projected[1] - p[1]) < 0.05,
                        "The survey returned a different sky grid."
                    );
                }
                for row in 0..h as usize {
                    let from = row * w as usize;
                    let to = (y as usize + row) * width as usize + x as usize;
                    image.pixels[to..to + w as usize]
                        .copy_from_slice(&tile.pixels[from..from + w as usize]);
                }
            }
        }
        Ok(image)
    }

    async fn native_cutout(
        &self,
        band: &Band,
        width: u32,
        height: u32,
        header: &Value,
        grid: &Value,
        job: &JobHandle,
    ) -> Result<fits::Image> {
        if band.survey == "DSS2" {
            return self
                .dss_cutout(band, width, height, header, grid, job)
                .await;
        }
        let scale = grid["pixel_scale_arcsec"]
            .as_f64()
            .filter(|n| n.is_finite() && *n > 0.)
            .ok_or_else(|| anyhow::anyhow!("Invalid cutout pixel scale."))?;
        let radius_arcsec = if band.survey == "Pan-STARRS" {
            498.
        } else {
            1798.
        };
        let side = ((radius_arcsec * 2f64.sqrt() / scale).floor() as u32)
            .saturating_sub(4)
            .max(1);
        let count = width.div_ceil(side) as u64 * height.div_ceil(side) as u64;
        ensure!(
            count <= 256,
            "This native survey cutout needs too many tiles. Choose a smaller frame or coarser scale."
        );
        let mut out = fits::Image::new(
            width,
            height,
            vec![f32::NAN; width as usize * height as usize],
            header.clone(),
        )?;
        for y in (0..height).step_by(side as usize) {
            for x in (0..width).step_by(side as usize) {
                check(Some(job))?;
                let w = side.min(width - x);
                let h = side.min(height - y);
                let target_header = subheader(header, x, y, w, h)?;
                let target = SkyWcs::new(&target_header, w, h)?;
                let (center, radius, points) = window_sky(&target_header, w, h)?;
                let mut images = Vec::new();
                let files = self.ps1_files(&points, band.band, Some(job)).await?;
                ensure!(
                    files.len() <= 16,
                    "Pan-STARRS returned too many overlapping stacks."
                );
                for (file, _) in files {
                    let params = [
                        ("ra", center[0].to_string()),
                        ("dec", center[1].to_string()),
                        (
                            "size",
                            (((radius * 2.) / 0.25).ceil() as u32 + 4)
                                .min(4000)
                                .to_string(),
                        ),
                        ("format", "fits".into()),
                        ("red", file),
                    ];
                    let bytes = self
                        .http_bytes(
                            self.client
                                .get(format!("{}/fitscut.cgi", self.endpoints.ps1))
                                .query(&params)
                                .timeout(Duration::from_secs(45)),
                            SURVEY_LIMIT,
                            Some(job),
                            "STScI Pan-STARRS science FITS",
                        )
                        .await?;
                    images.push(fits::Image::decode(&bytes)?);
                }
                for image in images {
                    let source =
                        super::wcs::SourceWcs::new(&image.header, image.width, image.height)?;
                    for row in 0..h {
                        if row % 32 == 0 {
                            check(Some(job))?;
                        }
                        for column in 0..w {
                            let dest = (y + row) as usize * width as usize + (x + column) as usize;
                            if out.pixels[dest].is_finite() {
                                continue;
                            }
                            if let Some(p) = target
                                .pixel_to_sky([column as f64, row as f64])
                                .and_then(|p| source.sky_to_pixel(p))
                            {
                                out.pixels[dest] = bilinear(&image, p);
                            }
                        }
                    }
                }
            }
        }
        ensure!(
            out.pixels.iter().any(|n| n.is_finite()),
            "The native survey returned no science coverage for this frame."
        );
        Ok(out)
    }

    async fn irsa_cutout(
        &self,
        band: &Band,
        width: u32,
        height: u32,
        header: &Value,
        grid: &Value,
        job: &JobHandle,
    ) -> Result<fits::Image> {
        let path = band
            .irsa_path()
            .ok_or_else(|| anyhow::anyhow!("IRSA does not supply this survey."))?;
        let scale = grid["pixel_scale_arcsec"]
            .as_f64()
            .filter(|n| n.is_finite() && *n > 0.)
            .ok_or_else(|| anyhow::anyhow!("Invalid survey pixel scale."))?;
        let order = irsa_order(band, scale)?;
        let nside = 512u32 << order;
        let target = SkyWcs::new(header, width, height)?;
        let mut out = fits::Image::new(
            width,
            height,
            vec![f32::NAN; width as usize * height as usize],
            header.clone(),
        )?;
        // Bounded stripes: four ring-centre neighbours per target pixel, with no
        // full-grid coordinate/index arrays or all-tile cache. Finite neighbours
        // are renormalized exactly as in the Python independent survey pipeline.
        let mut tile_count = 0;
        let rows = (262144usize / width as usize).max(1) as u32;
        for y0 in (0..height).step_by(rows as usize) {
            check(Some(job))?;
            let last_row = height.min(y0 + rows);
            let mut values =
                vec![WeightedSample::default(); (last_row - y0) as usize * width as usize];
            // u32 stripe/local indices keep four interpolation requests within
            // 16 MiB per stripe, plus the 4 MiB weighted accumulators.
            let mut requests: BTreeMap<u64, Vec<(u32, u32, f64)>> = BTreeMap::new();
            for y in y0..last_row {
                if (y - y0) % 16 == 0 {
                    check(Some(job))?;
                }
                for x in 0..width {
                    if let Some(mut sky) = target.pixel_to_sky([x as f64, y as f64]) {
                        if band.galactic {
                            sky = icrs_to_galactic(sky);
                        }
                        for (pixel, weight) in healpix_bilinear_weights(nside, sky)? {
                            if weight > 0. {
                                requests.entry(pixel / (512 * 512)).or_default().push((
                                    (y - y0) * width + x,
                                    (pixel % (512 * 512)) as u32,
                                    weight,
                                ));
                            }
                        }
                    }
                }
            }
            ensure!(
                requests.len() <= 512,
                "IRSA cutout exceeds the safe tile limit. Choose a smaller frame."
            );
            for (tile_id, samples) in requests {
                check(Some(job))?;
                tile_count += 1;
                ensure!(
                    tile_count <= 4096,
                    "IRSA cutout exceeds the safe tile limit."
                );
                let (url, cache) = self.irsa_tile(&path, order, tile_id);
                let bytes = if cache.is_file() {
                    std::fs::read(&cache)?
                } else {
                    match self
                        .http_bytes(
                            self.client.get(&url).timeout(Duration::from_secs(30)),
                            SURVEY_LIMIT,
                            Some(job),
                            "IRSA science HiPS tile",
                        )
                        .await
                    {
                        Ok(bytes) => {
                            let tile = fits::Image::decode_tile(&bytes)?;
                            ensure!(
                                tile.width == 512 && tile.height == 512,
                                "IRSA returned an invalid FITS tile shape."
                            );
                            cache_bytes(&cache, &bytes, Some(job))?;
                            bytes
                        }
                        Err(error) if error.to_string().contains("(HTTP 404)") => continue,
                        Err(error) => return Err(error),
                    }
                };
                let tile = fits::Image::decode_tile(&bytes)?;
                ensure!(
                    tile.width == 512 && tile.height == 512,
                    "IRSA returned an invalid FITS tile shape."
                );
                for (destination, local, weight) in samples {
                    let (tx, ty) = deinterleave(local as u64, 9);
                    // HiPS FITS columns follow nested Y; rows follow reversed X.
                    values[destination as usize]
                        .add(tile.pixels[(511 - tx as usize) * 512 + ty as usize], weight);
                }
            }
            let start = y0 as usize * width as usize;
            for (index, value) in values.into_iter().enumerate() {
                out.pixels[start + index] = value.value();
            }
        }
        ensure!(
            out.pixels.iter().any(|n| n.is_finite()),
            "IRSA returned no science coverage for this frame."
        );
        Ok(out)
    }
}

fn icrs_to_galactic(point: [f64; 2]) -> [f64; 2] {
    let (ra, dec) = (point[0].to_radians(), point[1].to_radians());
    let p = [dec.cos() * ra.cos(), dec.cos() * ra.sin(), dec.sin()];
    // ICRS frame rotation, including the FK5 frame bias (Astropy/ERFA convention).
    let matrix = [
        [
            -0.05487565771259168,
            -0.8734370519556163,
            -0.4838350736167154,
        ],
        [0.4941094371927275, -0.44482972122329517, 0.7469821839866676],
        [
            -0.8676661375596587,
            -0.19807633727300075,
            0.4559838136873016,
        ],
    ];
    let v = matrix.map(|row| row.into_iter().zip(p).map(|(a, b)| a * b).sum::<f64>());
    [
        v[1].atan2(v[0]).to_degrees().rem_euclid(360.),
        v[2].atan2(v[0].hypot(v[1])).to_degrees(),
    ]
}

fn survey_cache_name(grid_key: &str, provider: &str) -> String {
    // Existing builds retain their sources; only newly assembled cutouts use these revisions.
    let version = match provider {
        "irsa-hips-v1" => "-bilinear-v2",
        "stsci-dss-v1" => "-overlap-v2",
        _ => "",
    };
    format!("{}{version}.fits", &grid_key[..32])
}

fn irsa_order(band: &Band, scale: f64) -> Result<u8> {
    ensure!(
        scale.is_finite() && scale > 0.,
        "Invalid survey pixel scale."
    );
    let reduction = (scale / band.native_scale()).log2().floor().max(0.) as u8;
    Ok(band
        .order
        .ok_or_else(|| anyhow::anyhow!("This survey has no HiPS sampling."))?
        .saturating_sub(reduction))
}

fn window_sky(header: &Value, width: u32, height: u32) -> Result<([f64; 2], f64, Vec<[f64; 2]>)> {
    let target = SkyWcs::new(header, width, height)?;
    let center = target
        .pixel_to_sky([(width - 1) as f64 / 2., (height - 1) as f64 / 2.])
        .ok_or_else(|| anyhow::anyhow!("Invalid target cutout WCS."))?;
    let points = sample_wcs(&target, width, height);
    let radius = points
        .iter()
        .map(|p| separation(center, *p) * 3600.)
        .fold(0., f64::max);
    Ok((center, radius, points))
}

#[derive(Clone, Copy, Default)]
struct WeightedSample {
    sum: f64,
    weight: f64,
}

impl WeightedSample {
    fn add(&mut self, value: f32, weight: f64) {
        if value.is_finite() && weight.is_finite() && weight > 0. {
            self.sum += value as f64 * weight;
            self.weight += weight;
        }
    }

    fn value(self) -> f32 {
        if self.weight > 0. {
            (self.sum / self.weight) as f32
        } else {
            f32::NAN
        }
    }
}

/// Four HEALPix ring-centre neighbours and bilinear longitude/latitude weights,
/// expressed in nested indexing for the HiPS tile layout. At a pole the virtual
/// pole value is the mean of the four adjacent ring centres, matching Astropy.
fn healpix_bilinear_weights(nside: u32, sky: [f64; 2]) -> Result<[(u64, f64); 4]> {
    ensure!(
        nside.is_power_of_two() && nside <= 1 << 29,
        "Invalid HEALPix resolution."
    );
    validate_position(sky)?;
    let lat = sky[1].to_radians();
    let lon = sky[0].to_radians();
    let first_lat = ring_latitude(nside, 1);
    if lat.abs() >= first_lat {
        let ring = if lat >= 0. { 1 } else { 4 * nside - 1 };
        let pole_weight =
            ((lat.abs() - first_lat) / (std::f64::consts::FRAC_PI_2 - first_lat)).clamp(0., 1.);
        let mut result = [(0, 0.); 4];
        for (index, entry) in result.iter_mut().enumerate() {
            *entry = (
                nested_pixel(
                    nside,
                    [
                        (index as f64 + 0.5) * 90.,
                        ring_latitude(nside, ring).to_degrees(),
                    ],
                )?,
                pole_weight / 4.,
            );
        }
        for (pixel, weight) in ring_neighbours(nside, ring, lon)? {
            if let Some(entry) = result.iter_mut().find(|entry| entry.0 == pixel) {
                entry.1 += (1. - pole_weight) * weight;
            }
        }
        return Ok(result);
    }
    let n = nside as f64;
    let transition = (2f64 / 3.).asin();
    // The half-angle formula avoids subtracting two almost-equal numbers near
    // a pole at native HiPS resolution (nside up to 262144 for these surveys).
    let continuous_ring = if lat > transition {
        n * 6f64.sqrt() * ((std::f64::consts::FRAC_PI_2 - lat) / 2.).sin()
    } else if lat < -transition {
        4. * n - n * 6f64.sqrt() * ((std::f64::consts::FRAC_PI_2 + lat) / 2.).sin()
    } else {
        2. * n - 1.5 * n * lat.sin()
    };
    let upper = (continuous_ring.floor() as u32).clamp(1, 4 * nside - 2);
    let lower = upper + 1;
    let upper_lat = ring_latitude(nside, upper);
    let lower_lat = ring_latitude(nside, lower);
    let lower_weight = ((upper_lat - lat) / (upper_lat - lower_lat)).clamp(0., 1.);
    let a = ring_neighbours(nside, upper, lon)?;
    let b = ring_neighbours(nside, lower, lon)?;
    Ok([
        (a[0].0, a[0].1 * (1. - lower_weight)),
        (a[1].0, a[1].1 * (1. - lower_weight)),
        (b[0].0, b[0].1 * lower_weight),
        (b[1].0, b[1].1 * lower_weight),
    ])
}

fn ring_latitude(nside: u32, ring: u32) -> f64 {
    let n = nside as f64;
    if ring < nside {
        std::f64::consts::FRAC_PI_2 - 2. * (ring as f64 / (6f64.sqrt() * n)).asin()
    } else if ring > 3 * nside {
        -std::f64::consts::FRAC_PI_2 + 2. * ((4 * nside - ring) as f64 / (6f64.sqrt() * n)).asin()
    } else {
        ((2. * n - ring as f64) * 2. / (3. * n)).asin()
    }
}

fn ring_neighbours(nside: u32, ring: u32, lon: f64) -> Result<[(u64, f64); 2]> {
    let polar = ring < nside || ring > 3 * nside;
    let count = 4 * if ring < nside {
        ring
    } else if ring > 3 * nside {
        4 * nside - ring
    } else {
        nside
    };
    let offset = if !polar && (ring - nside) % 2 == 1 {
        0.
    } else {
        0.5
    };
    let spacing = std::f64::consts::TAU / count as f64;
    let position = lon / spacing - offset;
    let left = position.floor() as i64;
    let fraction = (position - left as f64).clamp(0., 1.);
    let latitude = ring_latitude(nside, ring).to_degrees();
    let index = |i: i64| -> Result<u64> {
        let longitude = ((i.rem_euclid(count as i64) as f64 + offset) * spacing)
            .to_degrees()
            .rem_euclid(360.);
        nested_pixel(nside, [longitude, latitude])
    };
    Ok([(index(left)?, 1. - fraction), (index(left + 1)?, fraction)])
}

fn fresh(path: &Path, seconds: u64) -> bool {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < Duration::from_secs(seconds))
}

fn cache_bytes(path: &Path, bytes: &[u8], job: Option<&JobHandle>) -> Result<()> {
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut staging = tempfile::Builder::new()
        .suffix(".part")
        .tempfile_in(path.parent().unwrap())?;
    staging.write_all(bytes)?;
    staging.flush()?;
    check(job)?;
    // Coverage and raw tiles have a different validator than celestial images.
    staging.as_file().sync_all()?;
    staging
        .persist(path)
        .map_err(|_| anyhow::anyhow!("Could not publish survey source cache."))?;
    Ok(())
}

fn sample_view(view: &Value) -> Result<Vec<[f64; 2]>> {
    let center = pair(&view["center_icrs_deg"])?;
    let mut points = vec![center];
    if let Some(corners) = view["corners_icrs_deg"].as_array() {
        points.extend(corners.iter().filter_map(|p| pair(p).ok()));
    }
    Ok(points)
}

fn sample_wcs(wcs: &SkyWcs, width: u32, height: u32) -> Vec<[f64; 2]> {
    let (w, h) = (width as f64 - 1., height as f64 - 1.);
    [
        [w / 2., h / 2.],
        [0., 0.],
        [w, 0.],
        [w, h],
        [0., h],
        [w / 2., 0.],
        [w, h / 2.],
        [w / 2., h],
        [0., h / 2.],
    ]
    .into_iter()
    .filter_map(|p| wcs.pixel_to_sky(p))
    .collect()
}

fn grid_header(grid: &Value) -> Result<(u32, u32, Value)> {
    let width = grid["width"].as_u64().unwrap_or(0);
    let height = grid["height"].as_u64().unwrap_or(0);
    ensure!(
        width > 0
            && height > 0
            && width <= 65536
            && height <= 65536
            && width * height <= 100_000_000,
        "Survey cutout exceeds the supported pixel limit."
    );
    let mut header = SkyWcs::new(&grid["wcs"], width as u32, height as u32)?.header;
    header["NAXIS1"] = json!(width);
    header["NAXIS2"] = json!(height);
    Ok((width as u32, height as u32, header))
}

fn subheader(header: &Value, x: u32, y: u32, width: u32, height: u32) -> Result<Value> {
    let mut h = header.clone();
    h["CRPIX1"] = json!(
        numeric(header, "CRPIX1").ok_or_else(|| anyhow::anyhow!("Missing cutout CRPIX1."))?
            - x as f64
    );
    h["CRPIX2"] = json!(
        numeric(header, "CRPIX2").ok_or_else(|| anyhow::anyhow!("Missing cutout CRPIX2."))?
            - y as f64
    );
    h["NAXIS1"] = json!(width);
    h["NAXIS2"] = json!(height);
    Ok(h)
}

fn parse_ps1(bytes: &[u8]) -> Result<Vec<(String, String)>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("Invalid Pan-STARRS stack metadata."))?;
    let mut lines = text.lines().filter(|s| !s.trim().is_empty());
    let columns: Vec<_> = lines.next().unwrap_or("").split_whitespace().collect();
    let filename = columns
        .iter()
        .position(|s| *s == "filename")
        .ok_or_else(|| anyhow::anyhow!("Pan-STARRS returned invalid image metadata."))?;
    let filter = columns
        .iter()
        .position(|s| *s == "filter")
        .ok_or_else(|| anyhow::anyhow!("Pan-STARRS returned invalid image metadata."))?;
    let mut files = Vec::new();
    for line in lines {
        let fields: Vec<_> = line.split_whitespace().collect();
        let file = fields
            .get(filename)
            .ok_or_else(|| anyhow::anyhow!("Pan-STARRS returned incomplete image metadata."))?;
        let band = fields
            .get(filter)
            .ok_or_else(|| anyhow::anyhow!("Pan-STARRS returned incomplete filter metadata."))?;
        ensure!(
            file.len() <= 2048 && file.ends_with(".fits") && !file.chars().any(|c| c.is_control()),
            "Pan-STARRS supplied an invalid science file reference."
        );
        ensure!(
            ["g", "r", "i", "z", "y"].contains(band),
            "Pan-STARRS supplied an invalid filter."
        );
        files.push((file.to_string(), band.to_string()));
    }
    Ok(files)
}

fn bilinear(image: &fits::Image, p: [f64; 2]) -> f32 {
    if !p.iter().all(|n| n.is_finite())
        || p[0] < -0.5
        || p[1] < -0.5
        || p[0] > image.width as f64 - 0.5
        || p[1] > image.height as f64 - 0.5
    {
        return f32::NAN;
    }
    let x = p[0].clamp(0., image.width as f64 - 1.);
    let y = p[1].clamp(0., image.height as f64 - 1.);
    let (x0, y0) = (x.floor() as u32, y.floor() as u32);
    let (fx, fy) = (x - x.floor(), y - y.floor());
    let mut value = 0.;
    let mut weight = 0.;
    for (xx, yy, w) in [
        (x0, y0, (1. - fx) * (1. - fy)),
        ((x0 + 1).min(image.width - 1), y0, fx * (1. - fy)),
        (x0, (y0 + 1).min(image.height - 1), (1. - fx) * fy),
        (
            (x0 + 1).min(image.width - 1),
            (y0 + 1).min(image.height - 1),
            fx * fy,
        ),
    ] {
        let sample = image.pixels[yy as usize * image.width as usize + xx as usize];
        if sample.is_finite() {
            value += sample as f64 * w;
            weight += w;
        }
    }
    if weight > 0. {
        (value / weight) as f32
    } else {
        f32::NAN
    }
}

/// HEALPix nested index, including polar caps and longitude wrap.
fn nested_pixel(nside: u32, sky: [f64; 2]) -> Result<u64> {
    ensure!(
        nside.is_power_of_two() && nside <= 1 << 29,
        "Invalid HEALPix resolution."
    );
    validate_position(sky)?;
    let n = nside as i64;
    let z = sky[1].to_radians().sin();
    let za = z.abs();
    let tt = sky[0].rem_euclid(360.) / 90.;
    let (face, ix, iy) = if za <= 2. / 3. {
        let t1 = n as f64 * (0.5 + tt);
        let t2 = n as f64 * z * 0.75;
        let jp = (t1 - t2).floor() as i64;
        let jm = (t1 + t2).floor() as i64;
        let ifp = jp / n;
        let ifm = jm / n;
        let face = if ifp == ifm {
            ifp | 4
        } else if ifp < ifm {
            ifp
        } else {
            ifm + 8
        };
        (face, jm.rem_euclid(n), n - jp.rem_euclid(n) - 1)
    } else {
        let quadrant = (tt.floor() as i64).min(3);
        let tp = tt - quadrant as f64;
        let tmp = n as f64 * (3. * (1. - za)).sqrt();
        let jp = ((tp * tmp).floor() as i64).min(n - 1);
        let jm = (((1. - tp) * tmp).floor() as i64).min(n - 1);
        if z >= 0. {
            (quadrant, n - jm - 1, n - jp - 1)
        } else {
            (quadrant + 8, jp, jm)
        }
    };
    let mut local = 0u64;
    for bit in 0..nside.trailing_zeros() {
        local |= ((ix as u64 >> bit) & 1) << (2 * bit);
        local |= ((iy as u64 >> bit) & 1) << (2 * bit + 1);
    }
    Ok(face as u64 * nside as u64 * nside as u64 + local)
}

fn deinterleave(pixel: u64, order: u32) -> (u32, u32) {
    let (mut x, mut y) = (0, 0);
    for bit in 0..order {
        x |= (((pixel >> (2 * bit)) & 1) as u32) << bit;
        y |= (((pixel >> (2 * bit + 1)) & 1) as u32) << bit;
    }
    (x, y)
}

fn nested_center(nside: u32, pixel: u64) -> Result<[f64; 2]> {
    let face = pixel / (nside as u64 * nside as u64);
    ensure!(face < 12, "Invalid HEALPix coverage pixel.");
    let (ix, iy) = deinterleave(
        pixel % (nside as u64 * nside as u64),
        nside.trailing_zeros(),
    );
    let n = nside as i64;
    let jr = [2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4][face as usize] * n - ix as i64 - iy as i64 - 1;
    let (nr, z, kshift) = if jr < n {
        (jr, 1. - (jr * jr) as f64 / (3. * n as f64 * n as f64), 0)
    } else if jr > 3 * n {
        let nr = 4 * n - jr;
        (nr, -1. + (nr * nr) as f64 / (3. * n as f64 * n as f64), 0)
    } else {
        (n, (2 * n - jr) as f64 * 2. / (3. * n as f64), (jr - n) & 1)
    };
    let mut jp = ([1, 3, 5, 7, 0, 2, 4, 6, 1, 3, 5, 7][face as usize] * nr + ix as i64 - iy as i64
        + 1
        + kshift)
        / 2;
    if jp > 4 * nr {
        jp -= 4 * nr;
    }
    if jp < 1 {
        jp += 4 * nr;
    }
    let phi = (jp as f64 - (kshift + 1) as f64 * 0.5) * std::f64::consts::PI / (2. * nr as f64);
    Ok([
        phi.to_degrees().rem_euclid(360.),
        z.clamp(-1., 1.).asin().to_degrees(),
    ])
}

fn cell_intersects_cone(nside: u32, pixel: u64, center: [f64; 2], radius: f64) -> Result<bool> {
    let distance = separation(center, nested_center(nside, pixel)?);
    if distance <= radius || nested_pixel(nside, center)? == pixel {
        return Ok(true);
    }
    if distance > radius + (2. / nside as f64).to_degrees() {
        return Ok(false);
    }
    // The disk above is only a rejection bound, never evidence of coverage.
    // Minimize distance on the four curved HEALPix boundaries. This avoids
    // announcing coverage on the opposite side of a coarse MOC cell.
    let face = pixel / (nside as u64 * nside as u64);
    let (x, y) = deinterleave(
        pixel % (nside as u64 * nside as u64),
        nside.trailing_zeros(),
    );
    for edge in 0..4 {
        let distance_at = |t: f64| {
            let (xx, yy) = match edge {
                0 => (x as f64 + t, y as f64),
                1 => (x as f64 + 1., y as f64 + t),
                2 => (x as f64 + t, y as f64 + 1.),
                _ => (x as f64, y as f64 + t),
            };
            separation(center, face_position(nside, face as usize, xx, yy))
        };
        let mut best = 0usize;
        let mut minimum = f64::INFINITY;
        for sample in 0..=8 {
            let value = distance_at(sample as f64 / 8.);
            if value <= radius + 1e-10 {
                return Ok(true);
            }
            if value < minimum {
                minimum = value;
                best = sample;
            }
        }
        let mut lo = best.saturating_sub(1) as f64 / 8.;
        let mut hi = (best + 1).min(8) as f64 / 8.;
        for _ in 0..32 {
            let a = (2. * lo + hi) / 3.;
            let b = (lo + 2. * hi) / 3.;
            if distance_at(a) < distance_at(b) {
                hi = b;
            } else {
                lo = a;
            }
        }
        if distance_at((lo + hi) / 2.) <= radius + 1e-10 {
            return Ok(true);
        }
    }
    Ok(false)
}

fn face_position(nside: u32, face: usize, x: f64, y: f64) -> [f64; 2] {
    let n = nside as f64;
    let jr = [2., 2., 2., 2., 3., 3., 3., 3., 4., 4., 4., 4.][face] * n - x - y;
    let (nr, z) = if jr < n {
        (jr, 1. - jr * jr / (3. * n * n))
    } else if jr > 3. * n {
        let nr = 4. * n - jr;
        (nr, -1. + nr * nr / (3. * n * n))
    } else {
        (n, (2. * n - jr) * 2. / (3. * n))
    };
    let phi = if nr.abs() < 1e-15 {
        0.
    } else {
        ([1., 3., 5., 7., 0., 2., 4., 6., 1., 3., 5., 7.][face] + (x - y) / nr)
            * std::f64::consts::PI
            / 4.
    };
    [
        phi.to_degrees().rem_euclid(360.),
        z.clamp(-1., 1.).asin().to_degrees(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn healpix_bilinear_preserves_affine_gradient_and_missing_neighbor_coverage() {
        // Design decision: interpolate in longitude/latitude between HEALPix
        // ring centres, matching astropy_healpix rather than a stretched HiPS
        // display tile or interpolation in the nested integer index.
        let point = [100., 10.];
        let neighbours = healpix_bilinear_weights(4, point).unwrap();
        let oracle = BTreeMap::from([
            (92, 0.5327233016389334),
            (87, 0.4261786413111469),
            (93, 0.038814831658257457),
            (94, 0.0022832253916622008),
        ]);
        let mut full = WeightedSample::default();
        let mut missing = WeightedSample::default();
        let mut expected_sum = 0.;
        let mut expected_weight = 0.;
        for (pixel, weight) in neighbours {
            assert!((weight - oracle[&pixel]).abs() < 1e-12);
            let sky = nested_center(4, pixel).unwrap();
            let value = (17. + 0.2 * sky[0] - 0.7 * sky[1]) as f32;
            full.add(value, weight);
            if pixel == 87 {
                missing.add(f32::NAN, weight);
            } else {
                missing.add(value, weight);
                expected_sum += value as f64 * oracle[&pixel];
                expected_weight += oracle[&pixel];
            }
        }
        assert!((full.value() - (17. + 0.2 * point[0] - 0.7 * point[1]) as f32).abs() < 1e-5);
        // Design decision: NaN pixels and omitted/404 tiles contribute neither
        // intensity nor weight. Renormalize finite neighbours; never add black.
        assert!((missing.value() - (expected_sum / expected_weight) as f32).abs() < 1e-5);
        assert!((missing.value() - expected_sum as f32).abs() > 1.);
        let mut empty = WeightedSample::default();
        empty.add(f32::NAN, 1.);
        empty.add(42., 0.);
        assert!(empty.value().is_nan());
        // Independent Astropy weights also cover a wrapped longitude, both
        // polar caps, and the actual native WISE sampling resolution.
        for (nside, point, expected) in [
            (
                512,
                [359.99, 60.],
                [
                    (909279, 0.3189682143816837),
                    (192495, 0.275998403117831),
                    (909301, 0.21709789302026072),
                    (192506, 0.1879354894802246),
                ],
            ),
            (
                4,
                [10., 89.],
                [
                    (15, 0.2808224360413196),
                    (63, 0.26185478309281524),
                    (47, 0.22866139043293257),
                    (31, 0.22866139043293257),
                ],
            ),
            (
                4,
                [190., -90.],
                [(160, 0.25), (144, 0.25), (128, 0.25), (176, 0.25)],
            ),
            (
                131072,
                [10., 89.],
                [
                    (17175457937, 0.6144121513534778),
                    (17175457938, 0.12288243027071018),
                    (17175457940, 0.18973169104918736),
                    (17175457939, 0.07297372732662452),
                ],
            ),
        ] {
            let actual: BTreeMap<_, _> = healpix_bilinear_weights(nside, point)
                .unwrap()
                .into_iter()
                .collect();
            for (pixel, weight) in expected {
                assert!(
                    (actual[&pixel] - weight).abs() < 3e-8,
                    "nside {nside}, {point:?}, pixel {pixel}: {} != {weight}",
                    actual[&pixel]
                );
            }
        }
    }

    #[test]
    fn healpix_nested_centers_round_trip_all_faces_and_poles() {
        for nside in [1u32, 2, 4, 16] {
            for pixel in 0..12 * nside as u64 * nside as u64 {
                assert_eq!(
                    nested_pixel(nside, nested_center(nside, pixel).unwrap()).unwrap(),
                    pixel
                );
            }
        }
        for point in [
            [0., 90.],
            [359.999, -90.],
            [0., 0.],
            [359.99, 89.999],
            [180., -89.99],
        ] {
            assert!(nested_pixel(131072, point).unwrap() < 12 * 131072u64 * 131072);
        }
        // Independent scalar oracle: astropy_healpix, nested ICRS indexing.
        // Avoid exact shared vertices: both adjacent cells own the same sky
        // boundary and different floating-point implementations break ties
        // differently. Off-boundary ownership is unambiguous.
        for (point, small, large) in [
            ([0.001, 0.001], 76, 81604378629),
            ([90.001, 0.001], 92, 98784247813),
            ([359.999, 0.], 73, 78741067094),
            ([10., 20.], 77, 83313045321),
            ([180., -89.99], 160, 171798692512),
            ([359.99, 89.999], 63, 68719476727),
            ([45.0001, 41.8104148957786], 6, 7158278826),
        ] {
            assert_eq!(nested_pixel(4, point).unwrap(), small);
            assert_eq!(nested_pixel(131072, point).unwrap(), large);
        }
    }

    #[test]
    fn moc_intersection_uses_actual_cells_not_their_large_bounding_disks() {
        assert!(cell_intersects_cone(1, 4, [0., 0.], 0.01).unwrap());
        assert!(!cell_intersects_cone(1, 4, [90., 0.], 1.).unwrap());
        assert!(cell_intersects_cone(1, 4, [90., 0.], 46.).unwrap());
        assert!(cell_intersects_cone(1, 0, [225., 89.], 1.01).unwrap());
        assert!(!cell_intersects_cone(1, 0, [225., 89.], 0.5).unwrap());
    }

    #[test]
    fn ps1_science_metadata_requires_real_fits_stack_references() {
        assert_eq!(
            parse_ps1(b"filename filter\n/stack/test.fits g\n").unwrap(),
            vec![("/stack/test.fits".into(), "g".into())]
        );
        assert!(parse_ps1(b"filename filter\npreview.jpg g\n").is_err());
        assert!(parse_ps1(b"<html>not metadata</html>").is_err());
    }
}
