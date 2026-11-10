use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use super::support::{ImportReport, lock_data_dir};

type ReferenceRewrite<'a> = dyn Fn(&str) -> Result<Option<String>> + 'a;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Settings {
    pub library_root: PathBuf,
    #[serde(default)]
    pub pending_root: Option<PathBuf>,
}

pub fn settings_path() -> Result<PathBuf> {
    Ok(super::support::settings_dir()
        .context("No local application settings folder is available")?
        .join("settings.json"))
}

pub fn read_settings(path: &Path) -> Result<Option<Settings>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).context("Invalid desktop storage settings")?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("Reading desktop storage settings"),
    }
}

pub fn save_settings(path: &Path, settings: &Settings) -> Result<()> {
    astroimp_desktop::projects::write_json(path, &serde_json::to_value(settings)?)
}

pub fn validate_root(path: &Path, protected: &Path) -> Result<PathBuf> {
    ensure!(
        path.is_absolute(),
        "Choose an absolute library folder path."
    );
    ensure!(
        path.components()
            .all(|part| !matches!(part, Component::ParentDir)),
        "The library path must not contain '..'."
    );
    ensure!(
        path.parent().is_some(),
        "Choose a dedicated folder, not a drive root."
    );
    let resolved = resolve_path(path)?;
    match protected.canonicalize() {
        Ok(protected) => ensure!(
            !resolved.starts_with(&protected) && !protected.starts_with(&resolved),
            "The original Python app's data folder is protected. Choose an independent library."
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("Checking the protected Python data folder"),
    }
    Ok(resolved)
}

// Resolve existing parents too, including Windows short names and junctions.
fn resolve_path(path: &Path) -> Result<PathBuf> {
    let mut existing = path;
    let mut suffix = Vec::new();
    while !existing.exists() {
        suffix.push(
            existing
                .file_name()
                .context("The library drive does not exist")?,
        );
        existing = existing
            .parent()
            .context("The library drive does not exist")?;
    }
    let mut resolved = existing
        .canonicalize()
        .context("Reading the library folder")?;
    for part in suffix.into_iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}

#[cfg(windows)]
pub fn available_bytes(path: &Path) -> Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            directory: *const u16,
            available: *mut u64,
            total: *mut u64,
            free: *mut u64,
        ) -> i32;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    ensure!(
        ok != 0,
        "Reading free disk space: {}",
        std::io::Error::last_os_error()
    );
    Ok(available)
}

#[cfg(not(windows))]
pub fn available_bytes(_path: &Path) -> Result<u64> {
    anyhow::bail!("Desktop library space checks currently require Windows.")
}

fn portable_string(value: &str, source: &Path) -> Result<Option<String>> {
    let path = Path::new(value);
    if !path.is_absolute() {
        return Ok(None);
    }
    let path = resolve_path(path)?;
    Ok(path
        .strip_prefix(source)
        .ok()
        .filter(|suffix| !suffix.as_os_str().is_empty())
        .map(|suffix| format!("library:{}", suffix.display())))
}

fn remap(value: &mut Value, replace: &ReferenceRewrite<'_>) -> Result<bool> {
    Ok(match value {
        Value::String(text) => {
            if let Some(relative) = replace(text)? {
                *text = relative;
                true
            } else {
                false
            }
        }
        Value::Array(items) => {
            let mut changed = false;
            for value in items {
                changed |= remap(value, replace)?;
            }
            changed
        }
        Value::Object(items) => {
            let mut changed = false;
            for value in items.values_mut() {
                changed |= remap(value, replace)?;
            }
            changed
        }
        _ => false,
    })
}

fn equal_files(a: &Path, b: &Path) -> Result<bool> {
    let mut a = File::open(a)?;
    let mut b = File::open(b)?;
    if a.metadata()?.len() != b.metadata()?.len() {
        return Ok(false);
    }
    let mut left = vec![0; 1024 * 1024];
    let mut right = vec![0; left.len()];
    loop {
        let count = a.read(&mut left)?;
        if count == 0 {
            return Ok(true);
        }
        b.read_exact(&mut right[..count])?;
        if left[..count] != right[..count] {
            return Ok(false);
        }
    }
}

