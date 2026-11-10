use super::*;
use crate::{
    build, grid,
    sky::{self, Capture},
};
use std::collections::HashSet;

type CaptureUpdates = Vec<(&'static str, Option<Vec<u8>>)>;

#[cfg(test)]
mod tests;

fn unavailable(error: anyhow::Error) -> ApiError {
    eprintln!("NightBloom workflow: {error:#}");
    ApiError(StatusCode::SERVICE_UNAVAILABLE, format!("{error:#}"))
}

fn capture_saved(path: &Path) -> Result<Capture> {
    let view = projects::read_json(&path.join("view.json"), Value::Null)?;
    anyhow::ensure!(!view.is_null(), "This project has no captured view.");
    let original = projects::read_json(&path.join("snapshot-view.json"), view.clone())?;
    let mut header = projects::read_json(&path.join("snapshot-wcs.json"), Value::Null)?;
    if header.is_null() && path.join("snapshot.jpg").exists() {
        use base64::Engine as _;
        header = sky::parse_snapshot(&format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(fs::read(path.join("snapshot.jpg"))?)
        ))?
        .header;
    }
    anyhow::ensure!(
        !header.is_null(),
        "This capture has no original WCS. Capture the view again."
    );
    Ok(Capture {
        view,
        original,
        header,
        jpeg: None,
    })
}

