use crate::{
    bookmarks::{self, Bookmarks},
    discovery::Archive,
    gallery,
    jobs::{JobHandle, Jobs},
    native,
    previews::Previews,
    projects::{self, Projects},
    render,
    samp::Samp,
    thumbnails,
};
use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path as RoutePath, Query, Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tower_http::services::{ServeDir, ServeFile};
use uuid::Uuid;

mod workflow;
use workflow::*;

struct App {
    data: PathBuf,
    projects: Mutex<Projects>,
    bookmarks: Mutex<Bookmarks>,
    previews: Previews,
    samp: Samp,
    archive: Archive,
    jobs: Jobs,
    thumbnails: Mutex<()>,
    export_preview: Mutex<Option<ExportPreview>>,
    rendering: Arc<tokio::sync::Semaphore>,
    gallery_decoding: Arc<tokio::sync::Semaphore>,
}
struct ExportPreview {
    path: PathBuf,
    state: Value,
    meta: Value,
    image: crate::contract::RenderedImage,
}
type Shared = Arc<App>;
type ApiResult<T> = std::result::Result<T, ApiError>;

async fn gallery_inventory(State(app): State<Shared>) -> ApiResult<Json<Value>> {
    let value = tokio::task::spawn_blocking(move || {
        gallery::inventory(&app.projects.lock().unwrap(), &app.data)
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::internal)?;
    Ok(Json(value))
}

#[derive(Deserialize)]
struct GalleryRestore {
    filename: String,
}

fn restore_export_look(app: &App, identifier: &str, checkpoint: &str) -> ApiResult<Value> {
    let mut projects = app.projects.lock().unwrap();
    let path = projects
        .resolve_readonly(identifier)
        .map_err(ApiError::missing)?;
    idle(app, &path)?;
    crate::export_history::restore(&path, checkpoint)
        .map_err(|error| ApiError(StatusCode::CONFLICT, format!("{error:#}")))?;
    projects.open(identifier).map_err(ApiError::internal)?;
    projects::payload(&path).map_err(ApiError::internal)
}

async fn gallery_restore(
    State(app): State<Shared>,
    RoutePath(identifier): RoutePath<String>,
    Json(body): Json<GalleryRestore>,
) -> ApiResult<Json<Value>> {
    let value = tokio::task::spawn_blocking(move || {
        let path = app
            .projects
            .lock()
            .unwrap()
            .resolve_readonly(&identifier)
            .map_err(ApiError::missing)?;
        let source = projects::export_file(&path, &body.filename).map_err(ApiError::missing)?;
        let checkpoint = crate::export_history::image_checkpoint(&path, &source, &body.filename)
            .map_err(|error| ApiError(StatusCode::CONFLICT, format!("{error:#}")))?;
        restore_export_look(&app, &identifier, &checkpoint)
    })
    .await
    .map_err(ApiError::internal)??;
    Ok(Json(value))
}

async fn checkpoint_restore(
    State(app): State<Shared>,
    RoutePath((identifier, checkpoint)): RoutePath<(String, String)>,
) -> ApiResult<Json<Value>> {
    let value =
        tokio::task::spawn_blocking(move || restore_export_look(&app, &identifier, &checkpoint))
            .await
            .map_err(ApiError::internal)??;
    Ok(Json(value))
}

async fn gallery_image(
    State(app): State<Shared>,
    RoutePath((identifier, filename)): RoutePath<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    request: Request,
) -> ApiResult<Response> {
    let project = app
        .projects
        .lock()
        .unwrap()
        .resolve_readonly(&identifier)
        .map_err(ApiError::missing)?;
    let source = projects::export_file(&project, &filename).map_err(ApiError::missing)?;
    if !gallery::is_image(&source) {
        return Err(ApiError::missing("Unknown exported image"));
    }
    let view = query
        .get("view")
        .cloned()
        .unwrap_or_else(|| "thumbnail".into());
    if view == "original" {
        let response = ServeFile::new(source)
            .try_call(request)
            .await
            .map_err(ApiError::internal)?;
        let mut response = response.map(axum::body::Body::new);
        response.headers_mut().insert(
            header::CONTENT_DISPOSITION,
            axum::http::HeaderValue::from_static("attachment"),
        );
        return Ok(response);
    }
    let mime = if view == "clipboard" {
        "image/png"
    } else {
        "image/jpeg"
    };
    let cache_control = if view == "clipboard" {
        "no-store"
    } else {
        "private, max-age=3600"
    };
    let permit = app
        .gallery_decoding
        .clone()
        .acquire_owned()
        .await
        .map_err(ApiError::internal)?;
    let bytes = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        gallery::preview(&source, &app.data.join("cache/gallery"), &view)
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::bad)?;
    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, cache_control),
        ],
        bytes,
    )
        .into_response())
}

async fn storage_inventory(State(app): State<Shared>) -> ApiResult<Json<Value>> {
    let value = tokio::task::spawn_blocking(move || {
        let _guard = app.projects.lock().unwrap();
        crate::storage::inventory(&app.data)
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::internal)?;
    Ok(Json(value))
}

async fn storage_delete(
    State(app): State<Shared>,
    Json(body): Json<crate::storage::Delete>,
) -> ApiResult<Json<Value>> {
    let value = tokio::task::spawn_blocking(move || {
        let _guard = app.projects.lock().unwrap();
        if app
            .jobs
            .all()
            .iter()
            .any(|job| matches!(job.state.as_str(), "running" | "queued"))
        {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "A job is running. Wait or cancel it before cleaning up.".into(),
            ));
        }
        crate::storage::delete(&app.data, body).map_err(ApiError::bad)
    })
    .await
    .map_err(ApiError::internal)??;
    Ok(Json(value))
}

async fn delete_project(
    State(app): State<Shared>,
    RoutePath(identifier): RoutePath<String>,
) -> ApiResult<Json<Value>> {
    tokio::task::spawn_blocking(move || {
        let mut projects = app.projects.lock().unwrap();
        crate::storage::files(&app.data.join("sessions")).map_err(ApiError::bad)?;
        let path = projects.resolve(&identifier).map_err(ApiError::missing)?;
        idle(&app, &path)?;
        let current = projects.trash(&identifier).map_err(ApiError::bad)?;
        Ok(Json(json!({"current":current})))
    })
    .await
    .map_err(ApiError::internal)?
}

struct ApiError(StatusCode, String);
impl ApiError {
    fn bad(error: impl std::fmt::Display) -> Self {
        Self(StatusCode::BAD_REQUEST, error.to_string())
    }
    fn missing(error: impl std::fmt::Display) -> Self {
        Self(StatusCode::NOT_FOUND, error.to_string())
    }
    fn internal(error: impl std::fmt::Display) -> Self {
        eprintln!("NightBloom HTTP error: {error}");
        Self(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"detail": self.1}))).into_response()
    }
}

fn project(app: &App, headers: &HeaderMap, query: &HashMap<String, String>) -> ApiResult<PathBuf> {
    let header = headers
        .get("x-astroimp-session")
        .map(|v| v.to_str().map_err(ApiError::bad))
        .transpose()?;
    let id = header.or_else(|| query.get("session").map(String::as_str));
    app.projects
        .lock()
        .unwrap()
        .current(id)
        .map_err(ApiError::missing)
}

fn id(path: &Path) -> &str {
    path.file_name().and_then(|s| s.to_str()).unwrap_or("")
}
fn idle(app: &App, path: &Path) -> ApiResult<()> {
    if app.jobs.busy(id(path)) {
        Err(ApiError(
            StatusCode::CONFLICT,
            "This project has an active job. Wait or cancel it first.".into(),
        ))
    } else {
        Ok(())
    }
}

/// Build the same-origin desktop HTTP service.
pub fn router(data: PathBuf, frontend: PathBuf) -> Result<Router> {
    Ok(router_with_shutdown(data, frontend)?.0)
}