fn copy_tree(
    source: &Path,
    target: &Path,
    replace: Option<&ReferenceRewrite<'_>>,
    report: &mut ImportReport,
) -> Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".astroimp-desktop.lock" {
            continue;
        }
        let kind = entry.file_type()?;
        ensure!(
            !kind.is_symlink(),
            "Cannot copy a link: {}",
            entry.path().display()
        );
        let destination = target.join(name);
        if kind.is_dir() {
            copy_tree(&entry.path(), &destination, replace, report)?;
            continue;
        }
        let from = entry.path();
        let transformed = if from.extension().is_some_and(|ext| ext == "json") {
            if let Some(replace) = replace {
                let mut value: Value = serde_json::from_slice(&fs::read(&from)?)
                    .with_context(|| format!("Invalid project JSON: {}", from.display()))?;
                remap(&mut value, replace)?
                    .then(|| serde_json::to_vec_pretty(&value))
                    .transpose()?
            } else {
                None
            }
        } else {
            None
        };
        if destination.exists() {
            let same = match &transformed {
                Some(bytes) => fs::read(&destination)? == *bytes,
                None => equal_files(&from, &destination)?,
            };
            ensure!(
                same,
                "Import would overwrite a different file: {}",
                destination.display()
            );
            report.files_kept += 1;
        } else {
            let mut temporary = tempfile::NamedTempFile::new_in(target)?;
            if let Some(bytes) = &transformed {
                temporary.write_all(bytes)?;
                temporary.flush()?;
                ensure!(
                    fs::read(temporary.path())? == *bytes,
                    "Project JSON verification failed"
                );
            } else {
                fs::copy(&from, temporary.path())?;
                ensure!(
                    equal_files(&from, temporary.path())?,
                    "Copy verification failed: {}",
                    from.display()
                );
            }
            temporary
                .as_file()
                .set_modified(entry.metadata()?.modified()?)?;
            temporary.as_file().sync_all()?;
            temporary
                .persist_noclobber(&destination)
                .map_err(|e| e.error)?;
            report.files_copied += 1;
        }
        report.bytes_verified += entry.metadata()?.len();
        if (report.files_copied + report.files_kept).is_multiple_of(64) {
            eprintln!(
                "Library transfer: {} files, {:.1} GiB verified",
                report.files_copied + report.files_kept,
                report.bytes_verified as f64 / 1073741824.
            );
        }
    }
    Ok(())
}

pub fn import_data(source: &Path, target: &Path) -> Result<ImportReport> {
    fs::create_dir_all(target)?;
    let required = remaining_bytes(&source.join("downloads"), &target.join("downloads"))?
        + remaining_bytes(&source.join("sessions"), &target.join("sessions"))?;
    ensure!(
        available_bytes(target)? >= required,
        "Not enough free space to copy the library."
    );
    let mut report = ImportReport::default();
    let marker = target.join(".astroimp-import.json");
    let expected = json!({"source":source});
    if marker.exists() {
        ensure!(
            astroimp_desktop::projects::read_json(&marker, Value::Null)? == expected,
            "This library has an unfinished import from another folder."
        );
    } else {
        astroimp_desktop::projects::write_json(&marker, &expected)?;
    }
    if source.join("downloads").is_dir() {
        copy_tree(
            &source.join("downloads"),
            &target.join("downloads"),
            None,
            &mut report,
        )?;
    }
    copy_tree(
        &source.join("sessions"),
        &target.join("sessions"),
        Some(&|value| portable_string(value, source)),
        &mut report,
    )?;
    for name in ["bookmarks.json", "aladin-view.json"] {
        if source.join(name).is_file() {
            let bytes = fs::read(source.join(name))?;
            let destination = target.join(name);
            if destination.exists() {
                ensure!(
                    fs::read(&destination)? == bytes,
                    "Import would overwrite {name}"
                );
                report.files_kept += 1;
            } else {
                astroimp_desktop::projects::write_bytes(&destination, &bytes)?;
                report.files_copied += 1;
            }
            report.bytes_verified += bytes.len() as u64;
        }
    }
    fs::remove_file(marker)?;
    Ok(report)
}

