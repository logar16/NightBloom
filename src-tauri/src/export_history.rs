//! Export-time edits are immutable and independent of output size or format.
use crate::projects;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

fn read(path: &Path, fallback: Value) -> Result<Value> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        let linked = metadata.file_type().is_symlink();
        #[cfg(windows)]
        let linked = {
            use std::os::windows::fs::MetadataExt;
            linked || metadata.file_attributes() & 0x400 != 0
        };
        ensure!(!linked, "Linked export history is not supported.");
    }
    projects::read_json(path, fallback)
}

fn validate(state: &Value, meta: &Value) -> Result<()> {
    ensure!(
        state["layers"].is_array() && state["global"].is_object(),
        "Expected complete export edits."
    );
    ensure!(
        meta["width"].as_u64().unwrap_or(0) > 0 && meta["height"].as_u64().unwrap_or(0) > 0,
        "Expected the export's build metadata."
    );
    Ok(())
}

pub fn save_checkpoint(session: &Path, state: &Value, meta: &Value) -> Result<String> {
    validate(state, meta)?;
    let mut snapshot = state.clone();
    snapshot.as_object_mut().unwrap().remove("checkpoints");
    let mut identity = snapshot.clone();
    identity.as_object_mut().unwrap().remove("selectedLayer");
    let content = serde_json::to_vec(&json!({"state":identity,"meta":meta}))?;
    let identifier = format!("{:x}", Sha256::digest(content));
    let root = session.join("export-checkpoints");
    fs::create_dir_all(&root)?;
    ensure!(
        root.canonicalize()?.parent() == Some(session),
        "Unsafe export checkpoint folder."
    );
    let destination = root.join(format!("{identifier}.json"));
    if read(&destination, Value::Null)?.is_null() {
        projects::write_json(
            &destination,
            &json!({"version":1,"id":identifier,"created":chrono::Utc::now().to_rfc3339(),
                "state":snapshot,"meta":meta}),
        )?;
    }
    Ok(identifier)
}

pub fn checkpoint(session: &Path, identifier: &str) -> Result<Value> {
    ensure!(
        identifier.len() == 64
            && identifier
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "Invalid export checkpoint."
    );
    let root = session.join("export-checkpoints");
    ensure!(
        root.canonicalize()
            .context("Export checkpoint is unavailable.")?
            .parent()
            == Some(session),
        "Unsafe export checkpoint folder."
    );
    let saved = read(&root.join(format!("{identifier}.json")), Value::Null)?;
    ensure!(
        saved["version"] == 1 && saved["id"].as_str() == Some(identifier),
        "Export checkpoint is unavailable."
    );
    validate(&saved["state"], &saved["meta"])?;
    Ok(saved)
}

pub fn records(session: &Path) -> Result<Value> {
    let value = read(&session.join("exports").join(".history.json"), json!({}))?;
    ensure!(value.is_object(), "Invalid export history.");
    Ok(value)
}

pub fn image_digest(path: &Path) -> Result<String> {
    let mut reader = BufReader::new(fs::File::open(path)?);
    let mut digest = Sha256::new();
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            break;
        }
        digest.update(bytes);
        let length = bytes.len();
        reader.consume(length);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Caller serializes publication, so concurrent exports cannot lose index entries.
pub fn record(session: &Path, image: &Path, filename: &str, identifier: &str) -> Result<()> {
    checkpoint(session, identifier)?;
    let mut history = records(session)?;
    history.as_object_mut().context("Invalid export history")?.insert(
        filename.into(),
        json!({"checkpoint":identifier,"sha256":image_digest(image)?,"size_bytes":image.metadata()?.len()}),
    );
    projects::write_json(&session.join("exports").join(".history.json"), &history)
}

pub fn image_checkpoint(session: &Path, image: &Path, filename: &str) -> Result<String> {
    let history = records(session)?;
    let entry = &history[filename];
    let identifier = entry["checkpoint"]
        .as_str()
        .context("This export has no saved edit checkpoint.")?;
    ensure!(
        entry["size_bytes"].as_u64() == Some(image.metadata()?.len())
            && entry["sha256"].as_str() == Some(&image_digest(image)?),
        "This image has changed since it was exported."
    );
    checkpoint(session, identifier)?;
    Ok(identifier.to_owned())
}

