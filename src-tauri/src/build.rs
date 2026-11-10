use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::Path,
};

use crate::{
    discovery::Archive,
    grid,
    jobs::JobHandle,
    native::{self, BuildSource},
    projects,
};

pub fn observations(session: &Path) -> Result<HashMap<String, Value>> {
    let catalog = projects::read_json(&session.join("catalog.json"), json!({"groups":[]}))?;
    Ok(catalog["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|group| group["observations"].as_array().into_iter().flatten())
        .filter_map(|row| {
            row["obs_id"]
                .as_str()
                .map(|id| (id.to_owned(), row.clone()))
        })
        .collect())
}

fn selected(session: &Path) -> Result<Vec<Value>> {
    let available = observations(session)?;
    let selection = projects::read_json(&session.join("selection.json"), json!({"obs_ids":[]}))?;
    let identifiers = selection["obs_ids"]
        .as_array()
        .context("Invalid observation selection")?;
    let identifiers = identifiers
        .iter()
        .map(|id| id.as_str().context("Invalid observation ID"))
        .collect::<Result<BTreeSet<_>>>()?;
    identifiers
        .into_iter()
        .map(|id| {
            available
                .get(id)
                .cloned()
                .context("Selection contains unknown observations.")
        })
        .collect()
}

fn merge_enabled(session: &Path) -> Result<bool> {
    Ok(
        projects::read_json(&session.join("session.json"), json!({}))?["build"]["mosaic"]
            .as_bool()
            .unwrap_or(true),
    )
}

fn groups(observations: &[Value], merge: bool) -> Result<Vec<Vec<Value>>> {
    let mut groups: Vec<Vec<Value>> = Vec::new();
    let mut indexes = HashMap::new();
    for observation in observations {
        let proposal = observation
            .get("proposal")
            .filter(|value| !value.is_null() && value.as_str() != Some(""))
            .or_else(|| {
                observation
                    .get("proposal_id")
                    .filter(|value| !value.is_null() && value.as_str() != Some(""))
            });
        let key =
            if merge && proposal.is_some() && crate::discovery::survey_id(observation).is_none() {
                serde_json::to_string(&json!([
                    observation["collection"],
                    observation["instrument"],
                    observation["filter"],
                    proposal
                ]))?
            } else {
                serde_json::to_string(&observation["obs_id"])?
            };
        let index = *indexes.entry(key).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[index].push(observation.clone());
    }
    let mut keys = HashSet::new();
    for group in &groups {
        ensure!(
            keys.insert(native::layer_key(group)?),
            "Observation IDs collide on disk."
        );
    }
    Ok(groups)
}

fn locked(session: &Path, view: &Value) -> Result<Option<Value>> {
    let meta = projects::project_meta(session)?;
    if !meta.is_null()
        && meta["view_rotation_deg"].as_f64().unwrap_or(0.)
            != view["rotation_deg"].as_f64().unwrap_or(0.)
    {
        return Ok(None);
    }
    let saved = projects::read_json(&session.join("grid.json"), Value::Null)?;
    if !saved.is_null() {
        return Ok(Some(grid::from_meta(&saved)));
    }
    Ok((!meta.is_null() && session.join("edits.json").exists()).then(|| grid::from_meta(&meta)))
}

fn candidate_grid(
    session: &Path,
    view: &Value,
    observations: &[Value],
    inputs: &[Value],
) -> Result<Value> {
    if let Some(grid) = locked(session, view)? {
        return Ok(grid);
    }
    if !inputs.is_empty()
        && view["source"] != "coordinates"
        && view["rotation_deg"].as_f64().unwrap_or(0.) == 0.
    {
        return grid::union(inputs, 3072);
    }
    let scale = inputs
        .iter()
        .filter_map(|input| input["pixel_scale_arcsec"].as_f64())
        .chain(
            observations
                .iter()
                .filter_map(|row| row["native_scale_arcsec"].as_f64()),
        )
        .fold(f64::INFINITY, f64::min);
    grid::view_grid(view, if scale.is_finite() { scale } else { 1. }, 3072)
}

pub fn plan(session: &Path, archive: &Archive) -> Result<Value> {
    let observations = selected(session)?;
    let merge = merge_enabled(session)?;
    let groups = groups(&observations, merge)?;
    let view = projects::read_json(&session.join("view.json"), json!({}))?;
    let locked = locked(session, &view)?;
    if observations.is_empty() {
        return Ok(
            json!({"files":[],"total_bytes":0,"cached_bytes":0,"layer_count":0,"grid_locked":locked.is_some(),"grid":locked}),
        );
    }
    let provisional = candidate_grid(session, &view, &observations, &[])?;
    let mut inputs = Vec::new();
    for observation in &observations {
        if crate::discovery::survey_id(observation).is_none()
            && let Some(path) = archive.cached_source(observation, &provisional)?
        {
            inputs.push(native::source_metadata(&path)?);
        }
    }
    let grid = candidate_grid(session, &view, &observations, &inputs)?;
    let mut files = Vec::new();
    let (mut total, mut cached_bytes) = (0_u64, 0_u64);
    for observation in &observations {
        let cached = archive.cached_source(observation, &grid)?;
        let size = match &cached {
            Some(path) => Some(fs::metadata(path)?.len()),
            None => observation["size_bytes"].as_u64(),
        };
        let name = cached
            .as_deref()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            .map(str::to_owned)
            .or_else(|| {
                observation["data_uri"]
                    .as_str()
                    .and_then(|uri| uri.rsplit('/').next())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| {
                format!("{}.fits", observation["obs_id"].as_str().unwrap_or("image"))
            });
        if let Some(size) = size {
            total = total.saturating_add(size);
            if cached.is_some() {
                cached_bytes = cached_bytes.saturating_add(size);
            }
        }
        files.push(json!({"obs_id":observation["obs_id"],"filter":observation["filter"],"filename":name,"size_bytes":size,"cached":cached.is_some(),
            "transfer_bytes":if cached.is_some() { Some(0) } else if crate::discovery::survey_id(observation).is_some() { None } else { size },
            "transfer_estimated":false,"transferred_bytes":0}));
    }

    Ok(
        json!({"files":files,"total_bytes":total,"cached_bytes":cached_bytes,"layer_count":groups.len(),"grid_locked":locked.is_some(),"grid":grid}),
    )
}

pub async fn transfer_plan(session: &Path, archive: &Archive, plan: &mut Value) -> Result<()> {
    let observations = observations(session)?;
    let mut tasks = tokio::task::JoinSet::new();
    for file in plan["files"].as_array().context("Invalid build plan")? {
        let observation = observations
            .get(file["obs_id"].as_str().context("Missing observation ID")?)
            .context("Missing planned observation")?;
        if file["cached"] == true || crate::discovery::survey_id(observation).is_none() {
            continue;
        }
        let archive = archive.clone();
        let observation = observation.clone();
        let grid = plan["grid"].clone();
        tasks.spawn(async move {
                let transfer = match tokio::time::timeout(std::time::Duration::from_secs(22), archive.survey_transfer(&observation, &grid)).await {
                    Ok(Ok(transfer)) => transfer,
                    Ok(Err(error)) => json!({"transfer_bytes":null,"transfer_estimated":false,"transfer_note":format!("Transfer estimate unavailable: {error}")}),
                    Err(_) => json!({"transfer_bytes":null,"transfer_estimated":false,"transfer_note":"Transfer metadata timed out; the download size is still unknown."}),
                };
                (observation["obs_id"].clone(), transfer)
            });
    }
    while let Some(result) = tasks.join_next().await {
        let (id, transfer) = result?;
        if let Some(file) = plan["files"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|file| file["obs_id"] == id)
        {
            for (key, value) in transfer.as_object().context("Invalid transfer estimate")? {
                file[key] = value.clone();
            }
        }
    }
    Ok(())
}

pub struct Prepared {
    stage: tempfile::TempDir,
    meta: Value,
    updates: Vec<(&'static str, Option<Vec<u8>>)>,
}

impl Prepared {
    pub fn publish(self, session: &Path) -> Result<Value> {
        projects::publish_bundle(
            session,
            &self.updates,
            Some(&self.stage.path().join("project")),
            true,
        )?;
        Ok(self.meta)
    }
}

fn bytes(value: &Value) -> Result<Option<Vec<u8>>> {
    if value.is_null() {
        Ok(None)
    } else {
        Ok(Some(serde_json::to_vec_pretty(value)?))
    }
}

fn members(layer: &Value) -> HashSet<String> {
    layer["tiles"]
        .as_array()
        .filter(|tiles| !tiles.is_empty())
        .map_or_else(
            || {
                layer["key"]
                    .as_str()
                    .map(|key| HashSet::from([key.to_owned()]))
                    .unwrap_or_default()
            },
            |tiles| {
                tiles
                    .iter()
                    .filter_map(|tile| tile["key"].as_str().map(str::to_owned))
                    .collect()
            },
        )
}

fn remap_sources(state: &mut Value, old: &Value, new: &Value) {
    let old = old["layers"].as_array().cloned().unwrap_or_default();
    let mut aliases = HashMap::new();
    for layer in new["layers"].as_array().into_iter().flatten() {
        let Some(target) = layer["key"].as_str() else {
            continue;
        };
        let current = members(layer);
        for previous in &old {
            let Some(source) = previous["key"].as_str() else {
                continue;
            };
            if source != target
                && (current.contains(source) || !current.is_disjoint(&members(previous)))
            {
                aliases
                    .entry(source.to_owned())
                    .or_insert_with(|| target.to_owned());
            }
        }
    }
    fn snapshot(state: &mut Value, aliases: &HashMap<String, String>) {
        let mut seen = HashSet::new();
        if let Some(layers) = state["layers"].as_array_mut() {
            layers.retain_mut(|layer| {
                let source = layer["source"].as_str().unwrap_or("").to_owned();
                let target = aliases.get(&source).unwrap_or(&source).clone();
                if source != target && seen.contains(&target) {
                    return false;
                }
                layer["source"] = json!(target);
                if let Some(source) = layer["mask"]["source"]
                    .as_str()
                    .and_then(|source| aliases.get(source))
                    .cloned()
                {
                    layer["mask"]["source"] = json!(source);
                }
                seen.insert(target);
                true
            });
        }
        if let Some(sources) = state["sources"].as_array_mut() {
            let mut seen = HashSet::new();
            sources.retain_mut(|source| {
                if let Some(target) = source.as_str().and_then(|source| aliases.get(source)) {
                    *source = json!(target);
                }
                seen.insert(source.to_string())
            });
        }
        if let Some(layers) = state["layers"].as_array()
            && !layers
                .iter()
                .any(|layer| layer["id"] == state["selectedLayer"])
        {
            state["selectedLayer"] = layers
                .first()
                .map(|layer| layer["id"].clone())
                .unwrap_or(Value::Null);
        }
    }
    if aliases.is_empty() {
        return;
    }
    snapshot(state, &aliases);
    if let Some(checkpoints) = state["checkpoints"].as_array_mut() {
        for checkpoint in checkpoints {
            if let Some(state) = checkpoint.get_mut("state") {
                snapshot(state, &aliases);
            }
        }
    }
}

pub async fn prepare(session: &Path, archive: &Archive, job: &JobHandle) -> Result<Prepared> {
    let observations = selected(session)?;
    ensure!(!observations.is_empty(), "Select at least one observation.");
    let view = projects::read_json(&session.join("view.json"), Value::Null)?;
    let merge = merge_enabled(session)?;
    let grouped = groups(&observations, merge)?;
    let mut initial = plan(session, archive)?;
    job.stage("download", 0., "Estimating survey transfers")?;
    crate::discovery::interruptible(transfer_plan(session, archive, &mut initial), Some(job))
        .await??;
    let mut files = initial["files"]
        .as_array()
        .context("Invalid build file plan")?
        .clone();
    for file in &mut files {
        file["state"] = json!(if file["cached"] == true {
            "cached"
        } else if file["transfer_bytes"].as_u64() == Some(0) {
            "to prepare"
        } else {
            "to download"
        });
        file["done_bytes"] = if file["cached"] == true {
            file["size_bytes"].clone()
        } else {
            json!(0)
        };
    }
    job.files(files)?;
    let mut paths = HashMap::new();
    let mut inputs = Vec::new();
    let mut grid = initial["grid"].clone();
    for survey in [false, true] {
        if survey {
            grid = candidate_grid(session, &view, &observations, &inputs)?;
            if grid != initial["grid"] {
                let mut revised = plan(session, archive)?;
                crate::discovery::interruptible(
                    transfer_plan(session, archive, &mut revised),
                    Some(job),
                )
                .await??;
                for updated in revised["files"].as_array().unwrap() {
                    if observations.iter().any(|row| {
                        row["obs_id"] == updated["obs_id"]
                            && crate::discovery::survey_id(row).is_some()
                    }) {
                        job.update_file(
                            updated["obs_id"]
                                .as_str()
                                .context("Missing revised observation ID")?,
                            |file| {
                                *file = updated.clone();
                                file["state"] = json!(if file["cached"] == true {
                                    "cached"
                                } else if file["transfer_bytes"].as_u64() == Some(0) {
                                    "to prepare"
                                } else {
                                    "to download"
                                });
                                file["done_bytes"] = if file["cached"] == true {
                                    file["size_bytes"].clone()
                                } else {
                                    json!(0)
                                };
                            },
                        )?;
                    }
                }
            }
        }
        let pending: Vec<_> = observations
            .iter()
            .filter(|row| crate::discovery::survey_id(row).is_some() == survey)
            .cloned()
            .collect();
        let worker_archive = archive.clone();
        let worker_grid = grid.clone();
        let completed = crate::discovery::parallel_downloads(
            pending,
            job,
            move |observation, job| {
                let archive = worker_archive.clone();
                let grid = worker_grid.clone();
                async move {
                    let identifier = observation["obs_id"]
                        .as_str()
                        .context("Missing selected observation ID")?;
                    job.update_file(identifier, |file| {
                        file["state"] = json!(if file["cached"] == true {
                            "cached"
                        } else if file["transfer_bytes"].as_u64() == Some(0) {
                            "assembling"
                        } else {
                            "downloading"
                        });
                    })?;
                    let source_job = job.for_file(identifier);
                    source_job.stage(
                        "download",
                        0.,
                        &format!(
                            "Preparing {}",
                            observation["name"].as_str().unwrap_or(identifier)
                        ),
                    )?;
                    let path = archive.download(&observation, &grid, &source_job).await?;
                    let size = fs::metadata(&path)?.len();
                    let transferred = job.transferred(identifier);
                    job.update_file(identifier, |file| {
                        file["state"] = json!(if file["cached"] == true {
                            "cached"
                        } else {
                            "done"
                        });
                        file["size_bytes"] = json!(size);
                        file["done_bytes"] = json!(size);
                        file["transfer_bytes"] = json!(transferred);
                        file["transferred_bytes"] = json!(transferred);
                        file["transfer_estimated"] = json!(false);
                        file["transfer_note"] = json!(
                            "Actual received science-response payload; cached source pixels need no transfer."
                        );
                    })?;
                    Ok((identifier.to_owned(), path))
                }
            },
        )
        .await?;
        for (identifier, path) in completed {
            if !survey {
                inputs.push(native::source_metadata(&path)?);
            }
            paths.insert(identifier, path);
        }
    }
    let sources = grouped
        .into_iter()
        .map(|group| {
            group
                .into_iter()
                .map(|observation| {
                    let path = paths
                        .get(observation["obs_id"].as_str().unwrap())
                        .context("Missing downloaded science")?
                        .clone();
                    Ok(BuildSource { observation, path })
                })
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<Vec<_>>>()?;
    let stage = tempfile::Builder::new()
        .prefix(".build-")
        .tempdir_in(session)?;
    let destination = stage.path().join("project");
    fs::create_dir(&destination)?;
    let meta = native::prepare_build(
        session,
        &destination,
        &grid,
        &sources,
        merge,
        view["rotation_deg"].as_f64().unwrap_or(0.),
        job,
    )?;
    let previous = projects::project_meta(session)?;
    let old_state = projects::edits(session)?;
    let mut state = old_state.clone();
    let mut markers = projects::read_json(&session.join("markers.json"), Value::Null)?;
    if !previous.is_null() && !grid::same(&previous, &meta) {
        state = grid::remap_state(&state, &previous, &meta)?;
        markers = grid::remap_markers(&markers, &previous, &meta)?;
    }
    if !state.is_null() {
        remap_sources(&mut state, &previous, &meta);
    }
    let mut updates = Vec::new();
    if state != old_state {
        updates.push(("edits.json", bytes(&state)?));
        let revision = projects::read_json(&session.join("edits.rev.json"), json!({}))?["revision"]
            .as_u64()
            .unwrap_or(0)
            + 1;
        updates.push((
            "edits.rev.json",
            bytes(&json!({"revision":revision,"source":"build","note":"Updated build canvas"}))?,
        ));
    }
    if !markers.is_null() {
        updates.push(("markers.json", bytes(&markers)?));
    }
    if !state.is_null() || session.join("grid.json").exists() {
        updates.push(("grid.json", bytes(&grid::from_meta(&meta))?));
    }
    job.stage("publish", 1., "Publishing aligned layers")?;
    Ok(Prepared {
        stage,
        meta,
        updates,
    })
}

pub fn history(session: &Path) -> Result<Vec<Value>> {
    fn summary(meta: &Value, identifier: &str, current: bool) -> Value {
        json!({"id":identifier,"built_at":meta["built_at"],"current":current,"width":meta["width"],"height":meta["height"],
            "layers":meta["layers"].as_array().into_iter().flatten().map(|layer|json!({"obs_id":layer["obs_id"],"filter":layer["filter"],"wavelength_nm":layer["wavelength_nm"]})).collect::<Vec<_>>()})
    }
    let mut result = Vec::new();
    let meta = projects::project_meta(session)?;
    if !meta.is_null() {
        result.push(summary(&meta, "current", true));
    }
    let root = session.join("builds");
    if root.exists() {
        ensure!(
            root.canonicalize()?.parent() == Some(session),
            "Unsafe build history"
        );
        let mut paths = fs::read_dir(&root)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        paths.reverse();
        for path in paths {
            if !path.is_dir() {
                continue;
            }
            ensure!(
                path.canonicalize()?.parent() == Some(root.as_path()),
                "Unsafe archived build"
            );
            let meta = projects::read_json(&path.join("project").join("meta.json"), Value::Null)?;
            if !meta.is_null() {
                result.push(summary(
                    &meta,
                    path.file_name()
                        .unwrap()
                        .to_str()
                        .context("Invalid archive ID")?,
                    false,
                ));
            }
        }
    }
    Ok(result)
}

pub fn restore(session: &Path, identifier: &str) -> Result<()> {
    restore_with_edits(session, identifier, None)
}

pub fn restore_with_edits(session: &Path, identifier: &str, saved: Option<&Value>) -> Result<()> {
    ensure!(
        !identifier.is_empty()
            && identifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c)),
        "Invalid build ID."
    );
    let archive = session
        .join("builds")
        .join(identifier)
        .canonicalize()
        .context("Unknown build")?;
    let root = session.join("builds").canonicalize()?;
    ensure!(root.parent() == Some(session), "Unsafe build history");
    ensure!(
        archive.parent() == Some(root.as_path()),
        "Unsafe build archive"
    );
    let meta = projects::read_json(&archive.join("project").join("meta.json"), Value::Null)?;
    ensure!(!meta.is_null(), "Unknown build");
    let previous = projects::project_meta(session)?;
    let mut state = match saved {
        Some(state) => state.clone(),
        None => projects::edits(&archive)?,
    };
    let archived_grid = projects::read_json(&archive.join("grid.json"), grid::from_meta(&meta))?;
    if saved.is_none() && !state.is_null() && !grid::same(&archived_grid, &meta) {
        state = grid::remap_state(&state, &archived_grid, &meta)?;
    }
    let mut markers = projects::read_json(&session.join("markers.json"), Value::Null)?;
    if !previous.is_null() {
        if state.is_null() {
            state = grid::remap_state(&projects::edits(session)?, &previous, &meta)?;
        }
        markers = grid::remap_markers(&markers, &previous, &meta)?;
    }
    let stage = tempfile::Builder::new()
        .prefix(".restore-")
        .tempdir_in(session)?;
    let destination = stage.path().join("project");
    fs::create_dir(&destination)?;
    let source = archive.join("project").canonicalize()?;
    ensure!(
        source.parent() == Some(archive.as_path()),
        "Unsafe archived project"
    );
    for entry in fs::read_dir(&source)? {
        let entry = entry?;
        ensure!(
            entry.file_type()?.is_file(),
            "Unsupported archived project entry"
        );
        fs::copy(entry.path(), destination.join(entry.file_name()))?;
    }
    let revision = projects::read_json(&session.join("edits.rev.json"), json!({}))?["revision"]
        .as_u64()
        .unwrap_or(0)
        + 1;
    let mut updates = vec![
        ("edits.json", bytes(&state)?),
        ("grid.json", bytes(&grid::from_meta(&meta))?),
        (
            "edits.rev.json",
            bytes(&json!({"revision":revision,"source":"restore","note":"Restored build"}))?,
        ),
    ];
    if !markers.is_null() {
        updates.push(("markers.json", bytes(&markers)?));
    }
    projects::publish_bundle(session, &updates, Some(&destination), true)?;
    if let Err(error) = fs::remove_dir_all(&archive) {
        eprintln!(
            "NightBloom build restored; could not remove consumed archive {}: {error}",
            archive.display()
        );
    }
    Ok(())
}
