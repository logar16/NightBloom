//! Display-only Pan-STARRS mosaics. These bytes never enter the science cache.
use super::*;
use image::GrayImage;
use serde::Deserialize;

#[derive(Deserialize)]
struct StackWcs {
    imsize: [u32; 2],
    crpix: [f64; 2],
    crval: [f64; 2],
    cdmatrix: [f64; 4],
    proj: String,
    refframe: String,
}

impl StackWcs {
    fn sky(&self) -> Result<SkyWcs> {
        ensure!(
            self.proj == "TAN",
            "Unsupported Pan-STARRS preview projection."
        );
        SkyWcs::new(
            &json!({
                "CTYPE1":"RA---TAN","CTYPE2":"DEC--TAN",
                "CRPIX1":self.crpix[0],"CRPIX2":self.crpix[1],
                "CRVAL1":self.crval[0],"CRVAL2":self.crval[1],
                "CD1_1":self.cdmatrix[0],"CD1_2":self.cdmatrix[1],
                "CD2_1":self.cdmatrix[2],"CD2_2":self.cdmatrix[3],
                "RADESYS":self.refframe,
            }),
            self.imsize[0],
            self.imsize[1],
        )
    }
}

struct DisplayTile {
    wcs: SkyWcs,
    origin: [u32; 2],
    extent: [u32; 2],
    image: GrayImage,
}

impl Archive {
    pub async fn panstarrs_preview(&self, observation: &Value, grid: &Value) -> Result<Vec<u8>> {
        let filter = text(observation, "hips_id")
            .strip_prefix("CDS/P/PanSTARRS/DR1/")
            .ok_or_else(|| anyhow::anyhow!("Unknown Pan-STARRS preview band."))?;
        ensure!(
            matches!(filter, "g" | "r" | "i" | "z"),
            "Unknown Pan-STARRS preview filter."
        );
        let (width, height, header) = grid_header(grid)?;
        ensure!(width.max(height) <= 1400, "Preview exceeds 1400 pixels.");
        let cache = self.root.join("cache/previews").join(format!(
            "ps1-display-v2-{}.jpg",
            hash(serde_json::to_vec(&json!([filter, grid]))?)
        ));
        if cache.is_file() {
            return Ok(std::fs::read(cache)?);
        }
        let target = SkyWcs::new(&header, width, height)?;
        let points = sample_wcs(&target, width, height);
        ensure!(
            !points.is_empty(),
            "Preview frame has no valid sky coordinates."
        );
        let mut files = self.ps1_files(&points[..1], filter, None).await?;
        if files.is_empty() {
            files = self.ps1_files(&points, filter, None).await?;
        }
        let first = files
            .first()
            .ok_or_else(|| anyhow::anyhow!("Pan-STARRS has no preview coverage for this frame."))?;
        let first_metadata = self.preview_stack_wcs(&first.0).await?;
        let target_scale = grid["pixel_scale_arcsec"]
            .as_f64()
            .filter(|value| value.is_finite() && *value > 0.)
            .ok_or_else(|| anyhow::anyhow!("Invalid preview pixel scale."))?;
        let lookups = lookup_positions(&target, width, height, target_scale, &first_metadata)?;
        let positions = lookups
            .iter()
            .map(|point| format!("{} {}", point[0], point[1]))
            .collect::<Vec<_>>()
            .join("\n");
        let form = reqwest::multipart::Form::new()
            .text("filters", filter.to_owned())
            .text("type", "stack")
            .part(
                "file",
                reqwest::multipart::Part::text(positions).file_name("positions.txt"),
            );
        let bytes = self
            .http_bytes(
                self.client
                    .post(format!("{}/ps1filenames.py", self.endpoints.ps1))
                    .multipart(form)
                    .timeout(Duration::from_secs(8)),
                JSON_LIMIT,
                None,
                "Pan-STARRS full-frame stack lookup",
            )
            .await?;
        let first_file = first.0.clone();
        let mut first_metadata = Some(first_metadata);
        files.extend(parse_ps1(&bytes)?);
        ensure!(
            files.iter().all(|(_, band)| band == filter),
            "Pan-STARRS returned the wrong preview filter."
        );
        let files: BTreeSet<_> = files.into_iter().map(|(file, _)| file).collect();
        ensure!(
            files.len() <= 128,
            "Pan-STARRS returned too many stacks. Narrow the frame."
        );
        let mut tiles = Vec::new();
        for file in files {
            let endpoint = format!("{}/fitscut.cgi", self.endpoints.ps1);
            let metadata = if file == first_file {
                first_metadata.take().unwrap()
            } else {
                self.preview_stack_wcs(&file).await?
            };
            let source = metadata.sky()?;
            let positions: Vec<_> = points
                .iter()
                .filter_map(|sky| source.sky_to_pixel(*sky))
                .collect();
            let Some((origin, extent)) = display_region(&positions, metadata.imsize) else {
                continue;
            };
            let scale = metadata.cdmatrix[0]
                .hypot(metadata.cdmatrix[2])
                .max(metadata.cdmatrix[1].hypot(metadata.cdmatrix[3]))
                * 3600.;
            let side = ((extent[0].max(extent[1]) as f64 * scale / target_scale).ceil() as u32)
                .clamp(1, 1400);
            let params = [
                ("red", file),
                ("format", "png".into()),
                ("corner", "yes".into()),
                ("x", origin[0].to_string()),
                ("y", origin[1].to_string()),
                (
                    "size",
                    if origin == [0, 0] && extent == metadata.imsize {
                        "ALL".into()
                    } else {
                        format!("{},{}", extent[0], extent[1])
                    },
                ),
                ("output_size", side.to_string()),
            ];
            let bytes = self
                .http_bytes(
                    self.client
                        .get(&endpoint)
                        .query(&params)
                        .timeout(Duration::from_secs(12)),
                    JSON_LIMIT,
                    None,
                    "Pan-STARRS display PNG",
                )
                .await?;
            let image =
                image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)?.to_luma8();
            ensure!(
                image.width().max(image.height()) <= 1400,
                "Pan-STARRS ignored the preview size limit."
            );
            tiles.push(DisplayTile {
                wcs: source,
                origin,
                extent,
                image,
            });
        }
        let bytes =
            tokio::task::spawn_blocking(move || render_preview(&target, width, height, &tiles))
                .await??;
        cache_bytes(&cache, &bytes, None)?;
        Ok(bytes)
    }

    async fn preview_stack_wcs(&self, file: &str) -> Result<StackWcs> {
        let bytes = self
            .http_bytes(
                self.client
                    .get(format!("{}/fitscut.cgi", self.endpoints.ps1))
                    .query(&[("red", file), ("getWCS", "yes")])
                    .timeout(Duration::from_secs(8)),
                JSON_LIMIT,
                None,
                "Pan-STARRS preview astrometry",
            )
            .await?;
        let metadata: StackWcs = serde_json::from_slice(&bytes)?;
        metadata.sky()?;
        Ok(metadata)
    }
}