fn build_available(folder: &Path, meta: &Value) -> Result<bool> {
    if !folder.is_dir() || read(&folder.join("meta.json"), Value::Null)? != *meta {
        return Ok(false);
    }
    let pixels = meta["width"]
        .as_u64()
        .context("Missing build width")?
        .checked_mul(meta["height"].as_u64().context("Missing build height")?)
        .context("Build dimensions overflow")?;
    let mut layers: Vec<&Value> = meta["layers"].as_array().into_iter().flatten().collect();
    if meta["luminance"].is_object() {
        layers.push(&meta["luminance"]);
    }
    for layer in layers {
        for (field, bytes) in [
            (
                "file",
                pixels.checked_mul(2).context("Plane size overflow")?,
            ),
            ("coverage_file", pixels),
        ] {
            if let Some(filename) = layer[field].as_str() {
                if Path::new(filename)
                    .file_name()
                    .and_then(|name| name.to_str())
                    != Some(filename)
                {
                    return Ok(false);
                }
                let plane = folder.join(filename);
                if !plane.is_file()
                    || plane.canonicalize()?.parent() != Some(folder)
                    || plane.metadata()?.len() != bytes
                {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

fn matching_build(session: &Path, meta: &Value) -> Result<Option<String>> {
    let current = session.join("project");
    if current.is_dir()
        && current.canonicalize()?.parent() == Some(session)
        && build_available(&current, meta)?
    {
        return Ok(None);
    }
    let root = session.join("builds");
    if root.is_dir() {
        ensure!(
            root.canonicalize()?.parent() == Some(session),
            "Unsafe build history"
        );
        let mut archives: Vec<PathBuf> = fs::read_dir(&root)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<_>>()?;
        archives.sort();
        for archive in archives {
            let project = archive.join("project");
            if archive.is_dir()
                && archive.canonicalize()?.parent() == Some(root.as_path())
                && project.is_dir()
                && project.canonicalize()?.parent() == Some(archive.as_path())
                && build_available(&project, meta)?
            {
                return Ok(Some(
                    archive.file_name().unwrap().to_string_lossy().into_owned(),
                ));
            }
        }
    }
    anyhow::bail!(
        "The original build or its image planes are no longer available. Current edits were not changed."
    )
}

pub fn restore(session: &Path, identifier: &str) -> Result<()> {
    let saved = checkpoint(session, identifier)?;
    let meta = &saved["meta"];
    let build = matching_build(session, meta)?;
    let current = projects::edits(session)?;
    let previous = read(&session.join("project").join("meta.json"), Value::Null)?;
    if !current.is_null() {
        validate(&current, &previous)?;
        ensure!(
            current["checkpoints"].is_null() || current["checkpoints"].is_array(),
            "Invalid checkpoint history. Current edits were not changed."
        );
    }
    let mut points = current["checkpoints"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if current.is_object() && previous.is_object() {
        if previous != *meta {
            for point in &mut points {
                if point["exportCheckpoint"].is_null() {
                    let id = save_checkpoint(session, &point["state"], &previous)?;
                    point["exportCheckpoint"] = json!(id);
                }
            }
        }
        let return_id = save_checkpoint(session, &current, &previous)?;
        if !points
            .iter()
            .any(|point| point["exportCheckpoint"].as_str() == Some(&return_id))
        {
            points.push(json!({"id":return_id,"exportCheckpoint":return_id,
                "name":"Before opening exported version","created":chrono::Utc::now().to_rfc3339(),
                "state":checkpoint(session, &return_id)?["state"]}));
        }
    }
    let mut state = saved["state"].clone();
    ensure!(
        state["layers"].is_array() && state["global"].is_object(),
        "Invalid checkpoint edits"
    );
    state["checkpoints"] = json!(points);
    state["sources"] = json!(
        meta["layers"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|layer| layer["key"].clone())
            .collect::<Vec<_>>()
    );
    if let Some(build) = build {
        crate::build::restore_with_edits(session, &build, Some(&state))
    } else {
        let revision = projects::read_json(&session.join("edits.rev.json"), json!({}))?["revision"]
            .as_u64()
            .unwrap_or(0)
            + 1;
        projects::publish_bundle(
            session,
            &[
                ("edits.json", Some(serde_json::to_vec_pretty(&state)?)),
                (
                    "edits.rev.json",
                    Some(serde_json::to_vec_pretty(&json!({
                        "revision":revision,"source":"restore","note":"Restored exported version"
                    }))?),
                ),
            ],
            None,
            false,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archived_look_keeps_the_working_look_and_manual_checkpoints_recoverable() {
        let temporary = tempfile::tempdir().unwrap();
        let session = temporary.path().canonicalize().unwrap();
        let archive = session.join("builds").join("original").join("project");
        fs::create_dir_all(&archive).unwrap();
        let first = json!({"width":2,"height":1,"built_at":"first",
            "layers":[{"key":"a","file":"a.f16","filter":"A"}]});
        let mut second = first.clone();
        second["built_at"] = json!("second");
        projects::write_json(&archive.join("meta.json"), &first).unwrap();
        fs::write(archive.join("a.f16"), [0; 4]).unwrap();
        let project = session.join("project");
        fs::create_dir(&project).unwrap();
        projects::write_json(&project.join("meta.json"), &second).unwrap();
        fs::write(project.join("a.f16"), [1; 4]).unwrap();
        let original = json!({"version":1,"layers":[],"global":{"output":{"exposure":0}}});
        let identifier = save_checkpoint(&session, &original, &first).unwrap();
        let mut current = json!({"version":1,"layers":[],"global":{"output":{"exposure":0.8}}});
        let mut manual = current.clone();
        manual["global"]["output"]["exposure"] = json!(0.3);
        current["checkpoints"] = json!([{"id":"manual","name":"Manual look","created":"first",
            "state":manual}]);
        projects::write_json(&session.join("edits.json"), &current).unwrap();
        restore(&session, &identifier).unwrap();
        let restored = projects::edits(&session).unwrap();
        assert_eq!(restored["global"], original["global"]);
        assert_eq!(
            projects::read_json(&project.join("meta.json"), Value::Null).unwrap(),
            first
        );
        let manual = restored["checkpoints"][0]["exportCheckpoint"]
            .as_str()
            .unwrap();
        assert_eq!(checkpoint(&session, manual).unwrap()["meta"], second);
        let return_id = restored["checkpoints"][1]["exportCheckpoint"]
            .as_str()
            .unwrap();
        restore(&session, return_id).unwrap();
        assert_eq!(
            projects::edits(&session).unwrap()["global"],
            current["global"]
        );
        assert_eq!(
            projects::read_json(&project.join("meta.json"), Value::Null).unwrap(),
            second
        );
        let before = fs::read(session.join("edits.json")).unwrap();
        fs::remove_file(project.join("a.f16")).unwrap();
        assert!(
            restore(&session, return_id)
                .unwrap_err()
                .to_string()
                .contains("no longer available")
        );
        assert_eq!(fs::read(session.join("edits.json")).unwrap(), before);
    }

    #[test]
    fn exports_share_an_immutable_look_across_sizes_without_overwriting_newer_edits() {
        let temporary = tempfile::tempdir().unwrap();
        let session = temporary.path().canonicalize().unwrap();
        fs::create_dir(session.join("exports")).unwrap();
        let state = json!({"version":1,"layers":[],"global":{"output":{"exposure":0}}});
        let meta = json!({"width":16,"height":8,"built_at":"first"});
        let identifier = save_checkpoint(&session, &state, &meta).unwrap();
        let newer = json!({"version":1,"layers":[],"global":{"output":{"exposure":1}}});
        projects::write_json(&session.join("edits.json"), &newer).unwrap();
        let mut selected = state.clone();
        selected["selectedLayer"] = json!("another selection");
        selected["checkpoints"] = json!([{"unrelated":"history"}]);
        assert_eq!(
            save_checkpoint(&session, &selected, &meta).unwrap(),
            identifier
        );
        for size in [4, 8] {
            let filename = format!("image-{size}.png");
            let image = session.join("exports").join(&filename);
            image::RgbImage::from_pixel(size, size, image::Rgb([64, 128, 192]))
                .save(&image)
                .unwrap();
            record(&session, &image, &filename, &identifier).unwrap();
            let history = records(&session).unwrap();
            assert_eq!(history[&filename]["checkpoint"], identifier);
            assert_eq!(history[&filename]["sha256"], image_digest(&image).unwrap());
        }
        assert_eq!(checkpoint(&session, &identifier).unwrap()["state"], state);
        assert_eq!(checkpoint(&session, &identifier).unwrap()["meta"], meta);
        assert_eq!(
            projects::read_json(&session.join("edits.json"), Value::Null).unwrap(),
            newer
        );
        assert_eq!(
            fs::read_dir(session.join("export-checkpoints"))
                .unwrap()
                .count(),
            1
        );
        assert_ne!(
            save_checkpoint(&session, &newer, &meta).unwrap(),
            identifier
        );
        let rebuilt = json!({"width":16,"height":8,"built_at":"second"});
        assert_ne!(
            save_checkpoint(&session, &state, &rebuilt).unwrap(),
            identifier
        );
    }
}
