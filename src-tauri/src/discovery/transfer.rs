//! Remaining science-response payloads, not the reprojected output file size.
use super::*;

fn fits_size(width: u64, height: u64, stride: u64, overhead: u64) -> u64 {
    overhead + (width * height * stride).div_ceil(2880) * 2880
}

#[derive(serde::Serialize, serde::Deserialize)]
struct DssProfile {
    scale_arcsec: [f64; 2],
    bytes_per_pixel: u64,
    overhead_bytes: u64,
}

impl Archive {
    pub async fn survey_transfer(&self, observation: &Value, grid: &Value) -> Result<Value> {
        if self.cached_survey(observation, grid)?.is_some() {
            return Ok(
                json!({"transfer_bytes":0,"transfer_estimated":false,"transfer_note":"The complete source is cached."}),
            );
        }
        let band = registry()
            .into_iter()
            .find(|band| Some(band.id.as_str()) == survey_id(observation))
            .ok_or_else(|| anyhow::anyhow!("Unknown FITS survey band."))?;
        let provider = band.sources(observation)?[0];
        let (width, height, header) = grid_header(grid)?;
        let scale = grid["pixel_scale_arcsec"]
            .as_f64()
            .ok_or_else(|| anyhow::anyhow!("Missing survey scale."))?;
        let (bytes, note) = match provider {
            "stsci-dss-v1" => {
                let (center, _, _) = window_sky(&header, width, height)?;
                let profile = self.dss_transfer_profile(&band, center).await?;
                let (_, windows) = dss::windows(width, height, scale)?;
                let count = windows.len();
                let mut bytes = 0;
                for [x, y, w, h] in windows {
                    let sub = subheader(&header, x, y, w, h)?;
                    let (_, radius, _) = window_sky(&sub, w, h)?;
                    let side_arcsec = (radius * 2. + 4.).min(3600.);
                    let [w, h] = profile
                        .scale_arcsec
                        .map(|scale| (side_arcsec / scale).ceil() as u64);
                    bytes += fits_size(w, h, profile.bytes_per_pixel, profile.overhead_bytes);
                }
                (
                    bytes,
                    format!(
                        "{count} overlapping native DSS2 squares, about {:.2} × {:.2} arcseconds/pixel and {}-bit FITS from this field's source headers. Sampling and auxiliary FITS data can vary between plates.",
                        profile.scale_arcsec[0],
                        profile.scale_arcsec[1],
                        profile.bytes_per_pixel * 8
                    ),
                )
            }
            "stsci-ps1-v1" => {
                let side = ((498. * 2f64.sqrt() / scale).floor() as u32)
                    .saturating_sub(4)
                    .max(1);
                ensure!(
                    width.div_ceil(side) as u64 * height.div_ceil(side) as u64 <= 256,
                    "This native survey cutout needs too many tiles."
                );
                let mut bytes = 0;
                let mut count = 0;
                for y in (0..height).step_by(side as usize) {
                    for x in (0..width).step_by(side as usize) {
                        let (w, h) = (side.min(width - x), side.min(height - y));
                        let sub = subheader(&header, x, y, w, h)?;
                        let (_, radius, points) = window_sky(&sub, w, h)?;
                        let files = self.ps1_files(&points, band.band, None).await?;
                        ensure!(
                            files.len() <= 16,
                            "Pan-STARRS returned too many overlapping stacks."
                        );
                        let size = (((radius * 2.) / 0.25).ceil() as u32 + 4).min(4000) as u64;
                        count += files.len();
                        bytes += files.len() as u64 * fits_size(size, size, 4, 2880);
                    }
                }
                ensure!(count > 0, "Pan-STARRS has no source stacks in this frame.");
                (
                    bytes,
                    format!(
                        "{count} native 0.25-arcsecond Pan-STARRS cutouts, including overlapping stacks. FITS headers may vary."
                    ),
                )
            }
            "irsa-hips-v1" => {
                self.irsa_transfer(&band, width, height, &header, scale)
                    .await?
            }
            "mdw-v1" => return self.mdw_transfer(width, height, &header).await,
            "cds" => {
                let mut bytes = 0;
                for y in (0..height).step_by(4000) {
                    for x in (0..width).step_by(4000) {
                        bytes += fits_size(
                            4000.min(width - x) as u64,
                            4000.min(height - y) as u64,
                            4,
                            2880,
                        );
                    }
                }
                (
                    bytes,
                    "CDS on-grid float32 FITS responses; headers may vary.".into(),
                )
            }
            _ => bail!("Unknown survey transfer provider."),
        };
        Ok(json!({"transfer_bytes":bytes,"transfer_estimated":true,
            "transfer_note":format!("{note} Retries or fallback sources can change the transfer.")}))
    }