fn lookup_positions(
    target: &SkyWcs,
    width: u32,
    height: u32,
    target_scale: f64,
    source: &StackWcs,
) -> Result<Vec<[f64; 2]>> {
    let angular_width = source.imsize[0] as f64 * source.cdmatrix[0].hypot(source.cdmatrix[2]);
    let angular_height = source.imsize[1] as f64 * source.cdmatrix[1].hypot(source.cdmatrix[3]);
    // Nine frame samples miss intervening skycells. Include edges and sample more
    // finely than half a stack's smaller axis, regardless of display resolution.
    let step = angular_width.min(angular_height) * 3600. / (2f64.sqrt() * 2. * target_scale);
    let columns = (width as f64 / step).ceil().max(1.);
    let rows = (height as f64 / step).ceil().max(1.);
    ensure!(
        (columns + 1.) * (rows + 1.) <= 1024.,
        "Pan-STARRS preview needs too many stack positions. Narrow the frame."
    );
    let (columns, rows) = (columns as u32, rows as u32);
    let mut points = Vec::new();
    for y in 0..=rows {
        for x in 0..=columns {
            if let Some(sky) = target.pixel_to_sky([
                x as f64 * (width - 1) as f64 / columns as f64,
                y as f64 * (height - 1) as f64 / rows as f64,
            ]) {
                points.push(sky);
            }
        }
    }
    Ok(points)
}

fn display_region(points: &[[f64; 2]], dimensions: [u32; 2]) -> Option<([u32; 2], [u32; 2])> {
    if points.is_empty() {
        return None;
    }
    let mut origin = [0; 2];
    let mut extent = [0; 2];
    for axis in 0..2 {
        let lower = points.iter().map(|p| p[axis]).fold(f64::INFINITY, f64::min);
        let upper = points
            .iter()
            .map(|p| p[axis])
            .fold(f64::NEG_INFINITY, f64::max);
        let begin = lower.floor().clamp(0., dimensions[axis] as f64) as u32;
        let end = (upper.ceil() + 1.).clamp(0., dimensions[axis] as f64) as u32;
        if end <= begin {
            return None;
        }
        origin[axis] = begin;
        extent[axis] = end - begin;
    }
    Some((origin, extent))
}

fn render_preview(
    target: &SkyWcs,
    width: u32,
    height: u32,
    tiles: &[DisplayTile],
) -> Result<Vec<u8>> {
    let mut image = GrayImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let Some(sky) = target.pixel_to_sky([x as f64, (height - 1 - y) as f64]) else {
                continue;
            };
            let mut value = 0.;
            let mut weight = 0.;
            for tile in tiles {
                let Some(position) = tile.wcs.sky_to_pixel(sky) else {
                    continue;
                };
                let native = [
                    position[0] - tile.origin[0] as f64,
                    position[1] - tile.origin[1] as f64,
                ];
                if (0..2).any(|axis| {
                    native[axis] < -0.5 || native[axis] >= tile.extent[axis] as f64 - 0.5
                }) {
                    continue;
                }
                let px =
                    (native[0] + 0.5) * tile.image.width() as f64 / tile.extent[0] as f64 - 0.5;
                let py = tile.image.height() as f64
                    - 0.5
                    - (native[1] + 0.5) * tile.image.height() as f64 / tile.extent[1] as f64;
                let sample = display_sample(&tile.image, px, py);
                // PNGs encode missing padding as black; it must not obscure another stack.
                if sample > 0. {
                    let edge = (px + 0.5)
                        .min(py + 0.5)
                        .min(tile.image.width() as f64 - 0.5 - px)
                        .min(tile.image.height() as f64 - 0.5 - py);
                    value += sample * edge;
                    weight += edge;
                }
            }
            if weight > 0. {
                image.put_pixel(x, y, image::Luma([(value / weight).round() as u8]));
            }
        }
    }
    ensure!(
        image.as_raw().iter().any(|value| *value > 0),
        "Pan-STARRS returned no usable preview pixels."
    );
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85).encode_image(&image)?;
    Ok(bytes)
}

