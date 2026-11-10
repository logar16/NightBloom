use crate::projects::{self, Projects};
use anyhow::{Context, Result, bail};
use image::{
    ImageEncoder, ImageReader,
    codecs::{jpeg::JpegEncoder, png::PngEncoder},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

pub fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            ["png", "jpg", "jpeg", "tif", "tiff"].contains(&ext.to_ascii_lowercase().as_str())
        })
}

fn files(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut pending = vec![directory.to_path_buf()];
    let mut images = Vec::new();
    while let Some(folder) = pending.pop() {
        for entry in fs::read_dir(folder)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                if !entry.path().canonicalize()?.starts_with(directory) {
                    bail!("Unsafe export subfolder");
                }
                pending.push(entry.path());
            } else if kind.is_file() && is_image(&entry.path()) {
                images.push(entry.path());
            }
        }
    }
    Ok(images)
}

pub fn inventory(projects: &Projects, data: &Path) -> Result<Value> {
    let mut albums = Vec::new();
    for entry in fs::read_dir(data.join("sessions"))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let identifier = entry.file_name().to_string_lossy().into_owned();
        let Ok(project) = projects.resolve_readonly(&identifier) else {
            continue;
        };
        let directory = project.join("exports");
        if !directory.exists() {
            continue;
        }
        let directory = directory.canonicalize()?;
        if directory.parent() != Some(project.as_path()) {
            bail!("Unsafe exports folder");
        }
        let (history, history_error) = match crate::export_history::records(&project) {
            Ok(value) => (value, None),
            Err(error) => {
                eprintln!(
                    "NightBloom gallery {}: saved edit history is unavailable: {error:#}",
                    project.display()
                );
                (json!({}), Some("Saved edit history is unavailable."))
            }
        };
        let mut images = Vec::new();
        for source in files(&directory)? {
            let filename = source
                .strip_prefix(&directory)?
                .to_string_lossy()
                .replace('\\', "/");
            let source = projects::export_file(&project, &filename)?;
            let metadata = source.metadata()?;
            let modified = metadata.modified()?.duration_since(UNIX_EPOCH)?;
            let (width, height, error) = match ImageReader::open(&source)?.into_dimensions() {
                Ok((width, height)) => (Some(width), Some(height), None),
                Err(error) => {
                    let error = format!("Cannot read image: {error}");
                    eprintln!("NightBloom gallery {}: {error}", source.display());
                    (None, None, Some(error))
                }
            };
            let checkpoint = history[&filename]["checkpoint"].as_str().filter(|id| {
                id.len() == 64
                    && id
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                    && project
                        .join("export-checkpoints")
                        .join(format!("{id}.json"))
                        .is_file()
            });
            images.push(json!({"filename":filename,"width":width,"height":height,
                "size_bytes":metadata.len(),"modified":modified.as_secs_f64(),
                "version":format!("{}-{}",modified.as_nanos(),metadata.len()),"error":error,
                "checkpoint":checkpoint,"checkpoint_error":history_error}));
        }
        if images.is_empty() {
            continue;
        }
        images.sort_by(|a, b| {
            let pixels = |image: &Value| {
                image["width"].as_u64().unwrap_or(0) * image["height"].as_u64().unwrap_or(0)
            };
            pixels(b)
                .cmp(&pixels(a))
                .then_with(|| {
                    b["modified"]
                        .as_f64()
                        .partial_cmp(&a["modified"].as_f64())
                        .unwrap()
                })
                .then_with(|| a["filename"].as_str().cmp(&b["filename"].as_str()))
        });
        let saved = projects::read_json(&project.join("session.json"), json!({}))?;
        let updated = images
            .iter()
            .filter_map(|image| image["modified"].as_f64())
            .fold(0., f64::max);
        albums.push(
            json!({"id":identifier,"name":saved["name"].as_str().unwrap_or(&identifier),
            "updated":updated,"images":images}),
        );
    }
    albums.sort_by(|a, b| {
        b["updated"]
            .as_f64()
            .partial_cmp(&a["updated"].as_f64())
            .unwrap()
            .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
    });
    Ok(json!(albums))
}

