//! DSS cutouts matched at the same sky coordinates, without fitting away nebular structure.
use super::*;
use crate::native::{mosaic_pair_fit, mosaic_samples};

struct Mosaic {
    image: fits::Image,
    weights: Vec<f32>,
    has_data: bool,
    unmatched: u32,
}

impl Mosaic {
    fn new(width: u32, height: u32, header: &Value) -> Result<Self> {
        let image = fits::Image::new(
            width,
            height,
            vec![f32::NAN; width as usize * height as usize],
            header.clone(),
        )?;
        Ok(Self {
            weights: vec![0.; image.pixels.len()],
            image,
            has_data: false,
            unmatched: 0,
        })
    }

    fn add(
        &mut self,
        tile: &fits::Image,
        x: u32,
        y: u32,
        feather: u32,
        job: &JobHandle,
    ) -> Result<()> {
        ensure!(
            x + tile.width <= self.image.width && y + tile.height <= self.image.height,
            "DSS overlap tile exceeds its target grid."
        );
        let mut previous = Vec::with_capacity(tile.pixels.len());
        for row in 0..tile.height {
            check(Some(job))?;
            let start = (y + row) as usize * self.image.width as usize + x as usize;
            previous.extend_from_slice(&self.image.pixels[start..start + tile.width as usize]);
        }
        let cancelled = || job.cancelled();
        let fit = if self.has_data {
            mosaic_pair_fit(
                &mosaic_samples(&previous, tile.width, tile.height, &cancelled)?,
                &mosaic_samples(&tile.pixels, tile.width, tile.height, &cancelled)?,
            )
        } else {
            None
        };
        drop(previous);
        let (gain, offset) = if let Some((gain, offset, _)) = fit {
            (gain, offset)
        } else {
            if self.has_data && tile.pixels.iter().any(|value| value.is_finite()) {
                self.unmatched += 1;
                job.stage(
                    "download",
                    0.,
                    "DSS overlap could not be brightness-matched; feathering only",
                )?;
            }
            (1., 0.)
        };
        let radius = feather.max(1) as f32;
        for row in 0..tile.height {
            if row % 16 == 0 {
                check(Some(job))?;
            }
            for column in 0..tile.width {
                let value = tile.pixels[(row * tile.width + column) as usize];
                if !value.is_finite() {
                    continue;
                }
                let corrected = (value as f64 * gain + offset) as f32;
                ensure!(corrected.is_finite(), "DSS overlap calibration overflowed.");
                let mut weight = 1_f32;
                for distance in [
                    (x > 0).then_some(column + 1),
                    (y > 0).then_some(row + 1),
                    (x + tile.width < self.image.width).then_some(tile.width - column),
                    (y + tile.height < self.image.height).then_some(tile.height - row),
                ]
                .into_iter()
                .flatten()
                {
                    weight = weight.min(distance as f32 / radius);
                }
                let index = ((y + row) * self.image.width + x + column) as usize;
                let total = self.weights[index] + weight;
                let previous = self.image.pixels[index];
                self.image.pixels[index] = if self.weights[index] > 0. {
                    (previous as f64
                        + (corrected as f64 - previous as f64) * (weight / total) as f64)
                        as f32
                } else {
                    corrected
                };
                self.weights[index] = total;
                self.has_data = true;
            }
        }
        Ok(())
    }

    fn finish(mut self, feather: u32) -> Result<fits::Image> {
        ensure!(
            self.has_data,
            "The DSS survey returned no science coverage for this frame."
        );
        self.image.header["AIMOSAIC"] = json!("overlap-v2");
        self.image.header["AIFEATH"] = json!(feather);
        self.image.header["AICALBAD"] = json!(self.unmatched);
        Ok(self.image)
    }
}