fn display_sample(image: &GrayImage, x: f64, y: f64) -> f64 {
    let x = x.clamp(0., (image.width() - 1) as f64);
    let y = y.clamp(0., (image.height() - 1) as f64);
    let (x0, y0) = (x.floor() as u32, y.floor() as u32);
    let (x1, y1) = (
        (x0 + 1).min(image.width() - 1),
        (y0 + 1).min(image.height() - 1),
    );
    let (dx, dy) = (x - x0 as f64, y - y0 as f64);
    (1. - dy)
        * ((1. - dx) * image.get_pixel(x0, y0).0[0] as f64
            + dx * image.get_pixel(x1, y0).0[0] as f64)
        + dy * ((1. - dx) * image.get_pixel(x0, y1).0[0] as f64
            + dx * image.get_pixel(x1, y1).0[0] as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_frame_lookup_reaches_skycells_between_the_center_and_edges() {
        let target = SkyWcs::new(
            &json!({
                "CTYPE1":"RA---TAN","CTYPE2":"DEC--TAN","RADESYS":"ICRS",
                "CRPIX1":4.5,"CRPIX2":3.,"CRVAL1":10.,"CRVAL2":20.,
                "CD1_1":-30./3600.,"CD1_2":0.,"CD2_1":0.,"CD2_2":30./3600.
            }),
            8,
            5,
        )
        .unwrap();
        let intermediate = StackWcs {
            imsize: [80, 256],
            crpix: [90.5, 128.5],
            crval: [10., 20.],
            cdmatrix: [-1. / 3600., 0., 0., 1. / 3600.],
            proj: "TAN".into(),
            refframe: "ICRS".into(),
        };
        let source = intermediate.sky().unwrap();
        let points = lookup_positions(&target, 8, 5, 30., &intermediate).unwrap();
        assert!(
            points
                .iter()
                .filter_map(|sky| source.sky_to_pixel(*sky))
                .any(|pixel| (0..2).all(|axis| {
                    pixel[axis] >= -0.5 && pixel[axis] < intermediate.imsize[axis] as f64 - 0.5
                }))
        );
    }

    #[test]
    fn cropped_binned_display_preserves_rotated_fits_centers_and_top_down_pixels() {
        let header = json!({
            "CTYPE1":"RA---TAN","CTYPE2":"DEC--TAN","RADESYS":"ICRS",
            "CRPIX1":16.5,"CRPIX2":16.5,"CRVAL1":10.,"CRVAL2":20.,
            "CD1_1":-0.001,"CD1_2":0.,"CD2_1":0.,"CD2_2":0.001
        });
        let mut target_header = header.clone();
        target_header["CRPIX1"] = json!(2.5);
        target_header["CRPIX2"] = json!(2.5);
        target_header["CD1_1"] = json!(0.);
        target_header["CD1_2"] = json!(0.004);
        target_header["CD2_1"] = json!(0.004);
        target_header["CD2_2"] = json!(0.);
        let target = SkyWcs::new(&target_header, 4, 4).unwrap();
        let blank = DisplayTile {
            wcs: SkyWcs::new(&header, 32, 32).unwrap(),
            origin: [8, 8],
            extent: [16, 16],
            image: GrayImage::new(4, 4),
        };
        assert!(render_preview(&target, 4, 4, &[blank]).is_err());
        let blank = DisplayTile {
            wcs: SkyWcs::new(&header, 32, 32).unwrap(),
            origin: [8, 8],
            extent: [16, 16],
            image: GrayImage::new(4, 4),
        };
        let tile = DisplayTile {
            wcs: SkyWcs::new(&header, 32, 32).unwrap(),
            origin: [8, 8],
            extent: [16, 16],
            image: GrayImage::from_fn(4, 4, |x, y| image::Luma([(50 + x * 10 + y * 40) as u8])),
        };
        let bytes = render_preview(&target, 4, 4, &[blank, tile]).unwrap();
        let image = image::load_from_memory(&bytes).unwrap().to_luma8();
        for (x, y, expected) in [(0, 0, 170), (3, 0, 50), (0, 3, 200), (3, 3, 80)] {
            assert!((image.get_pixel(x, y).0[0] as i32 - expected).abs() < 8);
        }
    }
}