fn captured_name(capture: &Capture, catalog: &Value) -> String {
    catalog["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|group| group["targets"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .find(|name| !name.trim().is_empty() && *name != "UNKNOWN")
        .map(|name| name.chars().take(200).collect())
        .unwrap_or_else(|| {
            format!(
                "RA {:.4} Dec {:.4}",
                capture.view["center_icrs_deg"][0].as_f64().unwrap_or(0.),
                capture.view["center_icrs_deg"][1].as_f64().unwrap_or(0.)
            )
        })
}

fn catalog_warning(catalog: &mut Value) {
    let survey = catalog
        .get("_survey_warning")
        .unwrap_or(&catalog["survey_warning"])
        .clone();
    catalog["_survey_warning"] = survey.clone();
    let mut warnings = Vec::new();
    if let Some(warning) = catalog["mast_warning"]
        .as_str()
        .filter(|warning| !warning.is_empty())
    {
        warnings.push(format!("HST/JWST archive: {warning}"));
        catalog["mast_searched"] = json!(false);
    }
    if let Some(warning) = survey.as_str().filter(|warning| !warning.is_empty()) {
        warnings.push(warning.to_owned());
    }
    catalog.as_object_mut().unwrap().remove("survey_warning");
    if !warnings.is_empty() {
        catalog["survey_warning"] = json!(warnings.join(" "));
    }
}

fn capture_updates(
    path: &Path,
    capture: &Capture,
    catalog: &Value,
    name: &str,
    keep_selection: bool,
) -> Result<CaptureUpdates> {
    let mut settings = projects::read_json(&path.join("session.json"), json!({}))?;
    settings["name"] = json!(name);
    settings["created"] = capture.original["captured_at_utc"].clone();
    let available: HashSet<_> = catalog["groups"]
        .as_array()
        .context("Invalid discovered catalog")?
        .iter()
        .flat_map(|group| group["observations"].as_array().into_iter().flatten())
        .filter_map(|row| row["obs_id"].as_str())
        .collect();
    let mut selection = projects::read_json(&path.join("selection.json"), json!({"obs_ids":[]}))?;
    if keep_selection {
        let selected = selection["obs_ids"]
            .as_array_mut()
            .context("Invalid selection")?;
        selected.retain(|value| value.as_str().is_some_and(|id| available.contains(id)));
    } else {
        selection = json!({"obs_ids":[]});
    }
    let mut updates = vec![
        ("view.json", Some(serde_json::to_vec_pretty(&capture.view)?)),
        (
            "snapshot-view.json",
            Some(serde_json::to_vec_pretty(&capture.original)?),
        ),
        (
            "snapshot-wcs.json",
            Some(serde_json::to_vec_pretty(&capture.header)?),
        ),
        ("catalog.json", Some(serde_json::to_vec_pretty(catalog)?)),
        (
            "selection.json",
            Some(serde_json::to_vec_pretty(&selection)?),
        ),
        ("session.json", Some(serde_json::to_vec_pretty(&settings)?)),
    ];
    if let Some(jpeg) = &capture.jpeg {
        updates.push(("snapshot.jpg", Some(jpeg.clone())));
    }
    Ok(updates)
}

fn start_capture(app: Shared, coordinates: Option<(Capture, String)>) -> ApiResult<Json<Value>> {
    let name = coordinates
        .as_ref()
        .map(|(_, name)| name.as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("New capture")
        .to_owned();
    let mut projects = app.projects.lock().unwrap();
    let path = projects.create(&name).map_err(ApiError::internal)?;
    let session_id = id(&path).to_owned();
    let runtime = tokio::runtime::Handle::current();
    let worker = app.clone();
    let job = app
        .jobs
        .start_kind("capture", false, &session_id, &name, move |handle| {
            runtime.block_on(async {
                handle.stage("snapshot", 0., "Reading captured view")?;
                let (capture, name) = match coordinates {
                    Some((capture, name)) => (capture, name),
                    None => {
                        let value = worker.samp.snapshot().await?;
                        (sky::parse_snapshot(&value)?, String::new())
                    }
                };
                handle.stage("snapshot", 1., "Sky frame received")?;
                let wcs = capture.wcs()?;
                let mut catalog = worker
                    .archive
                    .discover(&capture.view, &wcs.header, &handle)
                    .await?;
                catalog_warning(&mut catalog);
                let name = if name.is_empty() {
                    captured_name(&capture, &catalog)
                } else {
                    name
                };
                let updates = capture_updates(&path, &capture, &catalog, &name, false)?;
                handle.name(&name);
                let _guard = worker.projects.lock().unwrap();
                handle.publish(|| {
                    projects::publish_bundle(&path, &updates, None, false)?;
                    Ok(json!({"session":id(&path)}))
                })
            })
        })
        .map_err(ApiError::bad)?;
    Ok(Json(serde_json::to_value(job).map_err(ApiError::internal)?))
}

pub(super) async fn capture_aladin(State(app): State<Shared>) -> ApiResult<Json<Value>> {
    start_capture(app, None)
}

pub(super) async fn capture_coordinates(
    State(app): State<Shared>,
    Json(request): Json<Value>,
) -> ApiResult<Json<Value>> {
    let capture = sky::coordinate_capture(&request).map_err(ApiError::bad)?;
    let name = request["name"]
        .as_str()
        .unwrap_or("")
        .trim()
        .chars()
        .take(200)
        .collect::<String>();
    if name.chars().any(|character| character.is_control()) {
        return Err(ApiError::bad("Supply a printable project name."));
    }
    start_capture(app, Some((capture, name)))
}

pub(super) async fn coordinate_resolve(
    State(app): State<Shared>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let target = query
        .get("target")
        .map(String::as_str)
        .filter(|target| !target.trim().is_empty())
        .ok_or_else(|| ApiError::bad("Supply a name or coordinates."))?;
    Ok(Json(
        app.archive.resolve(target).await.map_err(unavailable)?,
    ))
}

pub(super) async fn coordinate_coverage(
    State(app): State<Shared>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let number = |name: &str| {
        query
            .get(name)
            .ok_or_else(|| ApiError::bad(format!("Supply {name}.")))?
            .parse::<f64>()
            .map_err(ApiError::bad)
    };
    let request = json!({
        "ra":number("ra")?,"dec":number("dec")?,"aspect":query.get("aspect").map(String::as_str).unwrap_or("16:9"),
        "refresh":query.get("refresh").is_some_and(|value|value=="true")
    });
    sky::coordinate_capture(&request).map_err(ApiError::bad)?;
    Ok(Json(
        app.archive.coverage(&request).await.map_err(unavailable)?,
    ))
}

pub(super) async fn build_plan(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let path = project(&app, &headers, &query)?;
    let mut plan = {
        let _guard = app.projects.lock().unwrap();
        build::plan(&path, &app.archive).map_err(ApiError::bad)?
    };
    build::transfer_plan(&path, &app.archive, &mut plan)
        .await
        .map_err(ApiError::bad)?;
    Ok(Json(plan))
}

pub(super) async fn build_start(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let path = project(&app, &headers, &query)?;
    let _guard = app.projects.lock().unwrap();
    idle(&app, &path)?;
    let plan = build::plan(&path, &app.archive).map_err(ApiError::bad)?;
    if plan["files"].as_array().is_none_or(Vec::is_empty) {
        return Err(ApiError::bad("Select at least one observation."));
    }
    let settings =
        projects::read_json(&path.join("session.json"), json!({})).map_err(ApiError::internal)?;
    let name = settings["name"].as_str().unwrap_or(id(&path)).to_owned();
    let worker = app.clone();
    let runtime = tokio::runtime::Handle::current();
    let session_id = id(&path).to_owned();
    let job = app
        .jobs
        .start_kind("build", true, &session_id, &name, move |handle| {
            let prepared = runtime.block_on(build::prepare(&path, &worker.archive, &handle))?;
            let _guard = worker.projects.lock().unwrap();
            handle.publish(|| prepared.publish(&path))
        })
        .map_err(ApiError::bad)?;
    Ok(Json(serde_json::to_value(job).map_err(ApiError::internal)?))
}

pub(super) async fn build_options(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let enabled = body["mosaic"]
        .as_bool()
        .ok_or_else(|| ApiError::bad("mosaic must be true or false."))?;
    let path = project(&app, &headers, &query)?;
    let _guard = app.projects.lock().unwrap();
    idle(&app, &path)?;
    let mut settings =
        projects::read_json(&path.join("session.json"), json!({})).map_err(ApiError::internal)?;
    if !settings["build"].is_object() {
        settings["build"] = json!({});
    }
    settings["build"]["mosaic"] = json!(enabled);
    projects::write_json(&path.join("session.json"), &settings).map_err(ApiError::internal)?;
    Ok(Json(json!({"mosaic":enabled})))
}

pub(super) async fn selection(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    update_observation_sets(app, headers, query, body, false)
}

pub(super) async fn triage(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    update_observation_sets(app, headers, query, body, true)
}

fn update_observation_sets(
    app: Shared,
    headers: HeaderMap,
    query: HashMap<String, String>,
    body: Value,
    triage: bool,
) -> ApiResult<Json<Value>> {
    let path = project(&app, &headers, &query)?;
    let _guard = app.projects.lock().unwrap();
    idle(&app, &path)?;
    let available = build::observations(&path).map_err(ApiError::internal)?;
    let mut content = json!({});
    for key in if triage {
        &["favorites", "dismissed"][..]
    } else {
        &["obs_ids"][..]
    } {
        let values = body[*key]
            .as_array()
            .ok_or_else(|| ApiError::bad("Supply observation ID arrays."))?;
        let mut seen = HashSet::new();
        let mut ids = Vec::new();
        for value in values {
            let identifier = value
                .as_str()
                .ok_or_else(|| ApiError::bad("Invalid observation ID."))?;
            if !available.contains_key(identifier) {
                return Err(ApiError::bad("Unknown observations."));
            }
            if seen.insert(identifier) {
                ids.push(identifier);
            }
        }
        content[*key] = json!(ids);
    }
    projects::write_json(
        &path.join(if triage {
            "triage.json"
        } else {
            "selection.json"
        }),
        &content,
    )
    .map_err(ApiError::internal)?;
    Ok(Json(content))
}

pub(super) async fn recapture(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let path = project(&app, &headers, &query)?;
    let _guard = app.projects.lock().unwrap();
    idle(&app, &path)?;
    if path.join("project").join("meta.json").exists() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "This project is built, so its view is locked. Use New project for a new view.".into(),
        ));
    }
    let saved = capture_saved(&path).map_err(ApiError::bad)?;
    if saved.view["source"] == "coordinates" {
        return Err(ApiError::bad(
            "Coordinate projects have no Aladin view to recapture.",
        ));
    }
    let worker = app.clone();
    let session_id = id(&path).to_owned();
    let runtime = tokio::runtime::Handle::current();
    let job = app
        .jobs
        .start_kind("capture", false, &session_id, "Recapture", move |handle| {
            runtime.block_on(async {
                let capture = sky::parse_snapshot(&worker.samp.snapshot().await?)?;
                let wcs = capture.wcs()?;
                let mut catalog = worker
                    .archive
                    .discover(&capture.view, &wcs.header, &handle)
                    .await?;
                catalog_warning(&mut catalog);
                let settings = projects::read_json(&path.join("session.json"), json!({}))?;
                let name = settings["name"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| captured_name(&capture, &catalog));
                let updates = capture_updates(&path, &capture, &catalog, &name, false)?;
                handle.name(&name);
                let _guard = worker.projects.lock().unwrap();
                handle.publish(|| {
                    projects::publish_bundle(&path, &updates, None, false)?;
                    Ok(json!({"session":id(&path)}))
                })
            })
        })
        .map_err(ApiError::bad)?;
    Ok(Json(serde_json::to_value(job).map_err(ApiError::internal)?))
}