pub struct Shutdown(Jobs);

impl Shutdown {
    pub fn cancel_and_wait(&self) {
        self.0.cancel_and_wait();
    }
}

pub fn router_with_shutdown(data: PathBuf, frontend: PathBuf) -> Result<(Router, Shutdown)> {
    let archive = Archive::new(&data)?;
    let samp = if cfg!(test) {
        Samp::at(data.join(".samp"))
    } else {
        Samp::default()
    };
    router_with_services(data, frontend, archive, samp)
}

fn router_with_services(
    data: PathBuf,
    frontend: PathBuf,
    archive: Archive,
    samp: Samp,
) -> Result<(Router, Shutdown)> {
    let app = Arc::new(App {
        data: data.canonicalize()?,
        samp,
        archive,
        bookmarks: Mutex::new(Bookmarks::new(&data)),
        previews: Previews::new(&data)?,
        projects: Mutex::new(Projects::new(data)?),
        jobs: Jobs::default(),
        thumbnails: Mutex::new(()),
        export_preview: Mutex::new(None),
        rendering: Arc::new(tokio::sync::Semaphore::new(1)),
        gallery_decoding: Arc::new(tokio::sync::Semaphore::new(1)),
    });
    let shutdown = Shutdown(app.jobs.clone());
    let router = Router::new()
        .route("/api/status", get(status))
        .route("/api/capture", post(capture_aladin))
        .route("/api/capture/coordinates", post(capture_coordinates))
        .route("/api/coordinates/resolve", get(coordinate_resolve))
        .route("/api/coordinates/coverage", get(coordinate_coverage))
        .route("/api/bookmarks", get(bookmarks_list).post(bookmarks_add))
        .route("/api/bookmarks/frame", post(bookmarks_frame))
        .route(
            "/api/bookmarks/{id}",
            axum::routing::patch(bookmark_note).delete(bookmark_remove),
        )
        .route("/api/bookmarks/{id}/preview.jpg", get(bookmark_preview))
        .route("/api/sessions", get(sessions))
        .route("/api/gallery", get(gallery_inventory))
        .route("/api/gallery/{id}/restore", post(gallery_restore))
        .route("/api/gallery/{id}/{*filename}", get(gallery_image))
        .route(
            "/api/sessions/{id}/export-checkpoints/{checkpoint}/restore",
            post(checkpoint_restore),
        )
        .route("/api/sessions/{id}", axum::routing::delete(delete_project))
        .route("/api/storage", get(storage_inventory))
        .route("/api/storage/delete", post(storage_delete))
        .route("/api/sessions/{id}/thumbnail.jpg", get(thumbnail))
        .route("/api/sessions/{id}/open", post(open))
        .route("/api/session", get(session).patch(rename))
        .route("/api/session/build", axum::routing::put(build_options))
        .route("/api/session/recapture", post(recapture))
        .route("/api/session/rotation", axum::routing::put(rotation))
        .route("/api/session/search-mast", post(search_mast))
        .route("/api/session/search-surveys", post(search_surveys))
        .route("/api/session/snapshot.jpg", get(snapshot_image))
        .route("/api/session/snapshot/refresh", post(refresh_snapshot))
        .route("/api/session/backdrop.jpg", get(coordinate_backdrop))
        .route("/api/aladin/goto", post(aladin_goto))
        .route("/api/aladin/footprints", post(aladin_footprints))
        .route("/api/selection", axum::routing::put(selection))
        .route("/api/triage", axum::routing::put(triage))
        .route("/api/preview/{id}", get(observation_preview))
        .route("/api/build", post(build_start))
        .route("/api/build/plan", get(build_plan))
        .route("/api/builds", get(build_history))
        .route("/api/builds/{id}/restore", post(restore_build))
        .route("/api/project/{file}", get(project_file))
        .route("/api/edits", get(edits).put(save_edits))
        .route("/api/edits/revision", get(revision))
        .route("/api/markers", get(markers).put(save_markers))
        .route("/api/job", get(job))
        .route("/api/job/cancel", post(cancel))
        .route("/api/export", post(export_png))
        .route("/api/export/checkpoint", post(export_checkpoint))
        .route("/api/export/full/plan", get(export_plan))
        .route("/api/export/full/size", post(export_size))
        .route("/api/export/full", post(export_full))
        .route("/api/export/full/download/{filename}", get(download))
        .route("/api/{*path}", any(unsupported))
        .fallback_service(ServeDir::new(frontend).append_index_html_on_directories(true))
        .layer(DefaultBodyLimit::max(256 * 1024 * 1024))
        .layer(middleware::from_fn(loopback_only))
        .with_state(app);
    Ok((router, shutdown))
}

fn loopback_authority(authority: &str) -> bool {
    let lower = authority.to_ascii_lowercase();
    let (host, port) = if lower.starts_with('[') {
        match lower.find(']') {
            Some(end) => (&lower[..=end], &lower[end + 1..]),
            None => return false,
        }
    } else {
        match lower.find(':') {
            Some(i) => (&lower[..i], &lower[i..]),
            None => (lower.as_str(), ""),
        }
    };
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
        && (port.is_empty()
            || port.strip_prefix(':').is_some_and(|p| {
                !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()) && p.parse::<u16>().is_ok()
            }))
}
pub async fn loopback_only(request: Request, next: Next) -> Response {
    let valid_host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(loopback_authority);
    if !valid_host {
        return ApiError(StatusCode::FORBIDDEN, "Forbidden host".into()).into_response();
    }
    if !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) && let Some(origin) = request.headers().get(header::ORIGIN)
    {
        let valid = origin
            .to_str()
            .ok()
            .and_then(|o| {
                o.strip_prefix("http://")
                    .or_else(|| o.strip_prefix("https://"))
            })
            .is_some_and(loopback_authority);
        if !valid {
            return ApiError(StatusCode::FORBIDDEN, "Forbidden origin".into()).into_response();
        }
    }
    next.run(request).await
}

async fn unsupported() -> ApiError {
    ApiError(
        StatusCode::NOT_IMPLEMENTED,
        "This feature is not available in the desktop milestone.".into(),
    )
}
async fn bookmarks_list(State(app): State<Shared>) -> ApiResult<Json<Value>> {
    let items = app
        .bookmarks
        .lock()
        .unwrap()
        .list()
        .map_err(ApiError::internal)?;
    Ok(Json(json!(items)))
}

#[derive(Deserialize)]
struct BookmarkAdd {
    session: String,
    obs_ids: Vec<String>,
}

