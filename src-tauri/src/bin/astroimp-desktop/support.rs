//! Desktop-shell helpers that need no GUI: data-folder locking, frontend discovery and the
//! one-way project import. Kept free of Tauri so they are unit-testable headlessly.

use anyhow::{Context, Result, bail};
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    path::{Path, PathBuf},
};

/// Same identifier as `tauri.conf.json`, so app data sits beside the WebView2 profile.
pub const IDENTIFIER: &str = "com.astroimp.studio";
const LOCK_FILE: &str = ".astroimp-desktop.lock";

/// Small bootstrap settings only; the user chooses the library's bulk-data location.
pub fn settings_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_DATA_HOME").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share"))
        })?;
    Some(base.join(IDENTIFIER))
}

pub fn display_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{unc}"));
    }
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
}

/// Places the built frontend may live, most specific first. Resolved from the executable's
/// location (Tauri copies `bundle.resources` to `<exe dir>\frontend`), never the working directory.
pub fn frontend_candidates(exe_dir: &Path, manifest_dir: &Path) -> Vec<PathBuf> {
    vec![
        exe_dir.join("frontend"),
        exe_dir.join("resources").join("frontend"),
        // Development checkout: `cargo run` from the repository.
        manifest_dir.join("..").join("frontend").join("dist"),
    ]
}

/// First candidate holding an `index.html`; the error lists every place that was tried.
pub fn find_frontend(candidates: &[PathBuf]) -> Result<PathBuf> {
    if let Some(found) = candidates
        .iter()
        .find(|dir| dir.join("index.html").is_file())
    {
        return Ok(found.clone());
    }
    let tried: Vec<_> = candidates
        .iter()
        .map(|dir| format!("  {}", dir.display()))
        .collect();
    bail!(
        "The built NightBloom frontend (index.html) was not found. Looked in:\n{}",
        tried.join("\n")
    )
}