pub(super) async fn rotation(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let angle = body["rotation_deg"]
        .as_f64()
        .ok_or_else(|| ApiError::bad("Supply a finite capture rotation."))?;
    let path = project(&app, &headers, &query)?;
    let _guard = app.projects.lock().unwrap();
    idle(&app, &path)?;
    let mut capture = capture_saved(&path).map_err(ApiError::bad)?;
    capture.view =
        sky::rotated_view(&capture.original, &capture.header, angle).map_err(ApiError::bad)?;
    let worker = app.clone();
    let session_id = id(&path).to_owned();
    let runtime = tokio::runtime::Handle::current();
    let job = app
        .jobs
        .start_kind(
            "view-rotation",
            false,
            &session_id,
            "Capture rotation",
            move |handle| {
                runtime.block_on(async {
                    let wcs = capture.wcs()?;
                    let mut catalog = worker
                        .archive
                        .discover(&capture.view, &wcs.header, &handle)
                        .await?;
                    catalog_warning(&mut catalog);
                    let name = projects::read_json(&path.join("session.json"), json!({}))?["name"]
                        .as_str()
                        .unwrap_or(id(&path))
                        .to_owned();
                    let updates = capture_updates(&path, &capture, &catalog, &name, true)?;
                    let _guard = worker.projects.lock().unwrap();
                    handle.publish(|| {
                        projects::publish_bundle(&path, &updates, None, false)?;
                        Ok(json!({"rotation_deg":angle}))
                    })
                })
            },
        )
        .map_err(ApiError::bad)?;
    Ok(Json(serde_json::to_value(job).map_err(ApiError::internal)?))
}

