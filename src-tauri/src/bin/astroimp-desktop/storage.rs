use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use super::library;

struct Storage {
    root: PathBuf,
    settings: Option<PathBuf>,
    protected: PathBuf,
}

struct Error(anyhow::Error);
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        eprintln!("Desktop storage: {:#}", self.0);
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"detail":format!("{:#}",self.0)})),
        )
            .into_response()
    }
}
impl From<anyhow::Error> for Error {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

fn display(path: &Path) -> String {
    super::support::display_path(path)
        .to_string_lossy()
        .into_owned()
}

fn location(storage: &Storage) -> Result<Value> {
    let pending = storage
        .settings
        .as_deref()
        .map(library::read_settings)
        .transpose()?
        .flatten()
        .and_then(|settings| settings.pending_root);
    Ok(json!({
        "root": display(&storage.root),
        "pending_root": pending.as_deref().map(display),
        "can_change": storage.settings.is_some(),
        "available_bytes": library::available_bytes(&storage.root)?,
    }))
}

pub fn missing_sources(root: &Path) -> Result<Vec<String>> {
    fn sources(value: &Value, session: &Path, missing: &mut BTreeSet<String>) -> Result<()> {
        match value {
            Value::Object(items) => {
                if let Some(source) = items.get("source").and_then(Value::as_str) {
                    let path = astroimp_desktop::projects::science_source(session, source)?;
                    if !path.is_file() {
                        missing.insert(display(&path));
                    }
                }
                for value in items.values() {
                    sources(value, session, missing)?;
                }
            }
            Value::Array(items) => {
                for value in items {
                    sources(value, session, missing)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn walk(path: &Path, missing: &mut BTreeSet<String>) -> Result<()> {
        if !path.is_dir() {
            return Ok(());
        }
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            ensure!(!kind.is_symlink(), "A library must not contain links");
            if kind.is_dir() {
                walk(&entry.path(), missing)?;
            } else if entry.file_name() == "meta.json"
                && path.file_name().is_some_and(|n| n == "project")
            {
                let value = astroimp_desktop::projects::read_json(&entry.path(), Value::Null)?;
                sources(
                    &value,
                    path.parent().context("Invalid project folder")?,
                    missing,
                )?;
            }
        }
        Ok(())
    }
    let mut missing = BTreeSet::new();
    walk(&root.join("sessions"), &mut missing)?;
    Ok(missing.into_iter().collect())
}

async fn get_location(State(storage): State<Arc<Storage>>) -> Result<Json<Value>, Error> {
    Ok(Json(location(&storage)?))
}

#[derive(Deserialize)]
struct Change {
    path: PathBuf,
}
async fn change_location(
    State(storage): State<Arc<Storage>>,
    Json(change): Json<Change>,
) -> Result<Json<Value>, Error> {
    Ok(Json(schedule_change(&storage, &change.path)?))
}

fn schedule_change(storage: &Storage, path: &Path) -> Result<Value> {
    let settings_file = storage
        .settings
        .as_deref()
        .context("This launch uses --data; change that argument instead.")?;
    let destination = library::validate_root(path, &storage.protected)?;
    let mut settings =
        library::read_settings(settings_file)?.context("Desktop storage settings are missing")?;
    ensure!(
        settings.library_root.canonicalize()? == storage.root,
        "Storage settings changed outside this window. Restart NightBloom first."
    );
    if destination == storage.root {
        settings.pending_root = None;
    } else {
        ensure!(
            !destination.starts_with(&storage.root) && !storage.root.starts_with(&destination),
            "The old and new libraries must not contain each other."
        );
        if destination.exists() {
            ensure!(
                fs::read_dir(&destination)?.next().is_none(),
                "Choose an empty folder. Existing files will not be overwritten."
            );
        }
        settings.pending_root = Some(destination);
    }
    library::save_settings(settings_file, &settings)?;
    location(storage)
}

async fn pick_folder(State(storage): State<Arc<Storage>>) -> Result<Json<Value>, Error> {
    let path = tokio::task::spawn_blocking(move || {
        rfd::FileDialog::new()
            .set_title("Choose NightBloom library folder")
            .set_directory(super::support::display_path(&storage.root))
            .pick_folder()
    })
    .await
    .context("Choosing the library folder")?;
    Ok(Json(json!({"path":path.as_deref().map(display)})))
}

pub fn router(root: PathBuf, settings: Option<PathBuf>, protected: PathBuf) -> Result<Router> {
    Ok(Router::new()
        .route(
            "/api/desktop/storage",
            get(get_location).post(change_location),
        )
        .route("/api/desktop/storage/pick", post(pick_folder))
        .layer(middleware::from_fn(astroimp_desktop::server::loopback_only))
        .with_state(Arc::new(Storage {
            root: root.canonicalize()?,
            settings,
            protected,
        })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choosing_a_folder_schedules_a_transfer_without_switching_or_overwriting() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("library");
        fs::create_dir_all(root.join("sessions/a/exports")).unwrap();
        fs::write(root.join("sessions/a/exports/art.png"), b"keep").unwrap();
        let settings_file = tmp.path().join("settings.json");
        library::save_settings(
            &settings_file,
            &library::Settings {
                library_root: root.clone(),
                pending_root: None,
            },
        )
        .unwrap();
        let storage = Storage {
            root: root.canonicalize().unwrap(),
            settings: Some(settings_file.clone()),
            protected: tmp.path().join("protected"),
        };
        let destination = tmp.path().join("new-library");
        let result = schedule_change(&storage, &destination).unwrap();
        assert!(result["pending_root"].is_string());
        assert!(root.join("sessions/a/exports/art.png").is_file());
        assert!(!destination.exists());
        schedule_change(&storage, &root).unwrap();
        assert!(
            library::read_settings(&settings_file)
                .unwrap()
                .unwrap()
                .pending_root
                .is_none()
        );
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("unrelated"), b"mine").unwrap();
        assert!(schedule_change(&storage, &destination).is_err());
        assert_eq!(fs::read(destination.join("unrelated")).unwrap(), b"mine");
        let inventory = astroimp_desktop::storage::inventory(&root).unwrap();
        assert_eq!(inventory["totals"]["exports"], 4);
        assert_eq!(inventory["can_cleanup"], true);
    }

    #[tokio::test]
    async fn storage_http_rejects_foreign_origins_and_accepts_explicit_root_changes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("library");
        fs::create_dir_all(root.join("sessions")).unwrap();
        let settings = tmp.path().join("settings.json");
        library::save_settings(
            &settings,
            &library::Settings {
                library_root: root.clone(),
                pending_root: None,
            },
        )
        .unwrap();
        let app = router(
            root.clone(),
            Some(settings.clone()),
            tmp.path().join("protected"),
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for (origin, expected) in [
            ("https://unrelated.example", "403"),
            ("http://127.0.0.1", "200"),
        ] {
            let body = json!({"path":tmp.path().join("new")}).to_string();
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            stream.write_all(format!(
                "POST /api/desktop/storage HTTP/1.1\r\nHost: {address}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()
            ).as_bytes()).await.unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            assert!(
                response.starts_with(&format!("HTTP/1.1 {expected}")),
                "{response}"
            );
        }
        assert_eq!(
            library::read_settings(&settings)
                .unwrap()
                .unwrap()
                .library_root,
            root
        );
        server.abort();
    }
}