async fn bookmarks_add(
    State(app): State<Shared>,
    Json(body): Json<BookmarkAdd>,
) -> ApiResult<Json<Value>> {
    let path = app
        .projects
        .lock()
        .unwrap()
        .resolve(&body.session)
        .map_err(ApiError::missing)?;
    let _guard = app.projects.lock().unwrap();
    let catalog =
        projects::read_json(&path.join("catalog.json"), json!({})).map_err(ApiError::internal)?;
    let available: HashMap<_, _> = catalog["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|group| group["observations"].as_array().into_iter().flatten())
        .filter_map(|row| row["obs_id"].as_str().map(|id| (id, row)))
        .collect();
    let rows = body
        .obs_ids
        .iter()
        .map(|id| {
            available
                .get(id.as_str())
                .map(|row| (*row).clone())
                .ok_or_else(|| ApiError::bad("Bookmarks contain unknown observations."))
        })
        .collect::<ApiResult<Vec<_>>>()?;
    let metadata =
        projects::read_json(&path.join("session.json"), json!({})).map_err(ApiError::internal)?;
    let name = metadata["name"]
        .as_str()
        .filter(|name| !name.is_empty())
        .unwrap_or(&body.session);
    let items = app
        .bookmarks
        .lock()
        .unwrap()
        .add(&rows, &body.session, name)
        .map_err(bookmark_error)?;
    Ok(Json(json!(items)))
}

fn bookmark_error(error: anyhow::Error) -> ApiError {
    if error.is::<std::io::Error>() || error.is::<serde_json::Error>() {
        ApiError::internal(error)
    } else {
        ApiError::bad(error)
    }
}

#[derive(Deserialize)]
struct BookmarkFrame {
    ids: Vec<String>,
    #[serde(default = "default_aspect")]
    aspect: String,
}

fn default_aspect() -> String {
    "16:9".into()
}

async fn bookmarks_frame(
    State(app): State<Shared>,
    Json(body): Json<BookmarkFrame>,
) -> ApiResult<Json<Value>> {
    let items = app
        .bookmarks
        .lock()
        .unwrap()
        .list()
        .map_err(ApiError::internal)?;
    let available: HashMap<_, _> = items
        .iter()
        .filter_map(|item| item["id"].as_str().map(|id| (id, item)))
        .collect();
    let mut selected: Vec<Value> = Vec::new();
    for identifier in body.ids {
        let item = available
            .get(identifier.as_str())
            .ok_or_else(|| ApiError::missing("Unknown bookmark."))?;
        if !selected.contains(*item) {
            selected.push((*item).clone());
        }
    }
    Ok(Json(
        bookmarks::frame(&selected, &body.aspect).map_err(ApiError::bad)?,
    ))
}

#[derive(Deserialize)]
struct BookmarkNote {
    note: String,
}

async fn bookmark_note(
    State(app): State<Shared>,
    RoutePath(identifier): RoutePath<String>,
    Json(body): Json<BookmarkNote>,
) -> ApiResult<Json<Value>> {
    if body.note.chars().count() > 200 {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Notes can contain at most 200 characters.".into(),
        ));
    }
    let item = app
        .bookmarks
        .lock()
        .unwrap()
        .note(&identifier, &body.note)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::missing("Unknown bookmark."))?;
    Ok(Json(item))
}

async fn bookmark_remove(
    State(app): State<Shared>,
    RoutePath(identifier): RoutePath<String>,
) -> ApiResult<Json<Value>> {
    if !app
        .bookmarks
        .lock()
        .unwrap()
        .remove(&identifier)
        .map_err(ApiError::internal)?
    {
        return Err(ApiError::missing("Unknown bookmark."));
    }
    Ok(Json(json!({"removed":identifier})))
}

async fn bookmark_preview(
    State(app): State<Shared>,
    RoutePath(identifier): RoutePath<String>,
) -> ApiResult<Response> {
    let item = app
        .bookmarks
        .lock()
        .unwrap()
        .get(&identifier)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::missing("Unknown bookmark."))?;
    let uri = item["preview"]
        .as_str()
        .filter(|uri| !uri.is_empty())
        .ok_or_else(|| ApiError::missing("Preview unavailable."))?;
    let bytes = app.previews.archive(uri, false).await.map_err(|error| {
        eprintln!("NightBloom bookmark preview: {error:#}");
        ApiError(
            StatusCode::BAD_GATEWAY,
            format!("Could not load archive preview. {error:#}"),
        )
    })?;
    Ok(([(header::CONTENT_TYPE, "image/jpeg")], bytes).into_response())
}

async fn sessions(State(app): State<Shared>) -> ApiResult<Json<Value>> {
    let list = app
        .projects
        .lock()
        .unwrap()
        .list()
        .map_err(ApiError::internal)?;
    Ok(Json(json!(list)))
}
async fn thumbnail(
    State(app): State<Shared>,
    RoutePath(identifier): RoutePath<String>,
) -> ApiResult<Response> {
    let path = app
        .projects
        .lock()
        .unwrap()
        .resolve(&identifier)
        .map_err(ApiError::missing)?;
    let bytes = tokio::task::spawn_blocking(move || {
        // Serialize image decoding independently of edit/export jobs and the async runtime.
        let _guard = app.thumbnails.lock().unwrap();
        thumbnails::thumbnail(&path)
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::internal)?
    .ok_or_else(|| ApiError::missing("No session thumbnail."))?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        bytes,
    )
        .into_response())
}
async fn open(
    State(app): State<Shared>,
    RoutePath(identifier): RoutePath<String>,
) -> ApiResult<Json<Value>> {
    let mut projects = app.projects.lock().unwrap();
    let path = projects.open(&identifier).map_err(ApiError::missing)?;
    Ok(Json(projects::payload(&path).map_err(ApiError::internal)?))
}
async fn session(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let path = project(&app, &headers, &query)?;
    let _guard = app.projects.lock().unwrap();
    Ok(Json(projects::payload(&path).map_err(ApiError::internal)?))
}

async fn snapshot_image(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let path = project(&app, &headers, &query)?;
    let _guard = app.projects.lock().unwrap();
    let bytes = fs::read(path.join("snapshot.jpg")).map_err(ApiError::missing)?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        bytes,
    )
        .into_response())
}
#[derive(Deserialize)]
struct Rename {
    name: String,
}
async fn rename(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<Rename>,
) -> ApiResult<Json<Value>> {
    let name = body.name.trim();
    if name.is_empty() || name.chars().count() > 200 || name.chars().any(|c| c.is_control()) {
        return Err(ApiError::bad(
            "Supply a session name of 1–200 printable characters.",
        ));
    }
    let path = project(&app, &headers, &query)?;
    let _guard = app.projects.lock().unwrap();
    idle(&app, &path)?;
    let mut value =
        projects::read_json(&path.join("session.json"), json!({})).map_err(ApiError::internal)?;
    value["name"] = name.into();
    value["auto_named"] = false.into();
    projects::write_json(&path.join("session.json"), &value).map_err(ApiError::internal)?;
    Ok(Json(json!({"name":name})))
}
async fn status(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let scoped = headers.contains_key("x-astroimp-session") || query.contains_key("session");
    let path = match project(&app, &headers, &query) {
        Ok(path) => Some(path),
        Err(_) if !scoped => None,
        Err(e) => return Err(e),
    };
    let _guard = app.projects.lock().unwrap();
    let session = path.as_deref().map(id);
    let rev = match &path {
        Some(path) => projects::read_json(&path.join("edits.rev.json"), json!({"revision":0}))
            .map_err(ApiError::internal)?,
        None => Value::Null,
    };
    Ok(Json(
        json!({"aladin":app.samp.connected(),"services":{"cds":{"ok":null,"checked":null},"mast":{"ok":null,"checked":null}},
        "stale":false,"restartable":false,"pid":std::process::id(),"edits":rev,"session":session,
        "job":app.jobs.latest(session),"jobs":app.jobs.all()}),
    ))
}
async fn project_file(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    RoutePath(file): RoutePath<String>,
) -> ApiResult<Response> {
    let path = project(&app, &headers, &query)?;
    let _guard = app.projects.lock().unwrap();
    let bytes = projects::project_file(&path, &file).map_err(ApiError::missing)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        bytes,
    )
        .into_response())
}
async fn edits(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let path = project(&app, &headers, &query)?;
    let _guard = app.projects.lock().unwrap();
    Ok(Json(projects::edits(&path).map_err(ApiError::internal)?))
}
async fn revision(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let path = project(&app, &headers, &query)?;
    let _guard = app.projects.lock().unwrap();
    Ok(Json(
        projects::read_json(
            &path.join("edits.rev.json"),
            json!({"revision":0,"source":"","note":""}),
        )
        .map_err(ApiError::internal)?,
    ))
}
async fn save_edits(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(state): Json<Value>,
) -> ApiResult<Json<Value>> {
    if !state.is_object() {
        return Err(ApiError::bad("Expected an edits object."));
    }
    let path = project(&app, &headers, &query)?;
    let _guard = app.projects.lock().unwrap();
    idle(&app, &path)?;
    let revision = projects::save_edits(&path, state).map_err(ApiError::internal)?;
    Ok(Json(json!({"revision":revision})))
}
async fn markers(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let path = project(&app, &headers, &query)?;
    Ok(Json(
        projects::read_json(&path.join("markers.json"), json!([])).map_err(ApiError::internal)?,
    ))
}
async fn save_markers(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(markers): Json<Value>,
) -> ApiResult<Json<Value>> {
    let count = markers
        .as_array()
        .ok_or_else(|| ApiError::bad("Expected a list of markers."))?
        .len();
    let path = project(&app, &headers, &query)?;
    let _guard = app.projects.lock().unwrap();
    idle(&app, &path)?;
    projects::write_json(&path.join("markers.json"), &markers).map_err(ApiError::internal)?;
    Ok(Json(json!({"count":count})))
}
async fn job(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let scoped = headers.contains_key("x-astroimp-session") || query.contains_key("session");
    let path = project(&app, &headers, &query);
    let path = match path {
        Ok(p) => Some(p),
        Err(e) if scoped => return Err(e),
        Err(_) => None,
    };
    Ok(Json(json!(app.jobs.latest(path.as_deref().map(id)))))
}
async fn cancel(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let scoped = headers.contains_key("x-astroimp-session") || query.contains_key("session");
    let path = match project(&app, &headers, &query) {
        Ok(p) => Some(p),
        Err(e) if scoped => return Err(e),
        Err(_) => None,
    };
    Ok(Json(json!(app.jobs.cancel(
        query.get("id").map(String::as_str),
        path.as_deref().map(id)
    ))))
}