pub(super) async fn search_mast(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    retry_search(app, headers, query, true)
}

pub(super) async fn search_surveys(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    retry_search(app, headers, query, false)
}

fn retry_search(
    app: Shared,
    headers: HeaderMap,
    query: HashMap<String, String>,
    mast: bool,
) -> ApiResult<Json<Value>> {
    let path = project(&app, &headers, &query)?;
    let _guard = app.projects.lock().unwrap();
    idle(&app, &path)?;
    let capture = capture_saved(&path).map_err(ApiError::bad)?;
    let worker = app.clone();
    let session_id = id(&path).to_owned();
    let runtime = tokio::runtime::Handle::current();
    let job = app
        .jobs
        .start_kind(
            if mast {
                "search-mast"
            } else {
                "search-surveys"
            },
            false,
            &session_id,
            "Archive search",
            move |handle| {
                runtime.block_on(async {
                    let wcs = capture.wcs()?;
                    let mut catalog =
                        projects::read_json(&path.join("catalog.json"), json!({"groups":[]}))?;
                    let searches = if !mast && catalog.get("mast_warning").is_some() {
                        vec![false, true]
                    } else {
                        vec![mast]
                    };
                    for mast in searches {
                        let found = if mast {
                            worker
                                .archive
                                .mast_groups(&capture.view, &wcs.header, &handle)
                                .await?
                        } else {
                            worker
                                .archive
                                .survey_groups(&capture.view, &wcs.header, &handle)
                                .await?
                        };
                        let original = catalog["groups"]
                            .as_array()
                            .context("Invalid saved catalog")?;
                        let mut groups: Vec<_> = original
                            .iter()
                            .filter(|group| (group["collection"] != "Survey") != mast)
                            .cloned()
                            .collect();
                        let warning_key = if mast {
                            "mast_warning"
                        } else {
                            "survey_warning"
                        };
                        let incoming = found["groups"]
                            .as_array()
                            .context("Invalid archive search response")?;
                        if incoming.is_empty() && found.get(warning_key).is_some() {
                            groups.extend(
                                original
                                    .iter()
                                    .filter(|group| (group["collection"] != "Survey") == mast)
                                    .cloned(),
                            );
                        } else {
                            groups.extend(incoming.iter().cloned());
                        }
                        catalog["groups"] = json!(groups);
                        if mast {
                            let object = catalog.as_object_mut().unwrap();
                            object.remove(warning_key);
                            if let Some(warning) = found.get(warning_key) {
                                object.insert(warning_key.into(), warning.clone());
                            }
                            catalog["mast_searched"] = json!(true);
                        } else {
                            catalog["_survey_warning"] =
                                found.get(warning_key).cloned().unwrap_or(Value::Null);
                        }
                    }
                    catalog["observations_in_view"] = json!(
                        catalog["groups"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|group| group["observations"].as_array().map_or(0, Vec::len))
                            .sum::<usize>()
                    );
                    catalog_warning(&mut catalog);
                    let _guard = worker.projects.lock().unwrap();
                    handle.publish(|| {
                        projects::publish_bundle(
                            &path,
                            &[("catalog.json", Some(serde_json::to_vec_pretty(&catalog)?))],
                            None,
                            false,
                        )?;
                        Ok(json!({"groups":catalog["groups"].as_array().unwrap().len()}))
                    })
                })
            },
        )
        .map_err(ApiError::bad)?;
    Ok(Json(serde_json::to_value(job).map_err(ApiError::internal)?))
}

