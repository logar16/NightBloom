//! Explicit, reference-checked cleanup; never follow links or infer missing manifests.
use crate::projects::read_json;
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

pub fn files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(result),
        Err(error) => return Err(error).with_context(|| format!("Reading {}", root.display())),
    };
    let linked = metadata.file_type().is_symlink();
    #[cfg(windows)]
    let linked = {
        use std::os::windows::fs::MetadataExt;
        linked || metadata.file_attributes() & 0x400 != 0
    };
    ensure!(
        !linked,
        "Linked library paths cannot be cleaned: {}",
        root.display()
    );
    if metadata.is_dir() {
        for entry in fs::read_dir(root)? {
            result.extend(files(&entry?.path())?);
        }
    } else {
        result.push(root.to_owned());
    }
    Ok(result)
}

fn bytes(paths: &[PathBuf]) -> Result<u64> {
    paths
        .iter()
        .try_fold(0, |sum, path| Ok(sum + fs::metadata(path)?.len()))
}

fn key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

fn manifest(path: &Path) -> Result<Value> {
    let value = read_json(path, json!({}))?;
    ensure!(
        value.is_object(),
        "Invalid reference manifest: {}",
        path.display()
    );
    Ok(value)
}

fn array<'a>(value: &'a Value, name: &str) -> Result<&'a [Value]> {
    match value.get(name) {
        None => Ok(&[]),
        Some(Value::Array(values)) => Ok(values),
        _ => anyhow::bail!("Invalid {name} in a reference manifest."),
    }
}

#[derive(Default)]
struct References {
    paths: BTreeMap<String, Value>,
    names: BTreeMap<String, Value>,
}

impl References {
    fn add(&mut self, source: &str, category: &str, reference: &Value) {
        let value = source.replace('\\', "/").to_lowercase();
        let (table, name) = if let Some(relative) = value.strip_prefix("library:") {
            (&mut self.paths, relative.to_owned())
        } else {
            (
                &mut self.names,
                value.rsplit('/').next().unwrap_or("").to_owned(),
            )
        };
        let refs = table.entry(name).or_insert_with(|| {
            json!({
                "live":[],"trash":[],"backups":[],"fixtures":[]
            })
        });
        let list = refs[category].as_array_mut().unwrap();
        if !list.contains(reference) {
            list.push(reference.clone());
        }
    }

    fn metadata(&mut self, value: &Value, category: &str, reference: &Value) {
        match value {
            Value::String(source) => self.add(source, category, reference),
            Value::Array(values) => values
                .iter()
                .for_each(|v| self.metadata(v, category, reference)),
            Value::Object(values) => values
                .values()
                .for_each(|v| self.metadata(v, category, reference)),
            _ => {}
        }
    }

    fn for_file(&self, root: &Path, path: &Path) -> Value {
        let mut result = json!({"live":[],"trash":[],"backups":[],"fixtures":[]});
        for references in [
            self.paths.get(&key(path.strip_prefix(root).unwrap())),
            self.names
                .get(&path.file_name().unwrap().to_string_lossy().to_lowercase()),
        ]
        .into_iter()
        .flatten()
        {
            for category in ["live", "trash", "backups", "fixtures"] {
                for reference in references[category].as_array().unwrap() {
                    let list = result[category].as_array_mut().unwrap();
                    if !list.contains(reference) {
                        list.push(reference.clone());
                    }
                }
            }
        }
        result
    }
}

struct Scan {
    inventory: Value,
    references: References,
}

