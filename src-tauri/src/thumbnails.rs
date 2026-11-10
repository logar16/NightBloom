use crate::{planes::project_file, projects};
use anyhow::{Context, Result};
use image::{DynamicImage, ImageReader, Rgb, RgbImage, codecs::jpeg::JpegEncoder};
use serde_json::{Value, json};
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

fn exports(session: &Path) -> Result<Vec<(SystemTime, PathBuf)>> {
    let mut pending = vec![project_file(session, "exports")?];
    let mut sources = Vec::new();
    while let Some(folder) = pending.pop() {
        let entries = match fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("Reading thumbnail exports"),
        };
        for entry in entries {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file()
                && entry
                    .path()
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| {
                        ["png", "jpg", "jpeg", "tif", "tiff"]
                            .contains(&s.to_ascii_lowercase().as_str())
                    })
            {
                match entry.metadata() {
                    Ok(metadata) => sources.push((metadata.modified()?, entry.path())),
                    Err(error) if error.kind() == ErrorKind::NotFound => continue,
                    Err(error) => return Err(error).context("Reading thumbnail source metadata"),
                }
            }
        }
    }
    sources.sort_unstable_by(|a, b| b.cmp(a));
    Ok(sources)
}

fn cached(destination: &Path, saved: &Value, signature: &Value) -> Result<Option<Vec<u8>>> {
    if saved != signature {
        return Ok(None);
    }
    match fs::read(destination) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("Reading cached thumbnail"),
    }
}

fn save(session: &Path, image: DynamicImage, signature: &Value) -> Result<Vec<u8>> {
    let image = image.thumbnail(480, 480).to_rgb8();
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, 85).encode_image(&image)?;
    projects::write_bytes(&project_file(session, "thumb.jpg")?, &bytes)?;
    projects::write_json(&project_file(session, "thumb.source.json")?, signature)?;
    Ok(bytes)
}

pub(crate) fn thumbnail(session: &Path) -> Result<Option<Vec<u8>>> {
    let destination = project_file(session, "thumb.jpg")?;
    let marker = project_file(session, "thumb.source.json")?;
    let saved = projects::read_json(&marker, Value::Null)?;
    let mut sources: Vec<_> = exports(session)?
        .into_iter()
        .map(|(_, path)| path)
        .collect();
    sources.push(project_file(session, "snapshot.jpg")?);
    for source in sources {
        let metadata = match fs::metadata(&source) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("Reading thumbnail source"),
        };
        let signature = json!({
            "source":source.strip_prefix(session)?,
            "mtime_ns":u64::try_from(metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos())?,
            "size":metadata.len(),
        });
        if let Some(bytes) = cached(&destination, &saved, &signature)? {
            return Ok(Some(bytes));
        }
        let image = match ImageReader::open(&source)
            .map_err(image::ImageError::IoError)
            .and_then(ImageReader::decode)
        {
            Ok(image) => image,
            Err(error) => {
                // Exports can still be in progress; try older artwork or the captured sky.
                eprintln!(
                    "NightBloom thumbnail: skipping {}: {error}",
                    source.display()
                );
                continue;
            }
        };
        return save(session, image, &signature).map(Some);
    }
    let view = projects::read_json(&project_file(session, "view.json")?, Value::Null)?;
    if view["source"] != "coordinates" {
        return Ok(None);
    }
    let signature = json!({
        "coordinates":view["center_icrs_deg"],"field":view["field_arcmin"],
        "rotation":view.get("rotation_deg").cloned().unwrap_or_else(|| json!(0)),
    });
    if let Some(bytes) = cached(&destination, &saved, &signature)? {
        return Ok(Some(bytes));
    }
    let width = view["pixel_dimensions"][0]
        .as_f64()
        .filter(|n| *n > 0.)
        .context("Invalid coordinate thumbnail width")?;
    let height = view["pixel_dimensions"][1]
        .as_f64()
        .filter(|n| *n > 0.)
        .context("Invalid coordinate thumbnail height")?;
    let scale = 480. / width.max(height);
    let (width, height) = (
        (width * scale).round().max(1.) as u32,
        (height * scale).round().max(1.) as u32,
    );
    let mut image = RgbImage::from_pixel(width, height, Rgb([23, 27, 32]));
    for i in 1..8 {
        let x = (width as f64 * i as f64 / 8.).round() as u32;
        let y = (height as f64 * i as f64 / 8.).round() as u32;
        for row in 0..height {
            image.put_pixel(x.min(width - 1), row, Rgb([42, 52, 62]));
        }
        for column in 0..width {
            image.put_pixel(column, y.min(height - 1), Rgb([42, 52, 62]));
        }
    }
    for x in (width / 2).saturating_sub(9)..=(width / 2 + 9).min(width - 1) {
        image.put_pixel(x, height / 2, Rgb([95, 201, 214]));
    }
    for y in (height / 2).saturating_sub(9)..=(height / 2 + 9).min(height - 1) {
        image.put_pixel(width / 2, y, Rgb([95, 201, 214]));
    }
    save(session, DynamicImage::ImageRgb8(image), &signature).map(Some)
}