pub(super) async fn refresh_snapshot(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let path = project(&app, &headers, &query)?;
    let saved = {
        let _guard = app.projects.lock().unwrap();
        idle(&app, &path)?;
        capture_saved(&path).map_err(ApiError::bad)?
    };
    if saved.view["source"] == "coordinates" {
        return Err(ApiError::bad(
            "Coordinate projects have no Aladin snapshot.",
        ));
    }
    let mut fresh = sky::parse_snapshot(&app.samp.snapshot().await.map_err(unavailable)?)
        .map_err(ApiError::bad)?;
    let number =
        |value: &Value, key: &str, index: usize| value[key][index].as_f64().unwrap_or(f64::NAN);
    let old_center = [
        number(&saved.original, "center_icrs_deg", 0),
        number(&saved.original, "center_icrs_deg", 1),
    ];
    let new_center = [
        number(&fresh.original, "center_icrs_deg", 0),
        number(&fresh.original, "center_icrs_deg", 1),
    ];
    let extent =
        |view: &Value| number(view, "field_arcmin", 0).hypot(number(view, "field_arcmin", 1));
    let scale = extent(&fresh.original) / extent(&saved.original);
    let tolerance = 0.1
        * number(&saved.original, "field_arcmin", 0).min(number(
            &saved.original,
            "field_arcmin",
            1,
        ));
    if sky::separation(old_center, new_center) * 60. > tolerance || !(0.8..=1.25).contains(&scale) {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Aladin's view changed. Use Recapture, or New project for a built project.".into(),
        ));
    }
    let _guard = app.projects.lock().unwrap();
    idle(&app, &path)?;
    if projects::read_json(&path.join("view.json"), Value::Null).map_err(ApiError::internal)?
        != saved.view
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "The captured view changed while refreshing. Try again.".into(),
        ));
    }
    fresh.view = sky::rotated_view(
        &fresh.original,
        &fresh.header,
        saved.view["rotation_deg"].as_f64().unwrap_or(0.),
    )
    .map_err(ApiError::bad)?;
    let wcs = fresh.wcs().map_err(ApiError::bad)?;
    let width = fresh.view["pixel_dimensions"][0].as_u64().unwrap() as u32;
    let height = fresh.view["pixel_dimensions"][1].as_u64().unwrap() as u32;
    let mut catalog = projects::read_json(&path.join("catalog.json"), json!({"groups":[]}))
        .map_err(ApiError::internal)?;
    for group in catalog["groups"]
        .as_array_mut()
        .context("Invalid saved catalog")
        .map_err(ApiError::internal)?
    {
        for observation in group["observations"]
            .as_array_mut()
            .context("Invalid saved group")
            .map_err(ApiError::internal)?
        {
            observation["pixels"] = crate::discovery::projected_pixels(
                observation["region"].as_str().unwrap_or(""),
                &wcs.header,
                width,
                height,
            )
            .map_err(ApiError::internal)?;
        }
    }
    let updates = [
        ("snapshot.jpg", fresh.jpeg),
        (
            "snapshot-view.json",
            Some(serde_json::to_vec_pretty(&fresh.original).map_err(ApiError::internal)?),
        ),
        (
            "snapshot-wcs.json",
            Some(serde_json::to_vec_pretty(&fresh.header).map_err(ApiError::internal)?),
        ),
        (
            "view.json",
            Some(serde_json::to_vec_pretty(&fresh.view).map_err(ApiError::internal)?),
        ),
        (
            "catalog.json",
            Some(serde_json::to_vec_pretty(&catalog).map_err(ApiError::internal)?),
        ),
    ];
    projects::publish_bundle(&path, &updates, None, false).map_err(ApiError::internal)?;
    Ok(Json(json!({"view":fresh.view})))
}