fn scan(root: &Path) -> Result<Scan> {
    let mut references = References::default();
    let mut totals: BTreeMap<&str, u64> = [
        "downloads",
        "aligned",
        "previews",
        "sessions",
        "trash",
        "backups",
        "fixtures",
        "exports",
        "runtime",
    ]
    .into_iter()
    .map(|name| (name, 0))
    .collect();
    let mut trash = Vec::new();
    files(&root.join("sessions"))?;
    files(&root.join("test-fixtures"))?;
    let mut roots = Vec::new();
    for (parent, category) in [
        (root.join("sessions"), "live"),
        (root.join("test-fixtures/sessions"), "fixtures"),
        (root.join("sessions/.trash"), "trash"),
    ] {
        if !parent.exists() {
            continue;
        }
        for entry in fs::read_dir(&parent)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() && !entry.file_type()?.is_symlink() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            if id.starts_with('.') {
                continue;
            }
            let category = if category == "live" && id.starts_with("_backup_") {
                "backups"
            } else {
                category
            };
            roots.push((entry.path(), category));
        }
    }
    if root.join("test-fixtures").is_dir() && !root.join("test-fixtures/sessions").exists() {
        roots.push((root.join("test-fixtures"), "fixtures"));
    }
    let mut missing = BTreeSet::new();
    for (folder, category) in roots {
        let id = folder.file_name().unwrap().to_string_lossy().into_owned();
        let manifests = files(&folder)?;
        let settings = manifest(&folder.join("session.json"))?;
        let reference = json!({"id":id,"name":settings["name"].as_str().unwrap_or(&id)});
        let mut observations = BTreeMap::new();
        for path in &manifests {
            if path.file_name().is_some_and(|n| n == "catalog.json") {
                let catalog = manifest(path)?;
                for group in array(&catalog, "groups")? {
                    for observation in array(group, "observations")? {
                        let id = observation["obs_id"]
                            .as_str()
                            .context("Catalog observation has no ID.")?;
                        observations.insert(id.to_owned(), observation.clone());
                    }
                }
            }
        }
        for path in &manifests {
            if path.file_name().is_some_and(|n| n == "meta.json") {
                let value = manifest(path)?;
                array(&value, "layers")?;
                references.metadata(&value, category, &reference);
                missing_sources(
                    &value,
                    path.parent().and_then(Path::parent).unwrap(),
                    &mut missing,
                )?;
            } else if path.file_name().is_some_and(|n| n == "selection.json") {
                let selection = manifest(path)?;
                for selected in array(&selection, "obs_ids")? {
                    let observation = selected
                        .as_str()
                        .and_then(|id| observations.get(id))
                        .context(
                            "Cannot verify a selected source missing from its project catalog.",
                        )?;
                    if let Some(survey) = crate::discovery::survey_id(observation) {
                        let slug = survey.replace('/', "-");
                        ensure!(
                            !slug.contains(['\\', ':']) && !slug.starts_with('.'),
                            "Invalid survey source ID."
                        );
                        let folder = root.join("downloads/surveys").join(slug);
                        for source in files(&folder)? {
                            references.add(
                                &format!("library:{}", key(source.strip_prefix(root)?)),
                                category,
                                &reference,
                            );
                        }
                    } else {
                        for name in ["data_uri", "filename", "source"] {
                            if let Some(value) = observation[name].as_str() {
                                references.add(value, category, &reference);
                            }
                        }
                    }
                }
            }
        }
        let size = bytes(&manifests)?;
        let exports = if category == "live" {
            bytes(
                &manifests
                    .iter()
                    .filter(|p| p.starts_with(folder.join("exports")))
                    .cloned()
                    .collect::<Vec<_>>(),
            )?
        } else {
            0
        };
        *totals.entry("exports").or_default() += exports;
        let total_key = if category == "live" {
            "sessions"
        } else {
            category
        };
        *totals.entry(total_key).or_default() += size - exports;
        if category == "trash" {
            let date = id
                .rsplit('-')
                .next()
                .and_then(|stamp| {
                    chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%SZ").ok()
                })
                .map(|date| date.and_utc())
                .unwrap_or(DateTime::<Utc>::from(fs::metadata(&folder)?.modified()?));
            trash.push(
                json!({"id":id,"name":reference["name"],"size":size,"date":date.to_rfc3339()}),
            );
        }
    }
    let downloads = files(&root.join("downloads"))?.into_iter().map(|path| {
        let refs = references.for_file(root, &path);
        let in_use = ["live", "backups", "fixtures"].iter().any(|c| !refs[c].as_array().unwrap().is_empty());
        let status = if in_use { "in_use" } else if !refs["trash"].as_array().unwrap().is_empty() { "trash_only" } else { "orphan" };
        Ok(json!({
            "path":path.strip_prefix(root.join("downloads"))?.to_string_lossy().replace('\\',"/"),
            "name":path.file_name().unwrap().to_string_lossy(),"size":fs::metadata(&path)?.len(),
            "status":status,"references":refs,
        }))
    }).collect::<Result<Vec<Value>>>()?;
    totals.insert(
        "downloads",
        downloads.iter().map(|v| v["size"].as_u64().unwrap()).sum(),
    );
    for (category, folder) in [
        ("aligned", "aligned_cache"),
        ("previews", "cache"),
        ("runtime", ".runtime"),
    ] {
        totals.insert(category, bytes(&files(&root.join(folder))?)?);
    }
    let total: u64 = totals.values().sum();
    Ok(Scan {
        references,
        inventory: json!({
            "total":total,"totals":totals,"downloads":downloads,"trash":trash,"backups":[],"can_cleanup":true,
            "missing_sources":missing.len()
        }),
    })
}