pub fn preview(source: &Path, cache: &Path, view: &str) -> Result<Vec<u8>> {
    if !["thumbnail", "preview", "clipboard"].contains(&view) {
        bail!("Unknown gallery image view");
    }
    let metadata = source.metadata()?;
    let signature = format!(
        "v1:{}:{}:{}:{view}",
        source.display(),
        metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos(),
        metadata.len()
    );
    let key = format!("{:x}", Sha256::digest(signature.as_bytes()));
    let destination = cache.join(format!("{key}.jpg"));
    if view != "clipboard" && destination.is_file() {
        return Ok(fs::read(destination)?);
    }
    let image = ImageReader::open(source)?
        .decode()
        .context("Decoding exported image")?;
    let image = if view == "clipboard" {
        image
    } else {
        let side = if view == "thumbnail" { 480 } else { 2560 };
        if image.width() > side || image.height() > side {
            image.thumbnail(side, side)
        } else {
            image
        }
    };
    let image = image.to_rgb8();
    let mut bytes = Vec::new();
    if view == "clipboard" {
        PngEncoder::new(&mut bytes).write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgb8,
        )?;
        if let Some(link) = crate::png_links::read(source)? {
            bytes.splice(33..33, crate::png_links::chunk(&link)?);
        }
    } else {
        JpegEncoder::new_with_quality(&mut bytes, 88).encode_image(&image)?;
        projects::write_bytes(&destination, &bytes)?;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};
    use std::{fs::FileTimes, time::Duration};

    fn setup(root: &Path) -> Projects {
        for identifier in ["alpha", "beta", "empty"] {
            let path = root.join("sessions").join(identifier);
            fs::create_dir_all(path.join("exports")).unwrap();
            projects::write_json(&path.join("catalog.json"), &json!({"groups":[]})).unwrap();
            projects::write_json(&path.join("session.json"), &json!({"name":identifier})).unwrap();
            projects::write_json(&path.join("edits.json"), &json!({"keep":identifier})).unwrap();
        }
        Projects::new(root.to_owned()).unwrap()
    }

    fn picture(path: &Path, width: u32, height: u32, stamp: u64) {
        RgbImage::from_pixel(width, height, Rgb([0, 128, 255]))
            .save(path)
            .unwrap();
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(stamp)))
            .unwrap();
    }

    #[test]
    fn albums_use_actual_export_dimensions_hires_then_newest_without_project_activation() {
        let temporary = tempfile::tempdir().unwrap();
        let mut projects = setup(temporary.path());
        projects.open("beta").unwrap();
        let root = projects.resolve_readonly("alpha").unwrap();
        let directory = root.join("exports");
        picture(&directory.join("large.png"), 80, 60, 100);
        picture(&directory.join("latest.png"), 80, 60, 200);
        picture(&directory.join("preview.jpg"), 40, 30, 400);
        fs::create_dir(directory.join("old")).unwrap();
        picture(&directory.join("old/art.tiff"), 64, 48, 300);
        fs::write(directory.join("bad.png"), b"unfinished image").unwrap();
        fs::write(directory.join("sidecar.json"), b"{}").unwrap();
        let pointer = fs::read(temporary.path().join("sessions/current.json")).unwrap();
        let edits = fs::read(root.join("edits.json")).unwrap();
        let result = inventory(&projects, temporary.path()).unwrap();
        let albums = result.as_array().unwrap();
        assert_eq!(albums.len(), 1);
        let images = albums[0]["images"].as_array().unwrap();
        assert_eq!(
            images
                .iter()
                .map(|image| image["filename"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "latest.png",
                "large.png",
                "old/art.tiff",
                "preview.jpg",
                "bad.png"
            ]
        );
        assert!(
            images[4]["error"]
                .as_str()
                .unwrap()
                .contains("Cannot read image")
        );
        assert_eq!(fs::read(root.join("edits.json")).unwrap(), edits);
        assert_eq!(
            fs::read(temporary.path().join("sessions/current.json")).unwrap(),
            pointer
        );
    }

    #[test]
    fn tiff_clipboard_is_full_size_png_preview_cache_refreshes_and_paths_stay_inside_exports() {
        let temporary = tempfile::tempdir().unwrap();
        let projects = setup(temporary.path());
        let project = projects.resolve_readonly("alpha").unwrap();
        let source = project.join("exports/colour.tiff");
        image::ImageBuffer::from_pixel(64, 48, Rgb([0u16, 32768, 65535]))
            .save(&source)
            .unwrap();
        let original = fs::read(&source).unwrap();
        let cache = temporary.path().join("cache/gallery");
        for view in ["thumbnail", "preview", "clipboard"] {
            let bytes = preview(&source, &cache, view).unwrap();
            let decoded = image::load_from_memory(&bytes).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (64, 48));
            if view == "clipboard" {
                assert_eq!(
                    image::guess_format(&bytes).unwrap(),
                    image::ImageFormat::Png
                );
                assert_eq!(decoded.to_rgb8().get_pixel(0, 0).0, [0, 128, 255]);
            }
        }
        assert_eq!(fs::read(&source).unwrap(), original);
        let first = preview(&source, &cache, "preview").unwrap();
        RgbImage::from_pixel(64, 48, Rgb([255, 0, 0]))
            .save(&source)
            .unwrap();
        assert_ne!(preview(&source, &cache, "preview").unwrap(), first);
        for filename in [
            "../catalog.json",
            "/etc/file.png",
            "C:other.png",
            "old/../../file.png",
            "old\\file.png",
        ] {
            assert!(projects::export_file(&project, filename).is_err());
        }
        assert!(preview(&source, &cache, "bogus").is_err());
    }
}