pub(super) async fn aladin_goto(
    State(app): State<Shared>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let target = body["target"]
        .as_str()
        .map(str::trim)
        .filter(|value| {
            !value.is_empty()
                && value.chars().count() <= 200
                && !value.chars().any(|c| c.is_control() || c == ';')
        })
        .ok_or_else(|| ApiError::bad("Supply a single Aladin target or coordinate pair."))?;
    let field = body.get("fov_arcmin").map_or(Ok(10.), |value| {
        value
            .as_f64()
            .ok_or_else(|| ApiError::bad("Field of view must be a number."))
    })?;
    if !field.is_finite() || field <= 0. {
        return Err(ApiError::bad("Field of view must be positive."));
    }
    app.samp
        .scripts(&[format!("goto {target}"), format!("zoom {field}arcmin")])
        .await
        .map_err(unavailable)?;
    Ok(Json(json!({"ok":true})))
}

pub(super) async fn aladin_footprints(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let path = project(&app, &headers, &query)?;
    let available = build::observations(&path).map_err(ApiError::internal)?;
    let ids = body["obs_ids"]
        .as_array()
        .ok_or_else(|| ApiError::bad("Supply observation IDs."))?;
    let mut commands = vec!["rm AstroImp".to_owned(), "rm NightBloom".to_owned()];
    if !ids.is_empty() {
        commands.extend(
            [
                "setconf frame=ICRS",
                "draw mode(radec)",
                "draw newtool(NightBloom)",
                "select NightBloom",
            ]
            .map(str::to_owned),
        );
    }
    let mut seen = HashSet::new();
    let mut count = 0;
    for id in ids {
        let id = id
            .as_str()
            .ok_or_else(|| ApiError::bad("Invalid observation ID."))?;
        let observation = available
            .get(id)
            .ok_or_else(|| ApiError::bad("Unknown observations."))?;
        if !seen.insert(id) {
            continue;
        }
        for polygon in bookmarks::polygons(observation["region"].as_str().unwrap_or("")) {
            let coordinates = polygon
                .iter()
                .flat_map(|point| point.iter())
                .map(|value| format!("{value:.10}"))
                .collect::<Vec<_>>()
                .join(",");
            commands.push(format!("draw polygon({coordinates})"));
            count += 1;
        }
    }
    app.samp
        .scripts(&[commands.join(";")])
        .await
        .map_err(unavailable)?;
    Ok(Json(json!({"ok":true,"polygons":count})))
}