fn missing_sources(value: &Value, session: &Path, missing: &mut BTreeSet<PathBuf>) -> Result<()> {
    match value {
        Value::Object(items) => {
            if let Some(source) = items.get("source").and_then(Value::as_str) {
                let path = crate::projects::science_source(session, source)?;
                if !path.is_file() {
                    missing.insert(path);
                }
            }
            for value in items.values() {
                missing_sources(value, session, missing)?;
            }
        }
        Value::Array(items) => {
            for value in items {
                missing_sources(value, session, missing)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn inventory(root: &Path) -> Result<Value> {
    Ok(scan(root)?.inventory)
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delete {
    #[serde(default)]
    pub downloads: Vec<String>,
    #[serde(default)]
    pub trash: Vec<String>,
    #[serde(default)]
    pub backups: Vec<String>,
    #[serde(default)]
    pub caches: Vec<String>,
}

pub fn delete(root: &Path, request: Delete) -> Result<Value> {
    ensure!(
        request.backups.is_empty(),
        "Backups and build history are protected."
    );
    let snapshot = scan(root)?;
    let mut targets = BTreeSet::new();
    for name in request.downloads {
        let item = snapshot.inventory["downloads"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["path"].as_str() == Some(&name))
            .context("Unknown download.")?;
        ensure!(
            item["status"] != "in_use",
            "A live project, backup or test fixture uses this download."
        );
        targets.insert(root.join("downloads").join(name));
    }
    for name in request.trash {
        ensure!(
            snapshot.inventory["trash"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"].as_str() == Some(&name)),
            "Unknown deleted project."
        );
        targets.insert(root.join("sessions/.trash").join(name));
    }
    for name in request.caches {
        let folder = match name.as_str() {
            "aligned" => "aligned_cache",
            "previews" => "cache",
            _ => anyhow::bail!("Unknown cache."),
        };
        let target = root.join(folder);
        for path in files(&target)? {
            let refs = snapshot.references.for_file(root, &path);
            ensure!(
                refs.as_object()
                    .unwrap()
                    .values()
                    .all(|v| v.as_array().unwrap().is_empty()),
                "A project references this cache; it cannot be cleaned."
            );
        }
        targets.insert(target);
    }
    let mut freed = 0;
    for path in &targets {
        freed += bytes(&files(path)?)?;
    }
    for path in targets {
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else if path.is_file() {
            fs::remove_file(&path)?;
        }
    }
    Ok(json!({"freed_bytes":freed}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::write_json;

    #[test]
    fn live_archived_and_fixture_sources_protect_whole_batches_and_trash_is_explicit() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        fs::create_dir_all(root.join("downloads")).unwrap();
        fs::write(root.join("downloads/used.fits"), b"used").unwrap();
        fs::write(root.join("downloads/old.fits"), b"old").unwrap();
        for (folder, source) in [
            ("sessions/live/builds/before/project", "used.fits"),
            ("test-fixtures/sessions/frozen/project", "used.fits"),
            ("sessions/.trash/gone/project", "old.fits"),
        ] {
            fs::create_dir_all(root.join(folder)).unwrap();
            write_json(
                &root.join(folder).join("meta.json"),
                &json!({"layers":[{"source":source}]}),
            )
            .unwrap();
        }
        let inventory = inventory(root).unwrap();
        let downloads = inventory["downloads"].as_array().unwrap();
        assert_eq!(
            downloads.iter().find(|d| d["name"] == "used.fits").unwrap()["status"],
            "in_use"
        );
        assert_eq!(
            downloads.iter().find(|d| d["name"] == "old.fits").unwrap()["status"],
            "trash_only"
        );
        assert!(
            delete(
                root,
                Delete {
                    downloads: vec!["old.fits".into(), "used.fits".into()],
                    ..Delete::default()
                }
            )
            .is_err()
        );
        assert!(root.join("downloads/old.fits").exists());
        let freed = delete(
            root,
            Delete {
                downloads: vec!["old.fits".into()],
                trash: vec!["gone".into()],
                ..Delete::default()
            },
        )
        .unwrap();
        assert!(freed["freed_bytes"].as_u64().unwrap() > 3);
        assert!(root.join("downloads/used.fits").exists());
        assert!(!root.join("sessions/.trash/gone").exists());
    }

    #[test]
    fn malformed_references_and_escape_paths_cannot_turn_sources_into_orphans() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("sessions/live")).unwrap();
        fs::write(root.path().join("sessions/live/selection.json"), b"{bad").unwrap();
        assert!(inventory(root.path()).is_err());
        fs::write(
            root.path().join("sessions/live/selection.json"),
            br#"{"obs_ids":"not-an-array"}"#,
        )
        .unwrap();
        assert!(inventory(root.path()).is_err());
        fs::remove_file(root.path().join("sessions/live/selection.json")).unwrap();
        for name in [
            "../sessions/live",
            "..\\sessions\\live",
            "C:\\outside.fits",
            "x:stream",
        ] {
            assert!(
                delete(
                    root.path(),
                    Delete {
                        downloads: vec![name.into()],
                        ..Delete::default()
                    }
                )
                .is_err()
            );
        }
        assert!(root.path().join("sessions/live").is_dir());
    }

    #[test]
    fn selected_survey_folders_and_direct_fixture_sources_survive_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        for survey in ["surveys/CDS-P-allWISE-W1", "surveys/MDW-Halpha/fields-v1"] {
            fs::create_dir_all(root.join("downloads").join(survey)).unwrap();
            fs::write(
                root.join("downloads").join(survey).join("old-grid.fits"),
                b"survey",
            )
            .unwrap();
        }
        fs::create_dir_all(root.join("sessions/live/project")).unwrap();
        fs::create_dir_all(root.join("test-fixtures/project")).unwrap();
        fs::create_dir_all(root.join("cache")).unwrap();
        fs::write(root.join("downloads/frozen.fits"), b"fixture").unwrap();
        fs::write(root.join("cache/source.fits"), b"source").unwrap();
        write_json(
            &root.join("sessions/live/catalog.json"),
            &json!({"groups":[{"observations":[
                {"obs_id":"wise","hips_id":"CDS/P/allWISE/W1"},
                {"obs_id":"mdw","survey_id":"MDW/Halpha","hips_id":null}
            ]}]}),
        )
        .unwrap();
        write_json(
            &root.join("sessions/live/selection.json"),
            &json!({"obs_ids":["wise","mdw"]}),
        )
        .unwrap();
        write_json(
            &root.join("sessions/live/project/meta.json"),
            &json!({"layers":[{"source":"library:cache/source.fits"}]}),
        )
        .unwrap();
        write_json(
            &root.join("test-fixtures/project/meta.json"),
            &json!({"layers":[{"source":"frozen.fits"}]}),
        )
        .unwrap();
        let value = inventory(root).unwrap();
        assert!(
            value["downloads"]
                .as_array()
                .unwrap()
                .iter()
                .all(|d| d["status"] == "in_use")
        );
        assert!(
            delete(
                root,
                Delete {
                    caches: vec!["previews".into()],
                    ..Delete::default()
                }
            )
            .is_err()
        );
        assert!(root.join("cache/source.fits").is_file());
    }
}