    pub(super) fn irsa_tile(&self, path: &str, order: u8, tile: u64) -> (String, PathBuf) {
        let url = format!(
            "{}/{path}/Norder{order}/Dir{}/Npix{tile}.fits",
            self.endpoints.irsa,
            tile / 10000 * 10000
        );
        let cache = self
            .root
            .join("cache/survey-tiles")
            .join(format!("{}.fits", hash(&url)));
        (url, cache)
    }

    async fn dss_transfer_profile(&self, band: &Band, center: [f64; 2]) -> Result<DssProfile> {
        let key = hash(serde_json::to_vec(&json!([
            self.endpoints.dss,
            band.id,
            center
        ]))?);
        let cache = self
            .root
            .join("cache/survey-transfer")
            .join(format!("dss-v1-{key}.json"));
        let cached = fresh(&cache, 86400);
        let profile: DssProfile = if cached {
            serde_json::from_slice(&std::fs::read(&cache)?)?
        } else {
            let bytes = self
                .dss_bytes(band, center, 0.1, None, 8, JSON_LIMIT)
                .await?;
            let image = fits::Image::decode(&bytes)?;
            let source =
                super::super::wcs::SourceWcs::new(&image.header, image.width, image.height)?;
            let pixel = [
                (image.width as f64 - 1.) / 2.,
                (image.height as f64 - 1.) / 2.,
            ];
            let sky = source
                .pixel_to_sky(pixel)
                .ok_or_else(|| anyhow::anyhow!("Invalid DSS sampling astrometry."))?;
            let mut scale_arcsec = [0.; 2];
            for axis in 0..2 {
                let mut adjacent = pixel;
                adjacent[axis] += 1.;
                let adjacent = source
                    .pixel_to_sky(adjacent)
                    .ok_or_else(|| anyhow::anyhow!("Invalid DSS sampling astrometry."))?;
                scale_arcsec[axis] = separation(sky, adjacent) * 3600.;
            }
            let stride = image.header["BITPIX"]
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("Missing DSS pixel format."))?
                .unsigned_abs()
                / 8;
            let image_bytes =
                (image.width as u64 * image.height as u64 * stride).div_ceil(2880) * 2880;
            DssProfile {
                scale_arcsec,
                bytes_per_pixel: stride,
                overhead_bytes: (bytes.len() as u64)
                    .checked_sub(image_bytes)
                    .ok_or_else(|| anyhow::anyhow!("Invalid DSS payload size."))?,
            }
        };
        ensure!(
            profile
                .scale_arcsec
                .iter()
                .all(|scale| scale.is_finite() && *scale > 0.)
                && matches!(profile.bytes_per_pixel, 1 | 2 | 4 | 8)
                && profile.overhead_bytes >= 2880,
            "Invalid DSS transfer profile."
        );
        if !cached {
            cache_bytes(&cache, &serde_json::to_vec(&profile)?, None)?;
        }
        Ok(profile)
    }

    async fn irsa_transfer(
        &self,
        band: &Band,
        width: u32,
        height: u32,
        header: &Value,
        scale: f64,
    ) -> Result<(u64, String)> {
        let path = band
            .irsa_path()
            .ok_or_else(|| anyhow::anyhow!("Missing IRSA survey."))?;
        let order = irsa_order(band, scale)?;
        let band = band.clone();
        let header = header.clone();
        let tiles = tokio::task::spawn_blocking(move || {
            irsa_transfer_tiles(&band, width, height, &header, scale)
        })
        .await??;
        let missing: Vec<_> = tiles
            .iter()
            .map(|tile| self.irsa_tile(&path, order, *tile))
            .filter(|(_, cache)| !cache.is_file())
            .collect();
        if missing.is_empty() {
            return Ok((
                0,
                format!(
                    "All {} required IRSA tiles are cached; no science transfer expected.",
                    tiles.len()
                ),
            ));
        }
        let profile = self.root.join("cache/survey-transfer").join(format!(
            "{}.json",
            hash(format!("{}/{path}/Norder{order}", self.endpoints.irsa))
        ));
        let size = if fresh(&profile, 86400) {
            let profile: Value = serde_json::from_slice(&std::fs::read(&profile)?)?;
            profile["bytes"]
                .as_u64()
                .filter(|bytes| *bytes > 0)
                .ok_or_else(|| anyhow::anyhow!("Invalid cached IRSA transfer sample."))?
        } else {
            let mut size = None;
            for (url, _) in &missing {
                let _slot = self.slots.acquire().await?;
                let response = self
                    .client
                    .head(url)
                    .header(reqwest::header::ACCEPT_ENCODING, "identity")
                    .timeout(Duration::from_secs(8))
                    .send()
                    .await
                    .map_err(|error| remote_error("IRSA tile size", &error))?;
                if response.status() == reqwest::StatusCode::NOT_FOUND {
                    continue;
                }
                ensure!(
                    response.status().is_success(),
                    "IRSA tile size is unavailable (HTTP {}).",
                    response.status()
                );
                size = response
                    .headers()
                    .get(reqwest::header::CONTENT_LENGTH)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|bytes| *bytes > 0);
                ensure!(size.is_some(), "IRSA did not report a tile payload size.");
                break;
            }
            let size = size.ok_or_else(|| anyhow::anyhow!("IRSA has no tiles in this frame."))?;
            cache_bytes(&profile, &serde_json::to_vec(&json!({"bytes":size}))?, None)?;
            size
        };
        Ok((
            missing.len() as u64 * size,
            format!(
                "About {} missing IRSA tiles at order {order}; {} cached. Based on a sampled tile's HTTP payload size; compression and coverage vary.",
                missing.len(),
                tiles.len() - missing.len()
            ),
        ))
    }
}