async fn survey_preview(
    app: Shared,
    path: PathBuf,
    observation: Value,
    max_side: u32,
) -> ApiResult<Response> {
    let view =
        projects::read_json(&path.join("view.json"), Value::Null).map_err(ApiError::internal)?;
    let grid = grid::view_grid(
        &view,
        observation["native_scale_arcsec"].as_f64().unwrap_or(1.),
        max_side,
    )
    .map_err(ApiError::bad)?;
    if observation["hips_id"]
        .as_str()
        .is_some_and(|id| id.starts_with("CDS/P/PanSTARRS/DR1/"))
    {
        let bytes = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            app.archive.panstarrs_preview(&observation, &grid),
        )
        .await
        .map_err(|_| unavailable(anyhow::anyhow!("Pan-STARRS preview timed out. Try again.")))?
        .map_err(unavailable)?;
        return Ok((
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::CACHE_CONTROL, "private, max-age=86400"),
            ],
            bytes,
        )
            .into_response());
    }
    let source = app
        .archive
        .download(&observation, &grid, &JobHandle::untracked())
        .await
        .map_err(unavailable)?;
    let permit = app
        .rendering
        .clone()
        .acquire_owned()
        .await
        .map_err(ApiError::internal)?;
    let bytes = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        native::preview_source(&source, &grid)
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::internal)?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "private, max-age=86400"),
        ],
        bytes,
    )
        .into_response())
}

pub(super) async fn coordinate_backdrop(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let path = project(&app, &headers, &query)?;
    let view =
        projects::read_json(&path.join("view.json"), Value::Null).map_err(ApiError::internal)?;
    if view["source"] != "coordinates" {
        return Err(ApiError::missing("No coordinate frame."));
    }
    survey_preview(app,path,json!({"obs_id":"survey-CDS-P-DSS2-red","hips_id":"CDS/P/DSS2/red","collection":"Survey","filter":"red","data_uri":"hips:CDS/P/DSS2/red"}),1000).await
}

pub(super) async fn observation_preview(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    RoutePath(identifier): RoutePath<String>,
) -> ApiResult<Response> {
    let path = project(&app, &headers, &query)?;
    let available = build::observations(&path).map_err(ApiError::internal)?;
    let observation = available
        .get(&identifier)
        .ok_or_else(|| ApiError::missing("Unknown observation."))?
        .clone();
    let size = query.get("size").map(String::as_str).unwrap_or("thumb");
    if !matches!(size, "thumb" | "large") {
        return Err(ApiError::bad("Size must be thumb or large."));
    }
    if crate::discovery::survey_id(&observation).is_some() {
        return survey_preview(
            app,
            path,
            observation,
            if size == "large" { 1400 } else { 320 },
        )
        .await;
    }
    let uri = observation["preview_uri"]
        .as_str()
        .ok_or_else(|| ApiError::missing("Preview unavailable."))?;
    let bytes = app
        .previews
        .archive(uri, size == "large")
        .await
        .map_err(unavailable)?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "private, max-age=86400"),
        ],
        bytes,
    )
        .into_response())
}

pub(super) async fn build_history(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let path = project(&app, &headers, &query)?;
    let _guard = app.projects.lock().unwrap();
    Ok(Json(json!(
        build::history(&path).map_err(ApiError::internal)?
    )))
}

pub(super) async fn restore_build(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    RoutePath(identifier): RoutePath<String>,
) -> ApiResult<Json<Value>> {
    let path = project(&app, &headers, &query)?;
    let restored = identifier.clone();
    tokio::task::spawn_blocking(move || {
        let _guard = app.projects.lock().unwrap();
        idle(&app, &path)?;
        build::restore(&path, &identifier).map_err(ApiError::bad)
    })
    .await
    .map_err(ApiError::internal)??;
    Ok(Json(json!({"restored":restored})))
}
