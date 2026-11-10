use anyhow::{Result, ensure};
use astroimp_desktop::projects::{Projects, export_file};
use axum::{
    Json, Router,
    extract::{Path as RoutePath, State},
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::post,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

type Opener = dyn Fn(&Path, bool) -> Result<()> + Send + Sync;
struct Files {
    projects: Projects,
    opener: Arc<Opener>,
}

#[derive(Debug)]
struct Error(StatusCode, anyhow::Error);
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        eprintln!("Desktop files: {:#}", self.1);
        (self.0, Json(json!({"detail": format!("{:#}", self.1)}))).into_response()
    }
}

async fn open(files: Arc<Files>, path: PathBuf, select: bool) -> Result<Json<Value>, Error> {
    tokio::task::spawn_blocking(move || (files.opener)(&path, select))
        .await
        .map_err(|error| Error(StatusCode::INTERNAL_SERVER_ERROR, error.into()))?
        .map_err(|error| Error(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok(Json(json!({"opened": true})))
}

async fn folder(
    State(files): State<Arc<Files>>,
    RoutePath(id): RoutePath<String>,
) -> Result<Json<Value>, Error> {
    let path = files
        .projects
        .resolve_readonly(&id)
        .map_err(|error| Error(StatusCode::BAD_REQUEST, error))?;
    open(files, path, false).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRequest {
    filename: String,
}

async fn reveal(
    State(files): State<Arc<Files>>,
    RoutePath(id): RoutePath<String>,
    Json(body): Json<FileRequest>,
) -> Result<Json<Value>, Error> {
    let path = files
        .projects
        .resolve_readonly(&id)
        .and_then(|project| export_file(&project, &body.filename))
        .map_err(|error| Error(StatusCode::BAD_REQUEST, error))?;
    open(files, path, true).await
}

async fn image(
    State(files): State<Arc<Files>>,
    RoutePath(id): RoutePath<String>,
    Json(body): Json<FileRequest>,
) -> Result<Json<Value>, Error> {
    let path =
        files
            .projects
            .resolve_readonly(&id)
            .and_then(|project| export_file(&project, &body.filename))
            .and_then(|path| {
                let extension = path
                    .extension()
                    .and_then(|value| value.to_str())
                    .map(str::to_ascii_lowercase);
                ensure!(extension.as_deref().is_some_and(|value|
                ["png", "jpg", "jpeg", "tif", "tiff"].contains(&value)),
                "Only exported PNG, JPEG and TIFF images can be opened");
                Ok(path)
            })
            .map_err(|error| Error(StatusCode::BAD_REQUEST, error))?;
    open(files, path, false).await
}

pub fn router(root: PathBuf) -> Result<Router> {
    let root = root.canonicalize()?;
    let projects = Projects::new(root.clone())?;
    ensure!(
        root.join("sessions").canonicalize()?.parent() == Some(root.as_path()),
        "Unsafe projects folder"
    );
    Ok(Router::new()
        .route("/api/desktop/projects/{id}/folder", post(folder))
        .route("/api/desktop/projects/{id}/reveal", post(reveal))
        .route("/api/desktop/projects/{id}/open-image", post(image))
        .layer(middleware::from_fn(astroimp_desktop::server::loopback_only))
        .with_state(Arc::new(Files {
            projects,
            opener: Arc::new(platform::open),
        })))
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::{ffi::c_void, os::windows::ffi::OsStrExt, ptr};

    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoInitializeEx(reserved: *mut c_void, flags: u32) -> i32;
        fn CoUninitialize();
        fn CoTaskMemFree(pointer: *mut c_void);
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHParseDisplayName(
            name: *const u16,
            bind: *mut c_void,
            pidl: *mut *mut c_void,
            attributes: u32,
            output: *mut u32,
        ) -> i32;
        fn SHOpenFolderAndSelectItems(
            pidl: *const c_void,
            count: u32,
            children: *const *const c_void,
            flags: u32,
        ) -> i32;
        fn ShellExecuteW(
            window: *mut c_void,
            verb: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> isize;
    }

    struct Apartment(bool);
    impl Drop for Apartment {
        fn drop(&mut self) {
            if self.0 {
                unsafe {
                    CoUninitialize();
                }
            }
        }
    }
    struct Item(*mut c_void);
    impl Drop for Item {
        fn drop(&mut self) {
            unsafe {
                CoTaskMemFree(self.0);
            }
        }
    }

    pub fn open(path: &Path, select: bool) -> Result<()> {
        let initialized = unsafe { CoInitializeEx(ptr::null_mut(), 2) };
        // A reused blocking worker may already have a different, valid COM apartment.
        ensure!(
            initialized >= 0 || initialized as u32 == 0x80010106,
            "Initializing the Windows shell failed (0x{:08X})",
            initialized as u32
        );
        let _apartment = Apartment(initialized >= 0);
        let path: Vec<u16> = super::super::support::display_path(path)
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        if select {
            let mut item = Item(ptr::null_mut());
            let parsed = unsafe {
                SHParseDisplayName(
                    path.as_ptr(),
                    ptr::null_mut(),
                    &mut item.0,
                    0,
                    ptr::null_mut(),
                )
            };
            ensure!(
                parsed >= 0 && !item.0.is_null(),
                "Finding export in File Explorer failed (0x{:08X})",
                parsed as u32
            );
            // With count=0, a fully qualified item opens its parent and selects that file.
            let opened = unsafe { SHOpenFolderAndSelectItems(item.0, 0, ptr::null(), 0) };
            ensure!(
                opened >= 0,
                "Selecting export in File Explorer failed (0x{:08X})",
                opened as u32
            );
        } else {
            let verb: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
            let opened = unsafe {
                ShellExecuteW(
                    ptr::null_mut(),
                    verb.as_ptr(),
                    path.as_ptr(),
                    ptr::null(),
                    ptr::null(),
                    1,
                )
            };
            ensure!(
                opened > 32,
                "Windows could not open the file or folder (code {opened}); check its default app"
            );
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;
    pub fn open(_path: &Path, _select: bool) -> Result<()> {
        anyhow::bail!("Opening project files is currently supported only on Windows")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, sync::Mutex};

    #[tokio::test]
    async fn actions_are_explicitly_project_scoped_and_reject_non_export_paths() {
        let temp = tempfile::tempdir().unwrap();
        for id in ["first", "second"] {
            let project = temp.path().join("sessions").join(id);
            fs::create_dir_all(project.join("exports")).unwrap();
            fs::write(project.join("catalog.json"), "{}").unwrap();
            fs::write(project.join("exports").join("art, one.png"), b"image").unwrap();
        }
        let calls = Arc::new(Mutex::new(Vec::new()));
        let captured = calls.clone();
        let files = Arc::new(Files {
            projects: Projects::new(temp.path().to_owned()).unwrap(),
            opener: Arc::new(move |path, select| {
                captured.lock().unwrap().push((path.to_owned(), select));
                Ok(())
            }),
        });
        let _ = folder(State(files.clone()), RoutePath("second".into()))
            .await
            .unwrap();
        let _ = reveal(
            State(files.clone()),
            RoutePath("first".into()),
            Json(FileRequest {
                filename: "art, one.png".into(),
            }),
        )
        .await
        .unwrap();
        let _ = image(
            State(files.clone()),
            RoutePath("first".into()),
            Json(FileRequest {
                filename: "art, one.png".into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                (
                    temp.path().join("sessions/second").canonicalize().unwrap(),
                    false
                ),
                (
                    temp.path()
                        .join("sessions/first/exports/art, one.png")
                        .canonicalize()
                        .unwrap(),
                    true
                ),
                (
                    temp.path()
                        .join("sessions")
                        .join("first")
                        .join("exports")
                        .join("art, one.png")
                        .canonicalize()
                        .unwrap(),
                    false
                ),
            ]
        );
        for filename in [
            "../catalog.json",
            "..\\catalog.json",
            "C:\\outside.png",
            ".partial.png",
            "missing.png",
            "art.png:stream",
        ] {
            assert!(
                reveal(
                    State(files.clone()),
                    RoutePath("first".into()),
                    Json(FileRequest {
                        filename: filename.into()
                    })
                )
                .await
                .is_err()
            );
            assert!(
                image(
                    State(files.clone()),
                    RoutePath("first".into()),
                    Json(FileRequest {
                        filename: filename.into()
                    })
                )
                .await
                .is_err()
            );
        }
        assert!(
            folder(State(files.clone()), RoutePath("../first".into()))
                .await
                .is_err()
        );
        for filename in ["unsafe.exe", "shortcut.lnk", "website.url"] {
            fs::write(
                temp.path()
                    .join("sessions")
                    .join("first")
                    .join("exports")
                    .join(filename),
                b"not an image",
            )
            .unwrap();
            assert!(
                image(
                    State(files.clone()),
                    RoutePath("first".into()),
                    Json(FileRequest {
                        filename: filename.into()
                    })
                )
                .await
                .is_err()
            );
        }
        assert_eq!(calls.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn shell_failure_is_not_reported_as_success() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sessions/first");
        fs::create_dir_all(path.join("exports")).unwrap();
        fs::write(path.join("catalog.json"), "{}").unwrap();
        fs::write(path.join("exports").join("image.png"), b"image").unwrap();
        let files = Arc::new(Files {
            projects: Projects::new(temp.path().to_owned()).unwrap(),
            opener: Arc::new(|_, _| anyhow::bail!("Explorer unavailable")),
        });
        let error = folder(State(files.clone()), RoutePath("first".into()))
            .await
            .unwrap_err();
        assert_eq!(error.0, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(error.1.to_string().contains("Explorer unavailable"));
        let error = image(
            State(files),
            RoutePath("first".into()),
            Json(FileRequest {
                filename: "image.png".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(error.1.to_string().contains("Explorer unavailable"));
    }

    #[test]
    fn linked_exports_cannot_reveal_files_outside_the_project() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("art.png"), b"outside").unwrap();
        let link = project.join("exports");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let quote = |path: &Path| path.to_string_lossy().replace('\'', "''");
            let script = format!(
                "New-Item -ItemType Junction -Path '{}' -Target '{}' -ErrorAction Stop",
                quote(&link),
                quote(&outside)
            );
            let output = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                .creation_flags(0x08000000)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(export_file(&project.canonicalize().unwrap(), "art.png").is_err());
        #[cfg(windows)]
        std::fs::remove_dir(&link).unwrap();
        #[cfg(unix)]
        std::fs::remove_file(&link).unwrap();
    }
}