impl Archive {
    pub(super) async fn dss_cutout(
        &self,
        band: &Band,
        width: u32,
        height: u32,
        header: &Value,
        grid: &Value,
        job: &JobHandle,
    ) -> Result<fits::Image> {
        let scale = grid["pixel_scale_arcsec"]
            .as_f64()
            .filter(|value| value.is_finite() && *value > 0.)
            .ok_or_else(|| anyhow::anyhow!("Invalid DSS cutout pixel scale."))?;
        let (feather, windows) = windows(width, height, scale)?;
        let mut mosaic = Mosaic::new(width, height, header)?;
        for [x0, y0, w, h] in windows {
            check(Some(job))?;
            let target_header = subheader(header, x0, y0, w, h)?;
            let target = SkyWcs::new(&target_header, w, h)?;
            let (center, radius, _) = window_sky(&target_header, w, h)?;
            let image = self
                .dss_image(
                    band,
                    center,
                    ((radius * 2. + 4.) / 60.).min(60.),
                    Some(job),
                    45,
                )
                .await?;
            let source =
                super::super::wcs::SourceWcs::new(&image.header, image.width, image.height)?;
            let mut tile =
                fits::Image::new(w, h, vec![f32::NAN; w as usize * h as usize], target_header)?;
            for row in 0..h {
                if row % 16 == 0 {
                    check(Some(job))?;
                }
                for column in 0..w {
                    if let Some(p) = target
                        .pixel_to_sky([column as f64, row as f64])
                        .and_then(|p| source.sky_to_pixel(p))
                    {
                        tile.pixels[(row * w + column) as usize] = bilinear(&image, p);
                    }
                }
            }
            mosaic.add(&tile, x0, y0, feather, job)?;
        }
        mosaic.finish(feather)
    }
}

pub(super) fn windows(width: u32, height: u32, scale: f64) -> Result<(u32, Vec<[u32; 4]>)> {
    ensure!(
        scale.is_finite() && scale > 0.,
        "Invalid DSS cutout pixel scale."
    );
    // Keep the expanded diagonal inside STScI's 60-arcminute request limit.
    let side = ((1798. * 2f64.sqrt() / scale).floor() as u32)
        .saturating_sub(4)
        .max(1);
    let feather = 64.min(side / 8);
    let core = side - 2 * feather;
    ensure!(
        width.div_ceil(core) as u64 * height.div_ceil(core) as u64 <= 256,
        "This DSS cutout needs too many tiles. Choose a smaller frame or coarser scale."
    );
    let mut windows = Vec::new();
    for y in (0..height).step_by(core as usize) {
        for x in (0..width).step_by(core as usize) {
            let x0 = x.saturating_sub(feather);
            let y0 = y.saturating_sub(feather);
            windows.push([
                x0,
                y0,
                (x + core + feather).min(width) - x0,
                (y + core + feather).min(height) - y0,
            ]);
        }
    }
    Ok((feather, windows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_dss_tiles_match_brightness_without_flattening_the_sky() {
        let signal = |x: u32, y: u32| (x * 13 + y * 7 + x * y % 17) as f32;
        let tile = |x0: u32, gain: f32, offset: f32| {
            fits::Image::new(
                80,
                64,
                (0..64)
                    .flat_map(|y| (0..80).map(move |x| (signal(x + x0, y) - offset) / gain))
                    .collect(),
                json!({}),
            )
            .unwrap()
        };
        let mut mosaic = Mosaic::new(128, 64, &json!({})).unwrap();
        mosaic
            .add(&tile(0, 1., 0.), 0, 0, 16, &JobHandle::untracked())
            .unwrap();
        mosaic
            .add(&tile(48, 1.8, 700.), 48, 0, 16, &JobHandle::untracked())
            .unwrap();
        let output = mosaic.finish(16).unwrap();
        assert_eq!(output.header["AICALBAD"], 0);
        for y in 0..64 {
            for x in 0..128 {
                assert!((output.pixels[(y * 128 + x) as usize] - signal(x, y)).abs() < 0.002);
            }
        }
    }

    #[test]
    fn unmatched_and_empty_coverage_are_explicit_and_do_not_invent_pixels() {
        let mut mosaic = Mosaic::new(24, 12, &json!({})).unwrap();
        let tile = fits::Image::new(8, 12, vec![100.; 96], json!({})).unwrap();
        mosaic.add(&tile, 0, 0, 2, &JobHandle::untracked()).unwrap();
        mosaic
            .add(&tile, 16, 0, 2, &JobHandle::untracked())
            .unwrap();
        let output = mosaic.finish(2).unwrap();
        assert_eq!(output.header["AICALBAD"], 1);
        for row in output.pixels.as_chunks::<24>().0 {
            assert!(row[8..16].iter().all(|value| value.is_nan()));
            assert!(
                row[..8]
                    .iter()
                    .chain(&row[16..])
                    .all(|value| *value == 100.)
            );
        }
        assert!(Mosaic::new(24, 12, &json!({})).unwrap().finish(2).is_err());
    }
}