/// Exclusive claim on a data folder, held for the life of the process. The OS drops it when the
/// process ends, so a crash never leaves a stale lock behind.
pub struct DataLock(#[allow(dead_code)] File);

pub fn lock_data_dir(data: &Path) -> Result<DataLock> {
    fs::create_dir_all(data).with_context(|| format!("Creating data folder {}", data.display()))?;
    let path = data.join(LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("Opening {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(DataLock(file)),
        Err(TryLockError::WouldBlock) => bail!(
            "Another NightBloom instance is already using the data folder {}.",
            data.display()
        ),
        Err(TryLockError::Error(error)) => Err(error).context("Locking the data folder"),
    }
}

/// True when the folder has no project store yet (first run).
pub fn is_unpopulated(data: &Path) -> bool {
    !data.join("sessions").exists()
}

#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ImportReport {
    pub files_copied: usize,
    pub files_kept: usize,
    pub bytes_verified: u64,
}

fn nested(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Copies an existing NightBloom data root including shared downloads, or a single project folder.
/// The source is only ever read. Files already present in `data` are kept, never overwritten,
/// and links are not followed.
pub fn import_projects(source: &Path, data: &Path) -> Result<ImportReport> {
    let source = source
        .canonicalize()
        .with_context(|| format!("Reading {}", source.display()))?;
    fs::create_dir_all(data)?;
    let data = data.canonicalize()?;
    if nested(&source, &data) {
        bail!("The import folder and the NightBloom data folder must not contain each other.");
    }
    if source.join("sessions").is_dir() {
        super::library::import_data(&source, &data)
    } else if source.join("session.json").is_file() {
        let name = source
            .file_name()
            .context("The project folder has no name")?;
        let target = data.join("sessions").join(name);
        if target.exists() {
            bail!(
                "A project named {} already exists in the NightBloom data folder.",
                name.to_string_lossy()
            );
        }
        super::library::import_single_project(&source, &target)
    } else {
        bail!(
            "{} is neither a NightBloom data folder (sessions\\) nor a project (session.json).",
            source.display()
        );
    }
}

/// Writes the headless-smoke report so a reader never sees a half-written file: write beside it,
/// then rename over the target. Creates the report's folder when missing.
pub fn write_report(path: &Path, text: &str) -> Result<()> {
    if let Some(folder) = path
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
    {
        fs::create_dir_all(folder).with_context(|| format!("Creating {}", folder.display()))?;
    }
    let name = path
        .file_name()
        .context("The report path has no file name")?;
    let partial = path.with_file_name(format!("{}.partial", name.to_string_lossy()));
    fs::write(&partial, text).with_context(|| format!("Writing {}", partial.display()))?;
    fs::rename(&partial, path).with_context(|| format!("Replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_is_written_whole_and_replaces_an_older_one() {
        let tmp = tempfile::tempdir().unwrap();
        let report = tmp.path().join("nested").join("smoke.json");
        write_report(&report, "{\"ok\":false}").unwrap();
        write_report(&report, "{\"ok\":true}").unwrap();
        assert_eq!(fs::read_to_string(&report).unwrap(), "{\"ok\":true}");
        assert!(!report.with_file_name("smoke.json.partial").exists());
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn import_copies_shared_downloads_but_not_disposable_caches() {
        let tmp = tempfile::tempdir().unwrap();
        let (source, data) = (tmp.path().join("src"), tmp.path().join("app"));
        write(&source.join("sessions/a/session.json"), "{}");
        write(&source.join("bookmarks.json"), "[]");
        write(&source.join("downloads/big.fits"), "raw");
        write(&source.join("cache/preview.png"), "cache");
        let report = import_projects(&source, &data).unwrap();
        assert_eq!(
            report,
            ImportReport {
                files_copied: 3,
                files_kept: 0,
                bytes_verified: 7,
            }
        );
        assert!(data.join("downloads/big.fits").is_file());
        assert!(!data.join("cache").exists());
        assert_eq!(
            fs::read_to_string(data.join("sessions/a/session.json")).unwrap(),
            "{}"
        );
        assert_eq!(
            fs::read_to_string(data.join("bookmarks.json")).unwrap(),
            "[]"
        );
        assert_eq!(
            fs::read_to_string(source.join("sessions/a/session.json")).unwrap(),
            "{}"
        );
        assert!(!data.join("sessions/a/session.json.importing").exists());
    }

    #[test]
    fn import_places_a_single_project_under_sessions_and_refuses_duplicates() {
        let tmp = tempfile::tempdir().unwrap();
        let (source, data) = (tmp.path().join("carina"), tmp.path().join("app"));
        write(&source.join("session.json"), "{}");
        write(&source.join("project/meta.json"), "{}");
        import_projects(&source, &data).unwrap();
        assert!(data.join("sessions/carina/project/meta.json").is_file());
        assert!(import_projects(&source, &data).is_err());
        assert!(
            super::super::library::import_single_project(&source, &data.join("sessions/carina"))
                .is_err()
        );
        write(&source.join("project/meta.json"), "{\"changed\":true}");
        assert!(
            super::super::library::import_single_project(&source, &data.join("sessions/carina"))
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(data.join("sessions/carina/project/meta.json")).unwrap(),
            "{}"
        );
    }

    #[test]
    fn malformed_nested_project_json_is_unpublished_and_repaired_input_can_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let (source, data) = (tmp.path().join("carina"), tmp.path().join("app"));
        write(&source.join("session.json"), "{\"name\":\"Keep my look\"}");
        write(&source.join("catalog.json"), "{}");
        write(
            &source.join("project/builds/old/meta.json"),
            "{\"layers\":[",
        );
        let error = import_projects(&source, &data).unwrap_err().to_string();
        assert!(error.contains("Invalid project JSON"), "{error}");
        // Design decision: failed imports never acquire a public sessions/<id> folder.
        // Retrying a repaired source must not be mistaken for overwriting a real project.
        assert!(!data.join("sessions/carina").exists());
        assert!(
            astroimp_desktop::projects::Projects::new(data.clone())
                .unwrap()
                .list()
                .unwrap()
                .is_empty()
        );
        assert_eq!(fs::read_dir(data.join("sessions")).unwrap().count(), 0);

        write(
            &source.join("project/builds/old/meta.json"),
            "{\"layers\":[]}",
        );
        import_projects(&source, &data).unwrap();
        assert_eq!(
            fs::read_to_string(data.join("sessions/carina/project/builds/old/meta.json")).unwrap(),
            "{\"layers\":[]}"
        );
        assert_eq!(
            astroimp_desktop::projects::Projects::new(data.clone())
                .unwrap()
                .list()
                .unwrap()
                .len(),
            1
        );
        assert!(import_projects(&source, &data).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn an_unreadable_nested_file_does_not_publish_a_project_or_block_retry() {
        use std::os::windows::fs::OpenOptionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let (source, data) = (tmp.path().join("carina"), tmp.path().join("app"));
        write(&source.join("session.json"), "{}");
        write(&source.join("project/meta.json"), "{}");
        let unreadable = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(source.join("project/meta.json"))
            .unwrap();
        assert!(import_projects(&source, &data).is_err());
        assert!(!data.join("sessions/carina").exists());
        assert_eq!(fs::read_dir(data.join("sessions")).unwrap().count(), 0);
        drop(unreadable);
        import_projects(&source, &data).unwrap();
        assert!(data.join("sessions/carina/project/meta.json").is_file());
    }

    #[test]
    fn abandoned_import_staging_is_unpublished_and_never_reused_or_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let (source, data) = (tmp.path().join("carina"), tmp.path().join("app"));
        write(&source.join("session.json"), "{}");
        write(&source.join("catalog.json"), "{}");
        fs::create_dir_all(data.join("sessions")).unwrap();
        let abandoned = tempfile::Builder::new()
            .prefix(".astroimp-project-import-")
            .tempdir_in(data.join("sessions"))
            .unwrap()
            .keep();
        // Design decision: a hard interruption may leave a private staging tree. A new
        // attempt owns a fresh temp directory, not this unverified one or a user-owned folder.
        write(&abandoned.join("project/session.json"), "{}");
        write(&abandoned.join("project/catalog.json"), "{}");
        write(&abandoned.join("project/meta.json"), "{\"truncated\":");
        let unowned = data.join("sessions/.astroimp-project-import-user");
        write(&unowned.join("mine.txt"), "untouched");
        assert!(
            astroimp_desktop::projects::Projects::new(data.clone())
                .unwrap()
                .list()
                .unwrap()
                .is_empty()
        );

        import_projects(&source, &data).unwrap();
        assert_eq!(
            fs::read_to_string(abandoned.join("project/meta.json")).unwrap(),
            "{\"truncated\":"
        );
        assert_eq!(
            fs::read_to_string(unowned.join("mine.txt")).unwrap(),
            "untouched"
        );
        assert!(!data.join("sessions/carina/project/meta.json").exists());
        assert_eq!(
            astroimp_desktop::projects::Projects::new(data)
                .unwrap()
                .list()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn import_rejects_unrelated_and_overlapping_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("app");
        write(&tmp.path().join("other/readme.txt"), "x");
        assert!(import_projects(&tmp.path().join("other"), &data).is_err());
        write(&data.join("sessions/a/session.json"), "x");
        assert!(import_projects(&data, &data).is_err());
        assert!(import_projects(tmp.path(), &data).is_err());
    }

    #[test]
    fn second_lock_on_the_same_folder_is_an_explicit_error() {
        let tmp = tempfile::tempdir().unwrap();
        let first = lock_data_dir(tmp.path()).unwrap();
        let error = lock_data_dir(tmp.path()).err().unwrap().to_string();
        assert!(error.contains("already using"), "{error}");
        drop(first);
        lock_data_dir(tmp.path()).unwrap();
    }

    #[test]
    fn frontend_lookup_needs_an_index_and_names_every_place_tried() {
        let tmp = tempfile::tempdir().unwrap();
        let (empty, built) = (tmp.path().join("empty"), tmp.path().join("built"));
        fs::create_dir_all(&empty).unwrap();
        write(&built.join("index.html"), "<html>");
        assert_eq!(
            find_frontend(&[empty.clone(), built.clone()]).unwrap(),
            built
        );
        let error = find_frontend(std::slice::from_ref(&empty))
            .unwrap_err()
            .to_string();
        assert!(error.contains(&empty.display().to_string()), "{error}");
    }
}