fn irsa_transfer_tiles(
    band: &Band,
    width: u32,
    height: u32,
    header: &Value,
    scale: f64,
) -> Result<BTreeSet<u64>> {
    let order = irsa_order(band, scale)?;
    let nside = 512u32 << order;
    let target = SkyWcs::new(header, width, height)?;
    let source_scale = (std::f64::consts::PI / 3.).sqrt().to_degrees() * 3600. / nside as f64;
    // 32 samples across each 512-pixel tile, plus every boundary pixel, keep
    // planning cheap without overlooking thin intersections at the frame edges.
    let step = (16. * source_scale / scale).floor().max(1.) as usize;
    let mut tiles = BTreeSet::new();
    let mut add = |x: u32, y: u32| -> Result<()> {
        if let Some(mut sky) = target.pixel_to_sky([x as f64, y as f64]) {
            if band.galactic {
                sky = icrs_to_galactic(sky);
            }
            for (pixel, weight) in healpix_bilinear_weights(nside, sky)? {
                if weight > 0. {
                    tiles.insert(pixel / (512 * 512));
                }
            }
        }
        Ok(())
    };
    for y in (0..height).step_by(step) {
        for x in (0..width).step_by(step) {
            add(x, y)?;
        }
    }
    for x in 0..width {
        add(x, 0)?;
        add(x, height - 1)?;
    }
    for y in 0..height {
        add(0, y)?;
        add(width - 1, y)?;
    }
    ensure!(
        tiles.len() <= 4096,
        "IRSA cutout exceeds the safe tile limit."
    );
    Ok(tiles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_sampling_matches_downloader_tiles_on_rotated_and_polar_frames() {
        for (ra, dec, width, rotation) in [
            (10., 20., 80., 37.),
            (359.9, 0., 180., 72.),
            (0., 89.5, 180., 37.),
            (274.7, -13.8, 140., 123.),
            (192.85948, 27.12825, 180., 80.),
        ] {
            for band in registry()
                .into_iter()
                .filter(|band| band.id == "CDS/P/allWISE/W1" || band.id == "CDS/P/SPITZER/IRAC1")
            {
                let capture = crate::sky::coordinate_capture(&json!({
                    "ra":ra,"dec":dec,"width_arcmin":width,"aspect":"3:2","rotation_deg":rotation
                }))
                .unwrap();
                let grid = crate::grid::view_grid(&capture.view, band.native_scale(), 512).unwrap();
                let (w, h, header) = grid_header(&grid).unwrap();
                let scale = grid["pixel_scale_arcsec"].as_f64().unwrap();
                let estimated = irsa_transfer_tiles(&band, w, h, &header, scale).unwrap();
                let target = SkyWcs::new(&header, w, h).unwrap();
                let nside = 512u32 << irsa_order(&band, scale).unwrap();
                let mut actual = BTreeSet::new();
                for y in 0..h {
                    for x in 0..w {
                        let mut sky = target.pixel_to_sky([x as f64, y as f64]).unwrap();
                        if band.galactic {
                            sky = icrs_to_galactic(sky);
                        }
                        for (pixel, weight) in healpix_bilinear_weights(nside, sky).unwrap() {
                            if weight > 0. {
                                actual.insert(pixel / (512 * 512));
                            }
                        }
                    }
                }
                assert_eq!(
                    estimated, actual,
                    "{} at {ra},{dec}, rotation {rotation}",
                    band.id
                );
            }
        }
    }
}
