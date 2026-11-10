use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use half::f16;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
mod publication;
pub use publication::{publish_bundle, recover_publication};

/// Project lookup and the current-project fallback; scoped callers resolve explicitly.
pub struct Projects {
    root: PathBuf,
    current: Option<String>,
}

pub fn read_json(path: &Path, fallback: Value) -> Result<Value> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("Invalid JSON in {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(fallback),
        Err(e) => Err(e).with_context(|| format!("Reading {}", path.display())),
    }
}

/// Replace a file only after its complete contents have been written.
pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("Missing parent directory")?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.flush()?;
    temporary.persist(path).map_err(|e| e.error)?;
    Ok(())
}

pub fn write_json(path: &Path, value: &Value) -> Result<()> {
    write_bytes(path, &serde_json::to_vec_pretty(value)?)
}

/// Create and validate the session's export directory without following a junction outside it.
pub fn export_dir(session: &Path) -> Result<PathBuf> {
    let path = session.join("exports");
    fs::create_dir_all(&path)?;
    let path = path.canonicalize()?;
    if path.parent() != Some(session) {
        bail!("Unsafe export directory");
    }
    Ok(path)
}

pub fn export_file(project: &Path, filename: &str) -> Result<PathBuf> {
    if filename.is_empty()
        || filename.contains(['\\', ':', '\0'])
        || filename
            .split('/')
            .any(|part| part.is_empty() || part.starts_with('.'))
    {
        bail!("Invalid export filename");
    }
    let directory = project
        .join("exports")
        .canonicalize()
        .context("Exports folder not found")?;
    if directory.parent() != Some(project) {
        bail!("Unsafe exports folder");
    }
    let file = directory
        .join(filename)
        .canonicalize()
        .context("Export file not found")?;
    if !file.starts_with(&directory) || !file.is_file() {
        bail!("Invalid export file");
    }
    Ok(file)
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.starts_with('.')
        && !name.contains(['/', '\\', ':', '\0'])
}

/// Imported science references are library-relative; old absolute/session-relative paths still work.
pub fn science_source(session: &Path, source: &str) -> Result<PathBuf> {
    if let Some(relative) = source.strip_prefix("library:") {
        let relative = Path::new(relative);
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            bail!("Invalid library science reference");
        }
        let root = session
            .ancestors()
            .find(|path| path.file_name().is_some_and(|name| name == "sessions"))
            .and_then(Path::parent)
            .context("The project is not inside a library")?;
        return Ok(root.join(relative));
    }
    let path = PathBuf::from(source);
    Ok(if path.is_absolute() {
        path
    } else {
        session.join(path)
    })
}

impl Projects {
    pub(crate) fn trash(&mut self, id: &str) -> Result<Option<String>> {
        let path = self.resolve(id)?;
        crate::storage::files(&self.root.join(id))?;
        let trash = self.root.join(".trash");
        fs::create_dir_all(&trash)?;
        crate::storage::files(&trash)?;
        let destination = trash.join(format!(
            "{id}-{}-{}",
            uuid::Uuid::new_v4().simple(),
            Utc::now().format("%Y%m%dT%H%M%SZ")
        ));
        fs::rename(&path, &destination).context("Moving the project to Trash")?;
        if self.current.as_deref() == Some(id) {
            if let Err(error) = write_json(&self.root.join("current.json"), &json!({"id":null})) {
                fs::rename(&destination, &path).context(
                    "Restoring the project after its current pointer could not be saved",
                )?;
                return Err(error);
            }
            self.current = None;
        }
        Ok(self.current.clone())
    }