fn safe_export_name(name: &str) -> String {
    let s: String = name
        .chars()
        .take(80)
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches(['.', '-']);
    if s.is_empty() {
        "nightbloom".into()
    } else {
        s.into()
    }
}
fn export_destination(directory: &Path, name: &str, width: u32, height: u32, ext: &str) -> PathBuf {
    directory.join(format!(
        "{}-{width}x{height}-{}.{}",
        safe_export_name(name),
        Uuid::new_v4().simple(),
        ext
    ))
}
fn same_export_meta(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter().all(|(key, value)| {
                    b.get(key)
                        .is_some_and(|other| same_export_meta(value, other))
                })
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_export_meta(a, b))
        }
        // Browser JSON normalizes whole-valued floats and rounds nanosecond timestamps.
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        _ => left == right,
    }
}

async fn export_checkpoint(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let path = project(&app, &headers, &query)?;
    let _guard = app.projects.lock().unwrap();
    idle(&app, &path)?;
    let visible = projects::project_meta(&path).map_err(ApiError::internal)?;
    if !same_export_meta(&body["meta"], &visible) {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "The build changed. Reload the project before exporting.".into(),
        ));
    }
    let meta = projects::read_json(&path.join("project").join("meta.json"), Value::Null)
        .map_err(ApiError::internal)?;
    let identifier = crate::export_history::save_checkpoint(&path, &body["state"], &meta)
        .map_err(ApiError::bad)?;
    Ok(Json(json!({"checkpoint":identifier})))
}

async fn export_png(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    bytes: Bytes,
) -> ApiResult<Json<Value>> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(ApiError::bad("Expected PNG bytes."));
    }
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .map_err(|e| ApiError::bad(format!("Invalid PNG: {e}")))?;
    let path = project(&app, &headers, &query)?;
    let _guard = app.projects.lock().unwrap();
    idle(&app, &path)?;
    let name = headers
        .get("x-name")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("nightbloom")
        .trim_end_matches(".png");
    let directory = projects::export_dir(&path).map_err(ApiError::internal)?;
    let destination = export_destination(&directory, name, image.width(), image.height(), "png");
    let mut temporary = tempfile::NamedTempFile::new_in(&directory).map_err(ApiError::internal)?;
    temporary.write_all(&bytes).map_err(ApiError::internal)?;
    if let Some(identifier) = headers.get("x-export-checkpoint") {
        let identifier = identifier.to_str().map_err(ApiError::bad)?;
        crate::export_history::checkpoint(&path, identifier).map_err(ApiError::bad)?;
        temporary = crate::png_links::attach(
            temporary,
            &crate::png_links::link(path.file_name().unwrap().to_str().unwrap(), identifier),
        )
        .map_err(ApiError::bad)?;
        let filename = destination.file_name().unwrap().to_string_lossy();
        crate::export_history::record(&path, temporary.path(), &filename, identifier)
            .map_err(ApiError::bad)?;
    }
    temporary
        .persist(&destination)
        .map_err(|error| ApiError::internal(error.error))?;
    Ok(Json(json!({"path":destination.to_string_lossy()})))
}
async fn export_plan(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let path = project(&app, &headers, &query)?;
    let state = projects::edits(&path).map_err(ApiError::internal)?;
    let plan = native::plan(&path, &state, None).map_err(ApiError::bad)?;
    Ok(Json(
        serde_json::to_value(plan).map_err(ApiError::internal)?,
    ))
}
#[derive(Deserialize)]
struct ExportOptions {
    #[serde(default = "default_format")]
    format: String,
    #[serde(default = "default_quality")]
    quality: u8,
    long_side: Option<u32>,
    scale_factor: Option<f64>,
}
fn default_format() -> String {
    "png8".into()
}
fn default_quality() -> u8 {
    92
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SizeOptions {
    width: u32,
    height: u32,
    format: String,
    quality: u8,
}

fn estimated_bytes(image: &crate::contract::RenderedImage, options: &SizeOptions) -> Result<u64> {
    anyhow::ensure!(
        options.width > 0 && options.height > 0,
        "Invalid output dimensions"
    );
    let pixels = u64::from(options.width) * u64::from(options.height);
    let sample_pixels = u64::from(image.width) * u64::from(image.height);
    let encoded = render::encoded_size(image, &options.format, options.quality)?;
    if options.format == "tiff16" {
        // TIFF is uncompressed RGB16. Only the small strip directory varies with size.
        pixels
            .checked_mul(6)
            .and_then(|bytes| bytes.checked_add(encoded.saturating_sub(sample_pixels * 6)))
            .context("Output size overflow")
    } else {
        // Don't multiply the encoder's fixed overhead when scaling small images.
        let baseline = render::encoded_size(
            &crate::contract::RenderedImage {
                width: 1,
                height: 1,
                pixels: vec![0.; 3],
            },
            &options.format,
            options.quality,
        )?;
        let bytes = baseline as f64
            + encoded.saturating_sub(baseline) as f64 * pixels as f64 / sample_pixels as f64;
        anyhow::ensure!(bytes < u64::MAX as f64, "Output size overflow");
        Ok(bytes.ceil() as u64)
    }
}

async fn export_size(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(options): Json<SizeOptions>,
) -> ApiResult<Json<Value>> {
    if options.width == 0
        || options.height == 0
        || !["png8", "png16", "tiff16", "jpeg"].contains(&options.format.as_str())
        || !(1..=100).contains(&options.quality)
        || (u64::from(options.width) * u64::from(options.height))
            .checked_mul(6)
            .is_none()
    {
        return Err(ApiError::bad("Invalid image size, format or quality."));
    }
    let path = project(&app, &headers, &query)?;
    idle(&app, &path)?;
    if app
        .jobs
        .all()
        .iter()
        .any(|job| matches!(job.state.as_str(), "running" | "queued"))
    {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "File size cannot be measured while an export is running.".into(),
        ));
    }
    let permit = app
        .rendering
        .clone()
        .acquire_owned()
        .await
        .map_err(ApiError::internal)?;
    let result = tokio::task::spawn_blocking(move || -> Result<Value> {
        let _permit = permit;
        let state = projects::edits(&path)?;
        let meta = projects::read_json(&path.join("project").join("meta.json"), Value::Null)?;
        let mut cache = app.export_preview.lock().unwrap();
        if !cache.as_ref().is_some_and(|preview| {
            preview.path == path && preview.state == state && preview.meta == meta
        }) {
            // Retain at most one one-megapixel composition, not full-resolution export planes.
            let image =
                render::render_project(&path.join("project"), &state, Some(1024), &|| false)?;
            *cache = Some(ExportPreview {
                path,
                state,
                meta,
                image,
            });
        }
        let image = &cache
            .as_ref()
            .context("Missing encoded-size preview")?
            .image;
        Ok(json!({
            "estimated_bytes": estimated_bytes(image, &options)?,
            "sample_width": image.width, "sample_height": image.height,
        }))
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::internal)?;
    Ok(Json(result))
}