/// Verify a single project privately, then publish it without replacing an existing project.
/// Each attempt owns only its fresh temp directory; abandoned or user-owned folders are untouched.
pub fn import_single_project(source: &Path, target: &Path) -> Result<ImportReport> {
    ensure!(
        !target.try_exists()?,
        "A project named {} already exists in the NightBloom data folder.",
        target
            .file_name()
            .context("The project folder has no name")?
            .to_string_lossy()
    );
    let parent = target
        .parent()
        .context("The project folder has no parent")?;
    fs::create_dir_all(parent)?;
    // Design decision: dot-prefixed staging is not a project ID. Only a fully verified tree
    // is renamed into sessions/<id>; a crash cannot publish a partial project or block retry.
    let staging = tempfile::Builder::new()
        .prefix(".astroimp-project-import-")
        .tempdir_in(parent)
        .context("Creating private project import staging")?;
    let staged_project = staging.path().join("project");
    let mut report = ImportReport::default();
    let external = |value: &str| {
        if value.starts_with("library:") {
            Ok(Some(
                astroimp_desktop::projects::science_source(source, value)?
                    .to_string_lossy()
                    .into_owned(),
            ))
        } else {
            Ok(None)
        }
    };
    copy_tree(source, &staged_project, Some(&external), &mut report)?;
    ensure!(
        !target.try_exists()?,
        "A project named {} already exists in the NightBloom data folder.",
        target
            .file_name()
            .context("The project folder has no name")?
            .to_string_lossy()
    );
    fs::rename(&staged_project, target)
        .with_context(|| format!("Publishing verified project {}", target.display()))?;
    Ok(report)
}

pub fn pending_import(root: &Path) -> Result<Option<PathBuf>> {
    let marker = root.join(".astroimp-import.json");
    if !marker.exists() {
        return Ok(None);
    }
    let value = astroimp_desktop::projects::read_json(&marker, Value::Null)?;
    Ok(Some(PathBuf::from(
        value["source"]
            .as_str()
            .context("Invalid unfinished import record")?,
    )))
}

fn remaining_bytes(source: &Path, target: &Path) -> Result<u64> {
    if !source.exists() {
        return Ok(0);
    }
    let mut bytes = 0;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        ensure!(
            !kind.is_symlink(),
            "Cannot transfer links: {}",
            entry.path().display()
        );
        let destination = target.join(entry.file_name());
        if kind.is_dir() {
            bytes += remaining_bytes(&entry.path(), &destination)?;
        } else if !destination.exists() {
            bytes += entry.metadata()?.len();
        }
    }
    Ok(bytes)
}

// Unlike copy_tree, this must never repair a missing file: settings already designate this
// destination as committed. Require the complete original inventory and bytes, with no extras
// except the destination's own transfer marker and the locks excluded by the copy protocol.
fn verify_relocated_tree(source: &Path, target: &Path, marker: &Path) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_name() == ".astroimp-desktop.lock" {
            continue;
        }
        let kind = entry.file_type()?;
        ensure!(
            !kind.is_symlink(),
            "Cannot verify a link: {}",
            entry.path().display()
        );
        let copied = target.join(entry.file_name());
        let copied_kind = fs::symlink_metadata(&copied)
            .with_context(|| {
                format!(
                    "The retained library transfer is incomplete: {}",
                    copied.display()
                )
            })?
            .file_type();
        ensure!(
            !copied_kind.is_symlink(),
            "Cannot verify a link: {}",
            copied.display()
        );
        if kind.is_dir() {
            ensure!(
                copied_kind.is_dir(),
                "Library transfer directory mismatch: {}",
                copied.display()
            );
            verify_relocated_tree(&entry.path(), &copied, marker)?;
        } else {
            ensure!(
                kind.is_file() && copied_kind.is_file() && equal_files(&entry.path(), &copied)?,
                "Library transfer verification failed: {}",
                copied.display()
            );
        }
    }
    for entry in fs::read_dir(target)? {
        let entry = entry?;
        if entry.file_name() == ".astroimp-desktop.lock" || entry.path() == marker {
            continue;
        }
        ensure!(
            source.join(entry.file_name()).try_exists()?,
            "The retained library transfer contains an unrelated entry: {}",
            entry.path().display()
        );
    }
    Ok(())
}