    pub fn create(&mut self, name: &str) -> Result<PathBuf> {
        let identifier = format!(
            "{}-{}",
            Utc::now().format("%Y%m%dT%H%M%SZ"),
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let path = self.root.join(&identifier);
        fs::create_dir(&path)?;
        write_json(&path.join("catalog.json"), &json!({"groups":[]}))?;
        write_json(&path.join("selection.json"), &json!({"obs_ids":[]}))?;
        write_json(
            &path.join("session.json"),
            &json!({"name":name,"created":Utc::now().to_rfc3339()}),
        )?;
        self.open(&identifier)
    }

    pub fn new(data: PathBuf) -> Result<Self> {
        let root = data.join("sessions");
        fs::create_dir_all(&root)?;
        let root = root.canonicalize()?;
        let saved = read_json(&root.join("current.json"), json!({}))?;
        let mut projects = Self {
            root,
            current: None,
        };
        if let Some(id) = saved["id"].as_str()
            && projects.resolve(id).is_ok()
        {
            projects.current = Some(id.to_owned());
        }
        Ok(projects)
    }

    pub fn resolve(&self, id: &str) -> Result<PathBuf> {
        let path = self.resolve_readonly(id)?;
        recover_publication(&path)?;
        Ok(path)
    }

    pub fn resolve_readonly(&self, id: &str) -> Result<PathBuf> {
        if !safe_name(id)
            || id.starts_with("_backup_")
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
        {
            bail!("Invalid project ID");
        }
        let path = self
            .root
            .join(id)
            .canonicalize()
            .context("Unknown project")?;
        if path.parent() != Some(self.root.as_path()) || !path.join("catalog.json").is_file() {
            bail!("Unknown project");
        }
        Ok(path)
    }

    pub fn current(&self, id: Option<&str>) -> Result<PathBuf> {
        self.resolve(
            id.or(self.current.as_deref())
                .context("No current session")?,
        )
    }

    pub fn open(&mut self, id: &str) -> Result<PathBuf> {
        let path = self.resolve(id)?;
        write_json(&self.root.join("current.json"), &json!({"id": id}))?;
        self.current = Some(id.to_owned());
        Ok(path)
    }

    pub fn list(&self) -> Result<Vec<Value>> {
        let mut result = Vec::new();
        for item in fs::read_dir(&self.root)? {
            let item = item?;
            let id = item.file_name().to_string_lossy().into_owned();
            let Ok(path) = self.resolve(&id) else {
                continue;
            };
            let saved = read_json(&path.join("session.json"), json!({}))?;
            let catalog = read_json(&path.join("catalog.json"), json!({}))?;
            let view = read_json(&path.join("view.json"), json!({}))?;
            let selection = read_json(&path.join("selection.json"), json!({}))?;
            let groups = catalog["groups"].as_array().cloned().unwrap_or_default();
            let observations = groups
                .iter()
                .map(|g| g["observations"].as_array().map_or(0, Vec::len))
                .sum::<usize>();
            let hint = groups
                .iter()
                .flat_map(|g| g["targets"].as_array().into_iter().flatten())
                .find_map(Value::as_str)
                .unwrap_or("");
            let name = saved["name"]
                .as_str()
                .unwrap_or(if hint.is_empty() { &id } else { hint });
            let modified = [
                "edits.json",
                "selection.json",
                "session.json",
                "catalog.json",
            ]
            .iter()
            .filter_map(|n| fs::metadata(path.join(n)).ok()?.modified().ok())
            .max();
            let updated = modified
                .map(|t| DateTime::<Utc>::from(t).to_rfc3339())
                .unwrap_or_default();
            let created = saved["created"]
                .as_str()
                .or(view["captured_at_utc"].as_str())
                .unwrap_or(&updated);
            result.push(
                json!({"id":id,"name":name,"created":created,"updated":updated,
                "observations":observations,"target_hint":hint,"groups":groups.len(),
                "selected":selection["obs_ids"].as_array().map_or(0, Vec::len),
                "built":path.join("project/meta.json").is_file()}),
            );
        }
        result.sort_by(|a, b| b["updated"].as_str().cmp(&a["updated"].as_str()));
        Ok(result)
    }
}

/// Ignore abandoned star-only state while retaining ordinary edits and checkpoints.
pub fn remove_star_state(state: &mut Value) {
    if let Some(object) = state.as_object_mut() {
        for key in [
            "starOverrides",
            "starWeights",
            "starClasses",
            "starsMigration",
        ] {
            object.remove(key);
        }
    }
    if let Some(global) = state.get_mut("global").and_then(Value::as_object_mut) {
        global.remove("stars");
        global.remove("parallax");
    }
    if let Some(checkpoints) = state.get_mut("checkpoints").and_then(Value::as_array_mut) {
        for checkpoint in checkpoints {
            if let Some(inner) = checkpoint.get_mut("state") {
                remove_star_state(inner);
            }
        }
    }
}

pub fn project_meta(session: &Path) -> Result<Value> {
    let project = session.join("project");
    if !project.exists() {
        return Ok(Value::Null);
    }
    let project = project.canonicalize()?;
    if project.parent() != Some(session) {
        bail!("Unsafe project directory");
    }
    let meta_path = project.join("meta.json");
    if meta_path.exists() && meta_path.canonicalize()?.parent() != Some(project.as_path()) {
        bail!("Unsafe project metadata");
    }
    let mut meta = read_json(&meta_path, Value::Null)?;
    if meta.is_null() {
        return Ok(meta);
    }
    if !meta.is_object() {
        bail!("Invalid project metadata");
    }
    let layers = meta
        .get_mut("layers")
        .and_then(Value::as_array_mut)
        .context("Project metadata lacks layers")?;
    for layer in layers {
        let file = layer["file"]
            .as_str()
            .context("Layer missing file")?
            .to_owned();
        if !safe_name(&file) {
            bail!("Unsafe layer filename");
        }
        layer
            .as_object_mut()
            .context("Invalid layer metadata")?
            .remove("starless");
    }
    if let Some(lum) = meta.get_mut("luminance").filter(|v| v.is_object()) {
        let file = lum["file"]
            .as_str()
            .context("Luminance missing file")?
            .to_owned();
        if !safe_name(&file) {
            bail!("Unsafe luminance filename");
        }
        lum.as_object_mut()
            .context("Invalid luminance metadata")?
            .remove("starless");
    }
    meta.as_object_mut()
        .context("Invalid project metadata")?
        .remove("stars");
    Ok(meta)
}

pub fn edits(session: &Path) -> Result<Value> {
    let mut state = read_json(&session.join("edits.json"), Value::Null)?;
    remove_star_state(&mut state);
    Ok(state)
}

pub fn payload(session: &Path) -> Result<Value> {
    let id = session
        .file_name()
        .context("Missing project ID")?
        .to_string_lossy();
    let settings = read_json(&session.join("session.json"), json!({}))?;
    let view = read_json(&session.join("view.json"), json!({}))?;
    let catalog = read_json(&session.join("catalog.json"), json!({}))?;
    let meta = project_meta(session)?;
    let rebuild_required = !meta.is_null()
        && meta["view_rotation_deg"].as_f64().unwrap_or(0.)
            != view["rotation_deg"].as_f64().unwrap_or(0.);
    let name = settings["name"].as_str().unwrap_or(&id);
    Ok(json!({"id":id,"name":name,"view":view,"catalog":catalog,
        "selection":read_json(&session.join("selection.json"),json!({"obs_ids":[]}))?,
        "triage":read_json(&session.join("triage.json"),json!({"favorites":[],"dismissed":[]}))?,
        "meta":meta, "edits":edits(session)?,
        "build":{"mosaic":settings["build"]["mosaic"].as_bool().unwrap_or(true)},
        "build_version":crate::native::BUILD_VERSION,
        "rebuild_required":rebuild_required}))
}

pub fn save_edits(session: &Path, mut state: Value) -> Result<u64> {
    if !state.is_object() {
        bail!("Expected an edits object");
    }
    remove_star_state(&mut state);
    let revision = read_json(&session.join("edits.rev.json"), json!({}))?["revision"]
        .as_u64()
        .unwrap_or(0)
        + 1;
    write_json(&session.join("edits.json"), &state)?;
    write_json(
        &session.join("edits.rev.json"),
        &json!({"revision":revision,"source":"browser","note":""}),
    )?;
    Ok(revision)
}

/// Read only supported, metadata-declared project assets.
pub fn project_file(session: &Path, file: &str) -> Result<Vec<u8>> {
    if !safe_name(file) {
        bail!("Unknown project file");
    }
    let meta = project_meta(session)?;
    let width = u32::try_from(meta["width"].as_u64().context("Missing width")?)? as usize;
    let height = u32::try_from(meta["height"].as_u64().context("Missing height")?)? as usize;
    let count = width
        .checked_mul(height)
        .context("Project dimensions overflow")?;
    let plane_bytes = count
        .checked_mul(2)
        .context("Project dimensions overflow")?;
    if file == "meta.json" {
        return Ok(serde_json::to_vec(&meta)?);
    }
    let layers = meta["layers"].as_array().context("Missing layers")?;
    let mut allowed = false;
    let mut edge_sources: Option<Vec<&Value>> = None;
    for layer in layers {
        let key = layer["key"].as_str().unwrap_or("");
        let edge = layer["edge_file"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{key}.edge.f16"));
        if file == edge {
            allowed = true;
            edge_sources = Some(vec![layer]);
        }
        if layer["file"].as_str() == Some(file) || layer["coverage_file"].as_str() == Some(file) {
            allowed = true;
        }
    }
    let lum = &meta["luminance"];
    if lum["file"].as_str() == Some(file) {
        allowed = true;
    }
    let lum_edge = lum["edge_file"].as_str().unwrap_or("luminance.edge.f16");
    if file == lum_edge && !layers.is_empty() {
        allowed = true;
        edge_sources = Some(layers.iter().collect());
    }
    if !allowed {
        bail!("Unknown project file");
    }
    let folder = session.join("project").canonicalize()?;
    let path = folder.join(file);
    if path.exists() {
        let actual = path.canonicalize()?;
        if actual.parent() != Some(folder.as_path()) {
            bail!("Unsafe project file");
        }
        let bytes = fs::read(actual)?;
        if edge_sources.is_none() || bytes.len() == plane_bytes {
            return Ok(bytes);
        }
    }
    if let Some(sources) = edge_sources {
        return edge_data(&folder, width, height, &sources);
    }
    bail!("Project file unavailable: {file}")
}

// Share the renderer's exact distance transform to keep HTTP and CPU edges consistent.
fn edge_data(folder: &Path, w: usize, h: usize, sources: &[&Value]) -> Result<Vec<u8>> {
    let n = w.checked_mul(h).context("Invalid dimensions")?;
    if n == 0 {
        bail!("Invalid dimensions");
    }
    let mut covered = vec![false; n];
    for source in sources {
        let key = source["key"].as_str().unwrap_or("");
        let name = source["coverage_file"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{key}.coverage.u8"));
        if !safe_name(&name) {
            bail!("Unsafe coverage filename");
        }
        let path = folder.join(name);
        if path.exists() {
            let actual = path.canonicalize()?;
            if actual.parent() != Some(folder) {
                bail!("Unsafe coverage file");
            }
            let data = fs::read(actual)?;
            if data.len() != n {
                bail!("Invalid coverage dimensions");
            }
            for (c, byte) in covered.iter_mut().zip(data) {
                *c |= byte != 0;
            }
        } else {
            // Legacy projects may lack a coverage plane: finite science is coverage,
            // not a fabricated all-covered image.
            let science = source["file"].as_str().context("Layer missing file")?;
            if !safe_name(science) {
                bail!("Unsafe layer filename");
            }
            let actual = folder.join(science).canonicalize()?;
            if actual.parent() != Some(folder) {
                bail!("Unsafe science file");
            }
            let data = fs::read(actual)?;
            if data.len() != n * 2 {
                bail!("Invalid science dimensions");
            }
            for (c, bytes) in covered.iter_mut().zip(data.as_chunks::<2>().0.iter()) {
                *c |= f16::from_bits(u16::from_le_bytes(*bytes)).is_finite();
            }
        }
    }
    let distances = crate::planes::edge_distance(&covered, w as u32, h as u32, &|| false)?;
    Ok(distances
        .into_iter()
        .flat_map(|v| f16::from_f32(v).to_bits().to_le_bytes())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn science_references_survive_a_library_move_and_keep_legacy_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("first");
        fs::create_dir_all(first.join("sessions/a")).unwrap();
        fs::create_dir_all(first.join("downloads")).unwrap();
        fs::write(first.join("downloads/source.fits"), b"science").unwrap();
        let reference = "library:downloads\\source.fits";
        assert_eq!(
            fs::read(science_source(&first.join("sessions/a"), reference).unwrap()).unwrap(),
            b"science"
        );
        let moved = tmp.path().join("moved");
        fs::rename(&first, &moved).unwrap();
        let session = moved.join("sessions/a");
        assert_eq!(
            fs::read(science_source(&session, reference).unwrap()).unwrap(),
            b"science"
        );
        let absolute = moved.join("downloads/source.fits");
        assert_eq!(
            science_source(&session, absolute.to_str().unwrap()).unwrap(),
            absolute
        );
        assert_eq!(
            science_source(&session, "old.fits").unwrap(),
            session.join("old.fits")
        );
        for invalid in [
            "library:",
            "library:..\\outside.fits",
            "library:C:\\outside.fits",
        ] {
            assert!(science_source(&session, invalid).is_err(), "{invalid}");
        }
    }

    fn fixture() -> (tempfile::TempDir, Projects) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        for id in ["one", "two"] {
            let path = root.join(id);
            std::fs::create_dir_all(path.join("project")).unwrap();
            write_json(&path.join("catalog.json"), &json!({"groups": []})).unwrap();
            write_json(
                &path.join("project/meta.json"),
                &json!({
                    "width": 2, "height": 2,
                    "layers": [{"key":"a", "file":"a.f16", "starless":"a.starless.f16"}],
                    "stars":{"file":"stars.u16","list":"stars.json","detect":true}
                }),
            )
            .unwrap();
        }
        let projects = Projects::new(temp.path().to_path_buf()).unwrap();
        (temp, projects)
    }

    #[test]
    fn scoped_saves_and_revisions_do_not_follow_current_project() {
        let (_temp, mut projects) = fixture();
        projects.open("one").unwrap();
        projects.open("two").unwrap();
        let one = projects.resolve("one").unwrap();
        let two = projects.resolve("two").unwrap();
        let rev = save_edits(
            &one,
            json!({"layers": [], "global":{"stars":{"separate":true}}, "checkpoints":[1]}),
        )
        .unwrap();
        assert_eq!(rev, 1);
        assert!(!two.join("edits.json").exists());
        assert_eq!(
            read_json(&one.join("edits.json"), Value::Null).unwrap()["checkpoints"],
            json!([1])
        );
        assert_eq!(
            read_json(&one.join("edits.json"), Value::Null).unwrap()["global"]["stars"],
            Value::Null
        );
        assert_eq!(
            save_edits(&one, json!({"layers": [], "global":{}})).unwrap(),
            2
        );
    }

    #[test]
    fn legacy_star_assets_are_ignored_and_paths_are_restricted() {
        let (_temp, projects) = fixture();
        assert!(projects.resolve("../two").is_err());
        assert!(projects.resolve("one\\..\\two").is_err());
        let path = projects.resolve("one").unwrap();
        let meta = project_meta(&path).unwrap();
        assert!(meta.get("stars").is_none());
        assert!(meta["layers"][0].get("starless").is_none());
        assert!(project_file(&path, "stars.u16").is_err());
        assert!(project_file(&path, "../catalog.json").is_err());
        assert!(project_file(&path, "a.starless.f16").is_err());
        assert_eq!(
            read_json(&path.join("project/meta.json"), Value::Null).unwrap()["stars"]["detect"],
            true
        );
    }

    #[test]
    fn missing_edge_uses_finite_science_coverage() {
        let (_temp, projects) = fixture();
        let path = projects.resolve("one").unwrap();
        let meta = json!({"width":3,"height":2,
            "layers":[{"key":"a","file":"a.f16"}],"stars":{}});
        write_json(&path.join("project/meta.json"), &meta).unwrap();
        let pixels = [f32::NAN, 1., 1., 1., 1., 1.];
        let data: Vec<u8> = pixels
            .into_iter()
            .flat_map(|x| f16::from_f32(x).to_bits().to_le_bytes())
            .collect();
        write_bytes(&path.join("project/a.f16"), &data).unwrap();
        let bytes = project_file(&path, "a.edge.f16").unwrap();
        let distance: Vec<f32> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| f16::from_bits(u16::from_le_bytes(*p)).to_f32())
            .collect();
        assert_eq!(distance[0], 0.);
        assert_eq!(distance[1], 1.);
        assert!((distance[4] - 2_f32.sqrt()).abs() < 0.001);
    }
}