async fn export_full(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<ExportOptions>,
) -> ApiResult<Json<Value>> {
    if !matches!(
        body.format.as_str(),
        "png" | "png8" | "png16" | "tiff16" | "jpeg"
    ) {
        return Err(ApiError::bad("format must be png8, png16, tiff16 or jpeg."));
    }
    if body.quality == 0 || body.quality > 100 {
        return Err(ApiError::bad("JPEG quality must be between 1 and 100."));
    }
    if body.scale_factor.is_some() {
        return Err(ApiError(
            StatusCode::NOT_IMPLEMENTED,
            "scale_factor export is not supported; use long_side.".into(),
        ));
    }
    let path = project(&app, &headers, &query)?;
    let _guard = app.projects.lock().unwrap();
    idle(&app, &path)?;
    let state = projects::edits(&path).map_err(ApiError::internal)?;
    if state.is_null() {
        return Err(ApiError::bad(
            "Open this build in the editor before exporting.",
        ));
    }
    native::plan(&path, &state, body.long_side).map_err(ApiError::bad)?;
    let name = projects::payload(&path).map_err(ApiError::internal)?["name"]
        .as_str()
        .unwrap_or("nightbloom")
        .to_owned();
    let project_id = id(&path).to_owned();
    let worker_id = project_id.clone();
    let worker_name = name.clone();
    let rendering = app.rendering.clone();
    let job = app
        .jobs
        .start(&project_id, &name, move |handle| {
            run_export(handle, path, state, worker_id, worker_name, body, rendering)
        })
        .map_err(|e| ApiError(StatusCode::CONFLICT, e.to_string()))?;
    Ok(Json(json!(job)))
}
fn run_export(
    handle: JobHandle,
    path: PathBuf,
    state: Value,
    project_id: String,
    name: String,
    body: ExportOptions,
    rendering: Arc<tokio::sync::Semaphore>,
) -> Result<()> {
    let _permit = tokio::runtime::Handle::current().block_on(rendering.acquire_owned())?;
    let meta = projects::read_json(&path.join("project").join("meta.json"), Value::Null)?;
    handle.stage("render", 0., "Rendering image")?;
    let image = native::render_export(&path, &state, body.long_side, &|| handle.cancelled())?;
    handle.stage("write", 0.9, "Encoding image")?;
    let ext = match body.format.as_str() {
        "jpeg" => "jpg",
        "tiff16" => "tiff",
        _ => "png",
    };
    let directory = projects::export_dir(&path)?;
    let temporary = tempfile::NamedTempFile::new_in(&directory)?;
    let format = if body.format == "png" {
        "png8"
    } else {
        &body.format
    };
    render::write_image(&image, temporary.path(), format, body.quality)?;
    let destination = export_destination(&directory, &name, image.width, image.height, ext);
    let filename = destination
        .file_name()
        .context("Missing export filename")?
        .to_string_lossy()
        .to_string();
    handle.publish(|| {
        let checkpoint = crate::export_history::save_checkpoint(&path, &state, &meta)?;
        let temporary = crate::png_links::attach(
            temporary, &crate::png_links::link(&project_id, &checkpoint),
        )?;
        crate::export_history::record(&path, temporary.path(), &filename, &checkpoint)?;
        temporary.persist(&destination).map_err(|e| e.error)?;
        Ok(
            json!({"path":destination.to_string_lossy(),"width":image.width,"height":image.height,"checkpoint":checkpoint,
            "download_url":format!("/api/export/full/download/{filename}?session={project_id}")}),
        )
    })
}
async fn download(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    RoutePath(filename): RoutePath<String>,
) -> ApiResult<Response> {
    if filename.starts_with('.')
        || filename.contains(['/', '\\', ':'])
        || ![".png", ".tiff", ".jpg"]
            .iter()
            .any(|ext| filename.ends_with(ext))
    {
        return Err(ApiError::missing("Unknown export."));
    }
    let path = project(&app, &headers, &query)?;
    let dir = path
        .join("exports")
        .canonicalize()
        .map_err(ApiError::missing)?;
    if dir.parent() != Some(path.as_path()) {
        return Err(ApiError::missing("Unknown export."));
    }
    let file = dir
        .join(&filename)
        .canonicalize()
        .map_err(ApiError::missing)?;
    if file.parent() != Some(dir.as_path()) {
        return Err(ApiError::missing("Unknown export."));
    }
    let bytes = fs::read(file).map_err(ApiError::missing)?;
    let mime = if filename.ends_with(".jpg") {
        "image/jpeg"
    } else if filename.ends_with(".tiff") {
        "image/tiff"
    } else {
        "image/png"
    };
    Ok(([(header::CONTENT_TYPE, mime)], bytes).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn cleanup_and_project_deletion_cannot_race_an_active_job() {
        let temp = tempfile::tempdir().unwrap();
        let app = Arc::new(App {
            data: temp.path().canonicalize().unwrap(),
            samp: Samp::at(temp.path().join(".samp")),
            archive: Archive::new(temp.path()).unwrap(),
            bookmarks: Mutex::new(Bookmarks::new(temp.path())),
            previews: Previews::new(temp.path()).unwrap(),
            projects: Mutex::new(Projects::new(temp.path().to_owned()).unwrap()),
            jobs: Jobs::default(),
            thumbnails: Mutex::new(()),
            export_preview: Mutex::new(None),
            rendering: Arc::new(tokio::sync::Semaphore::new(1)),
            gallery_decoding: Arc::new(tokio::sync::Semaphore::new(1)),
        });
        let path = app.projects.lock().unwrap().create("Busy").unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        app.jobs
            .start_kind("build", true, id(&path), "Build", move |_| {
                wait.recv_timeout(std::time::Duration::from_secs(10))?;
                Ok(())
            })
            .unwrap();
        let cleanup =
            storage_delete(State(app.clone()), Json(crate::storage::Delete::default())).await;
        assert_eq!(cleanup.err().unwrap().0, StatusCode::CONFLICT);
        let deletion = delete_project(State(app.clone()), RoutePath(id(&path).to_owned())).await;
        assert_eq!(deletion.err().unwrap().0, StatusCode::CONFLICT);
        assert!(path.join("catalog.json").is_file());
        release.send(()).unwrap();
        app.jobs.cancel_and_wait();
    }

    #[test]
    fn loopback_protection_rejects_rebinding() {
        for value in ["127.0.0.1:8876", "localhost", "[::1]:8876"] {
            assert!(loopback_authority(value));
        }
        for value in [
            "127.0.0.1.evil:8876",
            "localhost@evil",
            "localhost:bad",
            "evil",
            "[::1]evil",
        ] {
            assert!(!loopback_authority(value));
        }
    }

    #[tokio::test]
    async fn size_estimates_follow_encoding_quality_and_current_project_edits() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sessions").join("measured");
        let project_dir = path.join("project");
        fs::create_dir_all(&project_dir).unwrap();
        projects::write_json(&path.join("catalog.json"), &json!({})).unwrap();
        projects::write_json(
            &project_dir.join("meta.json"),
            &json!({
                "width": 512, "height": 256,
                "layers": [{"key": "a", "file": "a.f16", "wavelength_nm": 550}]
            }),
        )
        .unwrap();
        let plane: Vec<u8> = (0..512 * 256)
            .flat_map(|i| {
                let value = ((i * 7919 % 65521) as f32 / 65521.).sqrt();
                half::f16::from_f32(value).to_bits().to_le_bytes()
            })
            .collect();
        fs::write(project_dir.join("a.f16"), plane).unwrap();
        let state = json!({"layers": [{"source": "a", "color": "#ffffff",
                "stretch": "linear", "blend": "add", "mode": "tint"}], "global": {}});
        projects::write_json(&path.join("edits.json"), &state).unwrap();
        let app = Arc::new(App {
            data: temp.path().canonicalize().unwrap(),
            samp: Samp::at(temp.path().join(".samp")),
            archive: Archive::new(temp.path()).unwrap(),
            bookmarks: Mutex::new(Bookmarks::new(temp.path())),
            previews: Previews::new(temp.path()).unwrap(),
            projects: Mutex::new(Projects::new(temp.path().to_owned()).unwrap()),
            jobs: Jobs::default(),
            thumbnails: Mutex::new(()),
            export_preview: Mutex::new(None),
            rendering: Arc::new(tokio::sync::Semaphore::new(1)),
            gallery_decoding: Arc::new(tokio::sync::Semaphore::new(1)),
        });
        let mut headers = HeaderMap::new();
        headers.insert("x-astroimp-session", "measured".parse().unwrap());
        let request = |format: &str, quality| SizeOptions {
            width: 512,
            height: 256,
            format: format.to_owned(),
            quality,
        };
        let measure = async |options| {
            export_size(
                State(app.clone()),
                headers.clone(),
                Query(HashMap::new()),
                Json(options),
            )
            .await
            .unwrap_or_else(|error| panic!("{}", error.1))
            .0["estimated_bytes"]
                .as_u64()
                .unwrap()
        };
        let high = measure(request("jpeg", 95)).await;
        let low = measure(request("jpeg", 20)).await;
        assert!(high > low * 2, "{high} vs {low}");
        let textured = measure(request("png16", 92)).await;
        projects::write_json(
            &path.join("edits.json"),
            &json!({"layers": [], "global": {}}),
        )
        .unwrap();
        let empty = measure(request("png16", 92)).await;
        assert!(textured > empty * 10, "{textured} vs {empty}");
        assert!(!path.join("exports").exists());
    }

    #[test]
    fn tiff_budget_estimate_matches_actual_larger_rgb16_file() {
        use crate::contract::RenderedImage;
        let sample = RenderedImage {
            width: 512,
            height: 256,
            pixels: vec![0.25; 512 * 256 * 3],
        };
        let options = SizeOptions {
            width: 2048,
            height: 1024,
            format: "tiff16".into(),
            quality: 92,
        };
        let expected = estimated_bytes(&sample, &options).unwrap();
        let target = RenderedImage {
            width: options.width,
            height: options.height,
            pixels: vec![0.25; options.width as usize * options.height as usize * 3],
        };
        let file = tempfile::NamedTempFile::new().unwrap();
        render::write_image(&target, file.path(), &options.format, options.quality).unwrap();
        let actual = file.as_file().metadata().unwrap().len();
        assert!(expected.abs_diff(actual) < 1024, "{expected} vs {actual}");
    }

    #[test]
    fn small_jpeg_budget_does_not_scale_fixed_encoder_overhead() {
        use crate::contract::RenderedImage;
        let sample = RenderedImage {
            width: 8,
            height: 8,
            pixels: vec![0.25; 8 * 8 * 3],
        };
        let options = SizeOptions {
            width: 16,
            height: 16,
            format: "jpeg".into(),
            quality: 92,
        };
        let expected = estimated_bytes(&sample, &options).unwrap();
        let image = RenderedImage {
            width: 16,
            height: 16,
            pixels: vec![0.25; 16 * 16 * 3],
        };
        let file = tempfile::NamedTempFile::new().unwrap();
        render::write_image(&image, file.path(), &options.format, options.quality).unwrap();
        let actual = file.as_file().metadata().unwrap().len();
        assert!(expected.abs_diff(actual) < 100, "{expected} vs {actual}");
    }
    #[tokio::test]
    async fn preview_upload_uses_its_explicit_snapshot_even_after_newer_edits_arrive() {
        let temporary = tempfile::tempdir().unwrap();
        let session = temporary.path().join("sessions/first");
        projects::write_json(&session.join("catalog.json"), &json!({"groups":[]})).unwrap();
        let session = session.canonicalize().unwrap();
        let meta = json!({"width":16,"height":8,"layers":[],
            "wcs":{"CD1_1":1.0,"CD1_2":0.0}});
        projects::write_json(&session.join("project/meta.json"), &meta).unwrap();
        let original = json!({"version":1,"layers":[],"global":{"output":{"exposure":0}}});
        let app = router(
            temporary.path().to_owned(),
            temporary.path().join("frontend"),
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder()
            .default_headers(HeaderMap::from_iter([
                (header::ORIGIN, origin.parse().unwrap()),
                (
                    axum::http::HeaderName::from_static("x-astroimp-session"),
                    "first".parse().unwrap(),
                ),
            ]))
            .build()
            .unwrap();
        let mut browser_meta = meta.clone();
        browser_meta["wcs"]["CD1_1"] = json!(1);
        browser_meta["wcs"]["CD1_2"] = json!(0);
        let saved: Value = client
            .post(format!("{origin}/api/export/checkpoint"))
            .json(&json!({"state":original,"meta":browser_meta}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let identifier = saved["checkpoint"].as_str().unwrap();
        let newer = json!({"version":1,"layers":[],"global":{"output":{"exposure":1}}});
        projects::write_json(&session.join("edits.json"), &newer).unwrap();
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(4, 3)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let uploaded: Value = client
            .post(format!("{origin}/api/export"))
            .header("x-export-checkpoint", identifier)
            .body(png.get_ref().clone())
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let path = PathBuf::from(uploaded["path"].as_str().unwrap());
        let link = crate::png_links::link("first", identifier);
        assert_eq!(crate::png_links::read(&path).unwrap(), Some(link.clone()));
        let bytes = fs::read(&path).unwrap();
        let extra = crate::png_links::chunk(&link).unwrap().len();
        assert_eq!(&bytes[..33], &png.get_ref()[..33]);
        assert_eq!(&bytes[33 + extra..], &png.get_ref()[33..]);
        let history = crate::export_history::records(&session).unwrap();
        assert_eq!(
            history[path.file_name().unwrap().to_str().unwrap()]["checkpoint"],
            identifier
        );
        assert_eq!(
            crate::export_history::checkpoint(&session, identifier).unwrap()["state"],
            original
        );
        assert_eq!(
            crate::export_history::checkpoint(&session, identifier).unwrap()["meta"],
            meta
        );
        assert_eq!(
            projects::read_json(&session.join("edits.json"), Value::Null).unwrap(),
            newer
        );
        let restored: Value = client
            .post(format!("{origin}/api/gallery/first/restore"))
            .json(&json!({"filename":path.file_name().unwrap().to_str().unwrap()}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(restored["edits"]["global"], original["global"]);
        let returning = restored["edits"]["checkpoints"][0]["exportCheckpoint"]
            .as_str()
            .unwrap();
        let returned: Value = client
            .post(format!(
                "{origin}/api/sessions/first/export-checkpoints/{returning}/restore"
            ))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(returned["edits"]["global"], newer["global"]);
        let legacy = client
            .post(format!("{origin}/api/export"))
            .body(png.into_inner())
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        let legacy = Path::new(legacy["path"].as_str().unwrap())
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            crate::export_history::records(&session)
                .unwrap()
                .get(legacy)
                .is_none()
        );
        server.abort();
    }

    #[tokio::test]
    async fn gallery_http_preserves_originals_and_ignores_the_editing_project() {
        let temp = tempfile::tempdir().unwrap();
        for id in ["first", "second"] {
            projects::write_json(
                &temp.path().join("sessions").join(id).join("catalog.json"),
                &json!({"groups":[]}),
            )
            .unwrap();
        }
        let exports = temp.path().join("sessions/first/exports/earlier");
        fs::create_dir_all(&exports).unwrap();
        let source = exports.join("study α.tiff");
        image::ImageBuffer::from_pixel(13, 7, image::Rgb([16384u16, 32768, 49152]))
            .save(&source)
            .unwrap();
        let original = fs::read(&source).unwrap();
        let app = router(temp.path().to_owned(), temp.path().join("frontend")).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::new();
        let inventory: Value = client
            .get(format!("{origin}/api/gallery"))
            .header("X-AstroImp-Session", "second")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(inventory[0]["id"], "first");
        assert_eq!(
            inventory[0]["images"][0]["filename"],
            "earlier/study α.tiff"
        );
        for view in ["original", "clipboard", "preview"] {
            let response = client
                .get(format!(
                    "{origin}/api/gallery/first/earlier/study%20%CE%B1.tiff?view={view}"
                ))
                .header("X-AstroImp-Session", "second")
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
            if view == "original" {
                assert!(
                    response.headers()["content-disposition"]
                        .to_str()
                        .unwrap()
                        .contains("attachment")
                );
                assert_eq!(
                    response.bytes().await.unwrap().as_ref(),
                    original.as_slice()
                );
            } else {
                let expected = if view == "clipboard" {
                    "image/png"
                } else {
                    "image/jpeg"
                };
                assert_eq!(response.headers()["content-type"], expected);
                let bytes = response.bytes().await.unwrap();
                let decoded = image::load_from_memory(&bytes).unwrap().to_rgb8();
                assert_eq!(decoded.dimensions(), (13, 7));
            }
        }
        assert_eq!(fs::read(source).unwrap(), original);
        assert!(!temp.path().join("sessions/first/project").exists());
        server.abort();
    }

    #[tokio::test]
    async fn home_thumbnails_are_scoped_cached_and_refresh_from_readable_artwork() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("sessions").join("first");
        let second = temp.path().join("sessions").join("second");
        for session in [&first, &second] {
            fs::create_dir_all(session.join("exports")).unwrap();
            projects::write_json(&session.join("catalog.json"), &json!({"groups":[]})).unwrap();
            image::RgbImage::from_pixel(800, 400, image::Rgb([255, 0, 0]))
                .save(session.join("snapshot.jpg"))
                .unwrap();
        }
        let export = first.join("exports").join("old.png");
        image::ImageBuffer::from_pixel(800, 400, image::Rgb([16384u16, 32768, 49152]))
            .save(&export)
            .unwrap();
        let stamp =
            |path: &Path, seconds| {
                fs::OpenOptions::new()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_times(fs::FileTimes::new().set_modified(
                        std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds),
                    ))
                    .unwrap();
            };
        stamp(&export, 100);
        let app = router(temp.path().to_path_buf(), temp.path().join("frontend")).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        async fn request(address: std::net::SocketAddr, id: &str) -> (String, Vec<u8>) {
            let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
            socket
                .write_all(
                    format!(
                        "GET /api/sessions/{id}/thumbnail.jpg?refresh=1 HTTP/1.1\r\n\
                         Host: localhost\r\nConnection: close\r\nX-AstroImp-Session: second\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let mut response = Vec::new();
            socket.read_to_end(&mut response).await.unwrap();
            let split = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            (
                String::from_utf8(response[..split].to_vec()).unwrap(),
                response[split + 4..].to_vec(),
            )
        }
        let artwork = |headers: String, bytes: Vec<u8>, expected: [u8; 3]| {
            assert!(headers.contains("200 OK"), "{headers}");
            assert!(headers.contains("content-type: image/jpeg"), "{headers}");
            assert!(headers.contains("cache-control: no-cache"), "{headers}");
            let image = image::load_from_memory(&bytes).unwrap().to_rgb8();
            assert_eq!(image.dimensions(), (480, 240));
            for (actual, expected) in image.get_pixel(240, 120).0.into_iter().zip(expected) {
                assert!(actual.abs_diff(expected) <= 2, "{actual} != {expected}");
            }
        };
        let (headers, bytes) = request(address, "first").await;
        artwork(headers, bytes, [64, 128, 192]);
        let modified = fs::metadata(first.join("thumb.jpg"))
            .unwrap()
            .modified()
            .unwrap();
        let (headers, bytes) = request(address, "first").await;
        artwork(headers, bytes, [64, 128, 192]);
        assert_eq!(
            fs::metadata(first.join("thumb.jpg"))
                .unwrap()
                .modified()
                .unwrap(),
            modified
        );
        let newest = first.join("exports").join("new.tiff");
        image::ImageBuffer::from_pixel(800, 400, image::Rgb([49152u16, 32768, 16384]))
            .save(&newest)
            .unwrap();
        stamp(&newest, 200);
        fs::write(first.join("exports").join("broken.png"), b"incomplete").unwrap();
        fs::create_dir_all(first.join("exports").join(".partial")).unwrap();
        image::RgbImage::from_pixel(800, 400, image::Rgb([0, 255, 0]))
            .save(first.join("exports").join(".partial").join("work.png"))
            .unwrap();
        let (headers, bytes) = request(address, "first").await;
        artwork(headers, bytes, [192, 128, 64]);
        let (headers, bytes) = request(address, "second").await;
        artwork(headers, bytes, [255, 0, 0]);
        fs::remove_dir_all(first.join("exports")).unwrap();
        let (headers, bytes) = request(address, "first").await;
        artwork(headers, bytes, [255, 0, 0]);
        fs::remove_file(first.join("snapshot.jpg")).unwrap();
        assert!(request(address, "first").await.0.contains("404 Not Found"));
        assert!(
            request(address, "unknown")
                .await
                .0
                .contains("404 Not Found")
        );
        let coordinates = temp.path().join("sessions").join("coordinates");
        projects::write_json(&coordinates.join("catalog.json"), &json!({"groups":[]})).unwrap();
        projects::write_json(
            &coordinates.join("view.json"),
            &json!({"source":"coordinates","pixel_dimensions":[1200,800],
                "center_icrs_deg":[83.8,-5.4],"field_arcmin":[20,13.3]}),
        )
        .unwrap();
        let (headers, bytes) = request(address, "coordinates").await;
        assert!(headers.contains("200 OK"), "{headers}");
        let grid = image::load_from_memory(&bytes).unwrap().to_rgb8();
        assert_eq!(grid.dimensions(), (480, 320));
        let center = grid.get_pixel(240, 160).0;
        assert!(center[1] > center[0] + 80);
        server.abort();
    }

    #[tokio::test]
    async fn global_bookmarks_use_existing_files_and_validate_mutations() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("sessions").join("source");
        let row = json!({"obs_id":"one","collection":"HST","instrument":"WFC3","filter":"F656N",
            "target":"Field","wavelength_nm":656,"ra":12,"dec":20,
            "region":"POLYGON ICRS 11.99 19.99 12.01 19.99 12.01 20.01 11.99 20.01",
            "preview_uri":null});
        let mut survey = row.clone();
        survey["collection"] = json!("Survey");
        survey["obs_id"] = json!("survey");
        projects::write_json(
            &source.join("catalog.json"),
            &json!({"groups":[{"observations":[row,survey]}]}),
        )
        .unwrap();
        projects::write_json(
            &source.join("session.json"),
            &json!({"name":"Original field"}),
        )
        .unwrap();
        let saved = json!({"id":"legacy","obs_id":"old","collection":"JWST","instrument":"NIRCam",
            "filter":"F444W","target":"Field","wavelength_nm":4400,"region":"",
            "center_icrs_deg":[12,20],"size_arcmin":[1,1],"preview":null,
            "source_session":"deleted","source_name":"Earlier field","note":"Saved before import",
            "created":"2026-10-01T00:00:00+00:00"});
        projects::write_json(&temp.path().join("bookmarks.json"), &json!([saved])).unwrap();
        let app = router(temp.path().to_owned(), temp.path().join("frontend")).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let request = async |method: &str, path: &str, body: Value| {
            let response = client
                .request(
                    method.parse::<reqwest::Method>().unwrap(),
                    format!("http://{address}{path}"),
                )
                .header("Content-Type", "application/json")
                .header("X-AstroImp-Session", "nonexistent")
                .body(body.to_string())
                .send()
                .await
                .unwrap();
            let code = response.status().as_u16();
            let value: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
            (code, value)
        };
        assert_eq!(
            request("GET", "/api/bookmarks", Value::Null).await,
            (200, json!([saved]))
        );
        let (code, items) = request(
            "POST",
            "/api/bookmarks",
            json!({"session":"source","obs_ids":["one","one"]}),
        )
        .await;
        assert_eq!(code, 200);
        assert_eq!(items.as_array().unwrap().len(), 2);
        let identifier = items[1]["id"].as_str().unwrap();
        assert_eq!(items[1]["source_name"], "Original field");
        for ids in [vec!["missing"], vec!["survey"]] {
            assert_eq!(
                request(
                    "POST",
                    "/api/bookmarks",
                    json!({"session":"source","obs_ids":ids})
                )
                .await
                .0,
                400
            );
        }
        let note_route = format!("/api/bookmarks/{identifier}");
        assert_eq!(
            request("PATCH", &note_route, json!({"note":"x".repeat(201)}))
                .await
                .0,
            422
        );
        assert_eq!(
            request("PATCH", &note_route, json!({"note":"Keep"}))
                .await
                .1["note"],
            "Keep"
        );
        let rejected = client
            .delete(format!("http://{address}{note_route}"))
            .header("Origin", "https://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(rejected.status().as_u16(), 403);
        fs::remove_dir_all(&source).unwrap();
        let (code, frame) = request(
            "POST",
            "/api/bookmarks/frame",
            json!({"ids":["legacy",identifier,identifier],"aspect":"9:16"}),
        )
        .await;
        assert_eq!(code, 200, "{frame}");
        assert_eq!(frame["aspect"], "9:16");
        assert!(frame["ra"].as_f64().unwrap().is_finite());
        assert_eq!(
            request("GET", &format!("{note_route}/preview.jpg"), Value::Null)
                .await
                .0,
            404
        );
        assert_eq!(
            request("GET", "/api/bookmarks", Value::Null)
                .await
                .1
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            request("DELETE", "/api/bookmarks/legacy", Value::Null)
                .await
                .0,
            200
        );
        assert_eq!(
            request("DELETE", "/api/bookmarks/legacy", Value::Null)
                .await
                .0,
            404
        );
        let persisted =
            projects::read_json(&temp.path().join("bookmarks.json"), Value::Null).unwrap();
        assert_eq!(persisted.as_array().unwrap().len(), 1);
        assert_eq!(persisted[0]["note"], "Keep");
        server.abort();
    }

    #[tokio::test]
    async fn scoped_http_saves_and_switches_are_isolated() {
        let temp = tempfile::tempdir().unwrap();
        for id in ["first", "second"] {
            let session = temp.path().join("sessions").join(id);
            fs::create_dir_all(session.join("project")).unwrap();
            projects::write_json(&session.join("catalog.json"), &json!({"groups":[]})).unwrap();
            projects::write_json(
                &session.join("project/meta.json"),
                &json!({"width":1,"height":1,"layers":[{"key":"a","file":"a.f16"}],"stars":{}}),
            )
            .unwrap();
        }
        let app = router(temp.path().to_path_buf(), temp.path().join("frontend")).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        async fn request(
            address: std::net::SocketAddr,
            method: &str,
            path: &str,
            extra: &str,
            body: &str,
        ) -> String {
            let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
            let request = format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}\r\n{body}",
                body.len()
            );
            socket.write_all(request.as_bytes()).await.unwrap();
            let mut response = Vec::new();
            socket.read_to_end(&mut response).await.unwrap();
            String::from_utf8(response).unwrap()
        }
        assert!(
            request(address, "POST", "/api/sessions/first/open", "", "")
                .await
                .contains("200 OK")
        );
        assert!(
            request(address, "POST", "/api/sessions/second/open", "", "")
                .await
                .contains("200 OK")
        );
        let body = r#"{"layers":[],"global":{"stars":{"separate":true}},"checkpoints":[{"name":"saved"}]}"#;
        let saved = request(
            address,
            "PUT",
            "/api/edits",
            "X-AstroImp-Session: first\r\n",
            body,
        )
        .await;
        assert!(saved.contains(r#""revision":1"#), "{saved}");
        let first = request(address, "GET", "/api/edits?session=first", "", "").await;
        assert!(!first.contains(r#""stars""#), "{first}");
        assert!(first.contains("saved"));
        let second = request(address, "GET", "/api/edits?session=second", "", "").await;
        assert!(second.ends_with("null"), "{second}");
        let renamed = request(
            address,
            "PATCH",
            "/api/session",
            "X-AstroImp-Session: first\r\n",
            r#"{"name":"Renamed"}"#,
        )
        .await;
        assert!(renamed.contains(r#""name":"Renamed""#), "{renamed}");
        let markers = request(
            address,
            "PUT",
            "/api/markers",
            "X-AstroImp-Session: first\r\n",
            r#"[{"n":1}]"#,
        )
        .await;
        assert!(markers.contains(r#""count":1"#), "{markers}");
        let session = request(address, "GET", "/api/session?session=second", "", "").await;
        assert!(!session.contains("Renamed"), "{session}");
        let prohibited = request(
            address,
            "GET",
            "/api/project/secret.txt?session=first",
            "",
            "",
        )
        .await;
        assert!(prohibited.contains("404 Not Found"), "{prohibited}");
        let unsupported = request(address, "POST", "/api/probe", "", "").await;
        assert!(unsupported.contains("501 Not Implemented"), "{unsupported}");
        let forbidden = request(
            address,
            "POST",
            "/api/job/cancel",
            "Origin: http://evil.example\r\n",
            "",
        )
        .await;
        assert!(forbidden.contains("403 Forbidden"), "{forbidden}");
        fs::write(temp.path().join("downloads/unused.fits"), b"unused").unwrap();
        let inventory = request(address, "GET", "/api/storage", "", "").await;
        assert!(inventory.contains(r#""status":"orphan""#), "{inventory}");
        let deleted = request(address, "DELETE", "/api/sessions/first", "", "").await;
        assert!(deleted.contains(r#""current":"second""#), "{deleted}");
        assert!(!temp.path().join("sessions/first").exists());
        assert!(
            temp.path()
                .join("sessions/second/project/meta.json")
                .exists()
        );
        let trash = fs::read_dir(temp.path().join("sessions/.trash"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .file_name();
        let trashed = temp.path().join("sessions/.trash").join(&trash);
        assert!(trashed.join("edits.json").exists());
        let body =
            json!({"downloads":["unused.fits"],"trash":[trash.to_string_lossy()]}).to_string();
        let cleaned = request(address, "POST", "/api/storage/delete", "", &body).await;
        assert!(cleaned.contains("200 OK"), "{cleaned}");
        assert!(!trashed.exists());
        assert!(!temp.path().join("downloads/unused.fits").exists());
        let deleted = request(address, "DELETE", "/api/sessions/second", "", "").await;
        assert!(deleted.contains(r#""current":null"#), "{deleted}");
        assert!(
            Projects::new(temp.path().to_owned())
                .unwrap()
                .current(None)
                .is_err()
        );
        server.abort();
    }
}