/// Resume a pending relocation, or finalize a committed copy whose marker survived publication.
/// Saved-root recovery is read-only until provenance and every copied byte have been verified.
pub fn finish_relocation(settings_file: &Path, protected: &Path) -> Result<Option<PathBuf>> {
    let Some(mut settings) = read_settings(settings_file)? else {
        return Ok(None);
    };
    let source = validate_root(&settings.library_root, protected)?;
    ensure!(
        source.is_dir(),
        "The saved library {} is unavailable. Reconnect its drive or choose its new location; no empty replacement was created.",
        source.display()
    );
    if let Some(pending) = &settings.pending_root {
        let destination = validate_root(pending, protected)?;
        ensure!(
            !source.starts_with(&destination) && !destination.starts_with(&source),
            "The old and new libraries must not contain each other."
        );
        let _source_lock = lock_data_dir(&source)?;
        let _destination_lock = lock_data_dir(&destination)?;
        let marker = destination.join(".astroimp-transfer.json");
        let expected = json!({"source":source});
        if marker.exists() {
            ensure!(
                astroimp_desktop::projects::read_json(&marker, Value::Null)? == expected,
                "This folder belongs to a different transfer."
            );
        } else {
            ensure!(
                fs::read_dir(&destination)?
                    .all(|entry| entry.is_ok_and(|e| e.file_name() == ".astroimp-desktop.lock")),
                "Choose an empty destination folder; existing libraries will not be overwritten."
            );
            astroimp_desktop::projects::write_json(&marker, &expected)?;
        }
        ensure!(
            available_bytes(&destination)? >= remaining_bytes(&source, &destination)?,
            "Not enough free space to relocate the library."
        );
        let mut report = ImportReport::default();
        copy_tree(&source, &destination, None, &mut report)?;
        settings.library_root = destination.clone();
        settings.pending_root = None;
        save_settings(settings_file, &settings)?;
        fs::remove_file(marker)?;
        return Ok(Some(destination));
    }
    let marker = source.join(".astroimp-transfer.json");
    if marker.try_exists()? {
        let _destination_lock = lock_data_dir(&source)?;
        ensure!(
            fs::symlink_metadata(&marker)?.file_type().is_file(),
            "The retained library transfer record is not a regular file."
        );
        let record = astroimp_desktop::projects::read_json(&marker, Value::Null)
            .context("Reading the retained library transfer record")?;
        let original = PathBuf::from(
            record["source"]
                .as_str()
                .context("Invalid retained library transfer provenance")?,
        );
        let original = validate_root(&original, protected)?;
        ensure!(
            record == json!({"source":original}),
            "Invalid retained library transfer provenance."
        );
        ensure!(
            !original.starts_with(&source) && !source.starts_with(&original),
            "The old and new libraries must not contain each other."
        );
        ensure!(
            original.is_dir(),
            "The original library {} is unavailable; reconnect it to verify the retained transfer.",
            original.display()
        );
        ensure!(
            !original.join(".astroimp-transfer.json").try_exists()?,
            "The original library has an unfinished transfer; retained transfer provenance cannot be verified."
        );
        let _original_lock = lock_data_dir(&original)?;
        verify_relocated_tree(&original, &source, &marker)
            .context("Verifying the committed library transfer; no files were changed")?;
        fs::remove_file(&marker).context(
            "Finalizing the verified library transfer; the saved destination is unchanged",
        )?;
    }
    Ok(Some(source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_is_verified_portable_and_refuses_conflicting_files() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("original");
        let target = tmp.path().join("library");
        fs::create_dir_all(source.join("downloads")).unwrap();
        fs::create_dir_all(source.join("sessions/a/project")).unwrap();
        let raw = source.join("downloads/science.fits");
        fs::write(&raw, [1, 2, 3]).unwrap();
        let meta = source.join("sessions/a/project/meta.json");
        let original = json!({"layers":[{"source":raw,"name":"Keep my look"}]});
        astroimp_desktop::projects::write_json(&meta, &original).unwrap();
        import_data(&source.canonicalize().unwrap(), &target).unwrap();
        let copied: Value =
            serde_json::from_slice(&fs::read(target.join("sessions/a/project/meta.json")).unwrap())
                .unwrap();
        assert_eq!(
            copied["layers"][0]["source"],
            "library:downloads\\science.fits"
        );
        assert_eq!(copied["layers"][0]["name"], "Keep my look");
        assert_eq!(
            astroimp_desktop::projects::read_json(&meta, Value::Null).unwrap(),
            original
        );
        import_data(&source.canonicalize().unwrap(), &target).unwrap();
        fs::write(target.join("downloads/science.fits"), [9, 9, 9]).unwrap();
        assert!(import_data(&source.canonicalize().unwrap(), &target).is_err());
        assert_eq!(fs::read(&raw).unwrap(), [1, 2, 3]);
    }

    #[test]
    fn relocation_switches_only_after_verification_and_keeps_the_source() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("first");
        let target = tmp.path().join("second");
        fs::create_dir_all(source.join("sessions/a/exports")).unwrap();
        fs::write(source.join("sessions/a/exports/art.png"), b"art").unwrap();
        let settings_file = tmp.path().join("config/settings.json");
        save_settings(
            &settings_file,
            &Settings {
                library_root: source.clone(),
                pending_root: Some(target.clone()),
            },
        )
        .unwrap();
        let protected = tmp.path().join("protected");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("unrelated.txt"), b"mine").unwrap();
        assert!(finish_relocation(&settings_file, &protected).is_err());
        assert_eq!(
            read_settings(&settings_file).unwrap().unwrap().library_root,
            source
        );
        fs::remove_file(target.join("unrelated.txt")).unwrap();
        finish_relocation(&settings_file, &protected).unwrap();
        assert!(source.join("sessions/a/exports/art.png").is_file());
        assert_eq!(
            fs::read(target.join("sessions/a/exports/art.png")).unwrap(),
            b"art"
        );
        assert!(
            read_settings(&settings_file)
                .unwrap()
                .unwrap()
                .pending_root
                .is_none()
        );
    }

    fn committed_transfer_fixture(root: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let source = root.join("first");
        let target = root.join("second");
        fs::create_dir_all(source.join("sessions/a/exports")).unwrap();
        fs::write(source.join("sessions/a/exports/art.png"), b"art").unwrap();
        fs::write(
            source.join("sessions/a/session.json"),
            b"{\"name\":\"Keep my look\"}",
        )
        .unwrap();
        let source = source.canonicalize().unwrap();
        let mut report = ImportReport::default();
        copy_tree(&source, &target, None, &mut report).unwrap();
        let target = target.canonicalize().unwrap();
        let marker = target.join(".astroimp-transfer.json");
        astroimp_desktop::projects::write_json(&marker, &json!({"source":source})).unwrap();
        let settings_file = root.join("config/settings.json");
        // Design decision: persist the exact crash boundary after settings publication but
        // before marker removal. Recovery must verify, not recopy or bless incomplete data.
        save_settings(
            &settings_file,
            &Settings {
                library_root: target.clone(),
                pending_root: None,
            },
        )
        .unwrap();
        (source, target, marker, settings_file)
    }

    #[test]
    fn committed_relocation_with_a_retained_marker_is_finalized_at_bootstrap() {
        let tmp = tempfile::tempdir().unwrap();
        let (source, target, marker, settings_file) = committed_transfer_fixture(tmp.path());
        let original = fs::read(source.join("sessions/a/session.json")).unwrap();
        let saved = fs::read(&settings_file).unwrap();

        assert_eq!(
            finish_relocation(&settings_file, &tmp.path().join("protected")).unwrap(),
            Some(target.clone())
        );
        assert!(!marker.exists());
        assert_eq!(fs::read(&settings_file).unwrap(), saved);
        assert_eq!(
            fs::read(source.join("sessions/a/session.json")).unwrap(),
            original
        );
        assert_eq!(
            fs::read(source.join("sessions/a/exports/art.png")).unwrap(),
            b"art"
        );
        assert_eq!(
            fs::read(target.join("sessions/a/exports/art.png")).unwrap(),
            b"art"
        );
        assert_eq!(
            finish_relocation(&settings_file, &tmp.path().join("protected")).unwrap(),
            Some(target)
        );
    }

    #[test]
    fn retained_transfer_markers_require_complete_independent_originals() {
        for invalid in [
            "missing",
            "different",
            "extra",
            "wrong-source",
            "self",
            "malformed",
            "protected",
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let (source, target, marker, settings_file) = committed_transfer_fixture(tmp.path());
            let protected = tmp.path().join("protected");
            match invalid {
                "missing" => fs::remove_file(target.join("sessions/a/exports/art.png")).unwrap(),
                "different" => {
                    fs::write(target.join("sessions/a/exports/art.png"), b"bad").unwrap()
                }
                "extra" => fs::write(target.join("unrelated.txt"), b"mine").unwrap(),
                "wrong-source" => {
                    let unrelated = tmp.path().join("unrelated");
                    fs::create_dir_all(unrelated.join("sessions")).unwrap();
                    let unrelated = unrelated.canonicalize().unwrap();
                    astroimp_desktop::projects::write_json(&marker, &json!({"source":unrelated}))
                        .unwrap();
                }
                "self" => {
                    astroimp_desktop::projects::write_json(&marker, &json!({"source":target}))
                        .unwrap();
                }
                "malformed" => fs::write(&marker, b"{\"source\":").unwrap(),
                "protected" => {
                    fs::create_dir_all(&protected).unwrap();
                    astroimp_desktop::projects::write_json(&marker, &json!({"source":protected}))
                        .unwrap();
                }
                _ => unreachable!(),
            }
            let marker_bytes = fs::read(&marker).unwrap();
            let saved = fs::read(&settings_file).unwrap();
            assert!(
                finish_relocation(&settings_file, &protected).is_err(),
                "{invalid} marker was silently blessed"
            );
            assert_eq!(fs::read(&marker).unwrap(), marker_bytes, "{invalid}");
            assert_eq!(fs::read(&settings_file).unwrap(), saved, "{invalid}");
            assert_eq!(
                fs::read(source.join("sessions/a/exports/art.png")).unwrap(),
                b"art"
            );
        }
    }

    #[test]
    fn committed_transfer_finalization_requires_both_library_locks() {
        let tmp = tempfile::tempdir().unwrap();
        let (source, target, marker, settings_file) = committed_transfer_fixture(tmp.path());
        let protected = tmp.path().join("protected");
        for locked_root in [&source, &target] {
            let held = lock_data_dir(locked_root).unwrap();
            let error = finish_relocation(&settings_file, &protected)
                .unwrap_err()
                .to_string();
            assert!(error.contains("already using"), "{error}");
            assert!(marker.is_file());
            drop(held);
        }
        assert_eq!(
            finish_relocation(&settings_file, &protected).unwrap(),
            Some(target)
        );
        assert!(!marker.exists());
    }

    #[test]
    fn an_uncreated_child_cannot_bypass_protected_data() {
        let tmp = tempfile::tempdir().unwrap();
        let protected = tmp.path().join("original");
        fs::create_dir_all(&protected).unwrap();
        assert!(validate_root(&protected.join("new/child"), &protected).is_err());
        assert!(validate_root(tmp.path(), &protected).is_err());
    }

    #[test]
    fn a_missing_saved_library_is_not_recreated_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("moved-away");
        let settings = tmp.path().join("settings.json");
        save_settings(
            &settings,
            &Settings {
                library_root: missing.clone(),
                pending_root: None,
            },
        )
        .unwrap();
        assert!(finish_relocation(&settings, &tmp.path().join("protected")).is_err());
        assert!(!missing.exists());
    }

    #[test]
    fn a_single_portable_project_keeps_its_original_external_inputs() {
        let tmp = tempfile::tempdir().unwrap();
        let library = tmp.path().join("original");
        let source = library.join("sessions/a");
        fs::create_dir_all(source.join("project")).unwrap();
        fs::create_dir_all(library.join("downloads")).unwrap();
        fs::write(library.join("downloads/science.fits"), b"science").unwrap();
        astroimp_desktop::projects::write_json(
            &source.join("project/meta.json"),
            &json!({"layers":[{"source":"library:downloads\\science.fits"}]}),
        )
        .unwrap();
        let target = tmp.path().join("new/sessions/a");
        import_single_project(&source, &target).unwrap();
        let copied =
            astroimp_desktop::projects::read_json(&target.join("project/meta.json"), Value::Null)
                .unwrap();
        let science = astroimp_desktop::projects::science_source(
            &target,
            copied["layers"][0]["source"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(fs::read(science).unwrap(), b"science");
        assert!(!tmp.path().join("new/downloads").exists());
    }
}
