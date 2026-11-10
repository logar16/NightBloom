// Native Windows shell for NightBloom: runs the Rust HTTP service on a loopback port and shows
// the React/WebGL2 frontend in a WebView2 window (same-origin bundles or explicit Vite dev mode).
// Built only with `--features desktop`, so headless API/core builds and tests never need a GUI.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[path = "astroimp-desktop/files.rs"]
mod files;
#[path = "astroimp-desktop/library.rs"]
mod library;
#[path = "astroimp-desktop/storage.rs"]
mod storage;
#[path = "astroimp-desktop/support.rs"]
mod support;

use anyhow::{Context, Result, bail};
use clap::Parser;
use std::{
    io::ErrorKind,
    net::{Ipv4Addr, SocketAddrV4},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicI32, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tauri::{RunEvent, Url, WebviewUrl, WebviewWindowBuilder, webview::DownloadEvent};
use tokio::{io::AsyncReadExt, io::AsyncWriteExt, sync::oneshot};

// Design decision: a fixed default port, not a random one. The frontend keeps its preferences
// (appearance, export choices) in localStorage, which is scoped to the origin, so a new port per
// launch would silently reset them. A busy port is an explicit error, never an attach or a kill.
// 8765 is the owner's Python app and 8876 is used for development/test servers.
const DEFAULT_PORT: u16 = 8766;
const OWNER_PORT: u16 = 8765;

/// NightBloom desktop shell.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Override the saved library folder (holding sessions\ and downloads\).
    #[arg(long)]
    data: Option<PathBuf>,
    /// Loopback port for the built-in server (0 picks a free port, but then preferences do not persist).
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
    /// Built frontend folder; default is the bundled copy next to the executable.
    #[arg(long)]
    frontend: Option<PathBuf>,
    /// Load the configured Vite development server instead of a built frontend (debug builds only).
    #[arg(long, conflicts_with_all = ["frontend", "headless_smoke", "import_only"])]
    dev: bool,
    /// Copy projects, bookmarks and shared raw downloads from an existing data folder.
    /// The source is only read; science references become portable.
    #[arg(long, value_name = "FOLDER")]
    import: Option<PathBuf>,
    /// Complete --import without opening a window. Requires --data and --import.
    #[arg(long, requires_all = ["data", "import"])]
    import_only: bool,
    /// Remember the imported folder as the default library. Requires --import-only.
    #[arg(long, requires = "import_only")]
    set_default: bool,
    /// Allow --data to be this repository's own data\ folder (refused by default).
    #[arg(long)]
    allow_repo_data: bool,
    /// Verification mode, no dialogs and no visible window: open the first built project in a hidden
    /// WebView, check real canvas pixels and an Exposure autosave, write a JSON report to this file
    /// and exit (0 passed, 1 failed, 2 timed out). Requires --data (a disposable copy), not --import.
    #[arg(long, value_name = "REPORT.json")]
    headless_smoke: Option<PathBuf>,
}

/// The running Rust server: stop it with [`Server::shutdown`].
struct Server {
    runtime: tokio::runtime::Runtime,
    jobs: astroimp_desktop::server::Shutdown,
    stop: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    origin: String,
}

impl Server {
    fn start(data: PathBuf, frontend: PathBuf, port: u16) -> Result<Self> {
        Self::start_with_settings(data, frontend, port, None)
    }

    fn start_with_settings(
        data: PathBuf,
        frontend: PathBuf,
        port: u16,
        settings: Option<PathBuf>,
    ) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("astroimp-server")
            .build()
            .context("Starting the async runtime")?;
        let guard = runtime.enter();
        let readiness_path = if frontend.join("index.html").is_file() {
            "/"
        } else {
            "/api/status"
        };
        let (app, jobs) = astroimp_desktop::server::router_with_shutdown(data.clone(), frontend)?;
        let protected = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("data");
        let app = app
            .merge(files::router(data.clone())?)
            .merge(storage::router(data, settings, protected)?);
        // Bind before anything is shown so a busy port fails here, with a clear message.
        let std_listener =
            std::net::TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).map_err(
                |error| match error.kind() {
                    ErrorKind::AddrInUse | ErrorKind::PermissionDenied => anyhow::anyhow!(
                        "Port {port} is already in use (or blocked). Close the program using it, \
                     or start NightBloom with --port <number>."
                    ),
                    _ => anyhow::Error::from(error).context("Binding the loopback server"),
                },
            )?;
        std_listener.set_nonblocking(true)?;
        let listener = tokio::net::TcpListener::from_std(std_listener)?;
        let address = listener.local_addr()?;
        let (stop, stopped) = oneshot::channel::<()>();
        let task = runtime.spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    stopped.await.ok();
                })
                .await
        });
        drop(guard);
        let server = Server {
            runtime,
            jobs,
            stop: Some(stop),
            task: Some(task),
            origin: format!("http://{address}"),
        };
        server.wait_ready(address, readiness_path)?;
        Ok(server)
    }

    /// Checks the built frontend when present, or the API when Vite supplies the frontend.
    fn wait_ready(&self, address: std::net::SocketAddr, path: &str) -> Result<()> {
        self.runtime.block_on(async {
            for _ in 0..150 {
                if self.task.as_ref().is_some_and(|task| task.is_finished()) {
                    bail!("The NightBloom server stopped while starting.");
                }
                if let Ok(mut stream) = tokio::net::TcpStream::connect(address).await {
                    let request = format!(
                        "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
                    );
                    let mut head = [0u8; 12];
                    if stream.write_all(request.as_bytes()).await.is_ok()
                        && stream.read_exact(&mut head).await.is_ok()
                        && head.starts_with(b"HTTP/1.1 200")
                    {
                        return Ok(());
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            bail!("The NightBloom server did not become ready within 15 seconds.")
        })
    }

    /// Cancels running jobs and waits for their workers to finish, then stops accepting requests,
    /// lets in-flight ones finish for a few seconds, and drops the runtime.
    fn shutdown(&mut self) {
        // Jobs first: this also refuses new work while the window is closing.
        self.jobs.cancel_and_wait();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            // The timeout must be created inside the runtime (it needs the timer driver).
            let _ = self
                .runtime
                .block_on(async { tokio::time::timeout(Duration::from_secs(10), task).await });
        }
    }
}

/// Tells the page how a handled download ended so it can release the exported blob. The page
/// listens for this window event (frontend/src/api/desktop.ts); no Tauri IPC is involved.
fn notify_download<R: tauri::Runtime>(webview: &tauri::Webview<R>, url: &Url, state: &str) {
    let detail = serde_json::json!({ "url": url.as_str(), "state": state });
    let _ = webview.eval(format!(
        "window.dispatchEvent(new CustomEvent('astroimp:download', {{ detail: {detail} }}))"
    ));
}

fn fatal(message: &str) -> ! {
    eprintln!("NightBloom: {message}");
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("NightBloom")
        .set_description(message)
        .show();
    std::process::exit(1);
}

fn data_dir(args: &Args) -> Result<Option<PathBuf>> {
    let data = match &args.data {
        Some(path) => path.clone(),
        None => {
            let settings = library::settings_path()?;
            let protected = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("data");
            let previous = library::read_settings(&settings)?;
            let resolved = match library::finish_relocation(&settings, &protected) {
                Ok(root) => root,
                Err(error) => {
                    eprintln!("Library startup: {error:#}");
                    let mut saved = previous.clone().context("Reading the saved library")?;
                    let can_return = saved.pending_root.is_some() && saved.library_root.is_dir();
                    let action = if can_return {
                        "Use the previous library instead?"
                    } else {
                        "Choose its existing folder?"
                    };
                    let wanted = rfd::MessageDialog::new()
                        .set_title("NightBloom library unavailable")
                        .set_level(rfd::MessageLevel::Error)
                        .set_description(format!(
                            "{error:#}\n\nNo original files were deleted.\n\n{action}"
                        ))
                        .set_buttons(rfd::MessageButtons::YesNo)
                        .show();
                    if wanted != rfd::MessageDialogResult::Yes {
                        return Ok(None);
                    }
                    if can_return {
                        saved.pending_root = None;
                        library::save_settings(&settings, &saved)?;
                        Some(saved.library_root)
                    } else {
                        None
                    }
                }
            };
            if let Some(root) = resolved {
                root
            } else {
                let Some(picked) = rfd::FileDialog::new()
                    .set_title("Choose NightBloom library folder")
                    .pick_folder()
                else {
                    return Ok(None);
                };
                let root = library::validate_root(&picked, &protected)?;
                if previous.is_some() {
                    anyhow::ensure!(
                        root.join("sessions").is_dir(),
                        "The selected folder does not contain an existing library."
                    );
                }
                library::save_settings(
                    &settings,
                    &library::Settings {
                        library_root: root.clone(),
                        pending_root: None,
                    },
                )?;
                root
            }
        }
    };
    // Safety net: development and test runs must not write into the owner's repository data.
    let repo_data = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("data");
    let data = if data.is_absolute() {
        data
    } else {
        std::env::current_dir()?.join(data)
    };
    let data = if args.allow_repo_data {
        data
    } else {
        library::validate_root(&data, &repo_data)?
    };
    Ok(Some(data))
}

/// Offer a read-only import into a newly chosen library, never an explicit test --data folder.
fn offer_first_run_import(data: &Path) -> Result<()> {
    let marker = data.join(".import-offered");
    if !support::is_unpopulated(data) || marker.exists() {
        return Ok(());
    }
    std::fs::write(&marker, "")?;
    let wanted = rfd::MessageDialog::new()
        .set_title("NightBloom")
        .set_description(
            "Copy existing NightBloom projects into this library?\n\nChoose a data folder \
             (the one with \"sessions\") to copy projects, bookmarks and shared raw downloads. \
             The original is unchanged. Large libraries can take several minutes.\n\n\
             A single project folder can also be copied, but its external downloads stay in place.",
        )
        .set_buttons(rfd::MessageButtons::YesNo)
        .show();
    if wanted != rfd::MessageDialogResult::Yes {
        return Ok(());
    }
    if let Some(source) = rfd::FileDialog::new()
        .set_title("Choose a NightBloom data or project folder")
        .pick_folder()
    {
        support::import_projects(&source, data)?;
    }
    Ok(())
}

/// Asks where to save a download; the dialog runs on its own thread while the WebView waits.
fn ask_save_location(suggested: &Path) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title("Save image");
    if let Some(name) = suggested.file_name() {
        dialog = dialog.set_file_name(name.to_string_lossy());
    }
    if let Some(folder) = suggested.parent().filter(|folder| folder.is_dir()) {
        dialog = dialog.set_directory(support::display_path(folder));
    }
    if let Some(extension) = suggested.extension() {
        let extension = extension.to_string_lossy().to_lowercase();
        dialog = dialog.add_filter(extension.clone(), &[extension.as_str()]);
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(dialog.save_file());
    });
    rx.recv().ok().flatten()
}

fn resolve_frontend(args: &Args) -> Result<PathBuf> {
    match &args.frontend {
        Some(path) => support::find_frontend(std::slice::from_ref(path)),
        None => {
            let exe = std::env::current_exe()?;
            let exe_dir = exe.parent().context("The executable has no folder")?;
            support::find_frontend(&support::frontend_candidates(
                exe_dir,
                Path::new(env!("CARGO_MANIFEST_DIR")),
            ))
        }
    }
}

fn development_url(enabled: bool, configured: Option<&Url>) -> Result<Option<Url>> {
    if !enabled {
        return Ok(None);
    }
    anyhow::ensure!(cfg!(debug_assertions), "--dev requires a debug build.");
    let url = configured.context("--dev requires build.devUrl in tauri.conf.json.")?;
    anyhow::ensure!(
        url.scheme() == "http"
            && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none(),
        "The development server must be an HTTP loopback origin without credentials or a path."
    );
    Ok(Some(url.clone()))
}

fn run(args: Args) -> Result<()> {
    if args.port == OWNER_PORT {
        bail!("Port {OWNER_PORT} belongs to the owner's Python app; choose another with --port.");
    }
    let context = tauri::generate_context!();
    let dev_url = development_url(args.dev, context.config().build.dev_url.as_ref())?;
    let Some(data) = data_dir(&args)? else {
        return Ok(());
    };
    let _lock = support::lock_data_dir(&data)?;
    if data.join(".astroimp-transfer.json").try_exists()? {
        let recovery = if args.data.is_some() {
            "Explicit --data does not finalize transfers. Launch without --data to attempt recovery of the saved library."
        } else {
            "Saved-library transfer verification did not finish. Reconnect the original library and retry."
        };
        bail!(
            "This folder still has a library transfer record. {recovery} No original files were deleted."
        );
    }
    let import_source = args.import.clone().or(library::pending_import(&data)?);
    if let Some(source) = &import_source {
        let report = support::import_projects(source, &data)?;
        let missing = storage::missing_sources(&data)?;
        eprintln!(
            "Imported {} files; {} already verified; {:.2} GiB verified; {} referenced FITS unavailable.",
            report.files_copied,
            report.files_kept,
            report.bytes_verified as f64 / 1073741824.,
            missing.len()
        );
        astroimp_desktop::projects::write_json(
            &data.join(".import-report.json"),
            &serde_json::json!({
                "copy":report,"missing_sources":missing,
            }),
        )?;
        if args.set_default {
            library::save_settings(
                &library::settings_path()?,
                &library::Settings {
                    library_root: data.canonicalize()?,
                    pending_root: None,
                },
            )?;
        }
        if args.import_only {
            return Ok(());
        }
    } else if args.data.is_none() {
        offer_first_run_import(&data)?;
    }
    let frontend = if dev_url.is_some() {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("frontend")
            .join("dist")
    } else {
        resolve_frontend(&args)?
    };
    let settings = if args.data.is_none() {
        Some(library::settings_path()?)
    } else {
        None
    };
    let mut server = Server::start_with_settings(data.clone(), frontend, args.port, settings)?;
    let origin = dev_url.unwrap_or(Url::parse(&server.origin)?);

    let navigation_origin = origin.origin();
    let download_origin = origin.origin();
    let url = origin.clone();
    let runtime = data.join(".runtime").join("webview");
    std::fs::create_dir_all(&runtime)?;
    let downloads_data = data.clone();
    let app = tauri::Builder::default()
        .setup(move |app| {
            // Design decision: the window is created here, after the server answers, instead of in
            // tauri.conf.json, so it can load the selected loopback URL. No capability files exist,
            // so this remote origin is granted no Tauri IPC at all (no fs/shell/commands).
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .title("NightBloom")
                .inner_size(1440.0, 900.0)
                .min_inner_size(1000.0, 700.0)
                .center()
                // WebView2 otherwise swallows HTML5 drag and drop, which the editor uses.
                .disable_drag_drop_handler()
                .data_directory(runtime)
                .on_navigation(move |target| target.origin() == navigation_origin)
                .on_download(move |webview, event| match event {
                    DownloadEvent::Requested { url, destination } => {
                        // Only this app's own exports (blob: or same-origin URLs) may be saved.
                        if url.origin() != download_origin {
                            return false;
                        }
                        let session = url
                            .query_pairs()
                            .find(|(key, _)| key == "session")
                            .map(|(_, value)| value.into_owned());
                        let preferred = session
                            .filter(|id| {
                                id.chars()
                                    .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
                                    && !id.starts_with('.')
                            })
                            .map(|id| downloads_data.join("sessions").join(id).join("exports"))
                            .unwrap_or_else(|| downloads_data.join("exports"));
                        if let Err(error) = std::fs::create_dir_all(&preferred) {
                            eprintln!("Creating export folder: {error}");
                            notify_download(&webview, &url, "failed");
                            return false;
                        }
                        let suggested = preferred.join(
                            destination
                                .file_name()
                                .unwrap_or(std::ffi::OsStr::new("export.png")),
                        );
                        match ask_save_location(&suggested) {
                            Some(chosen) => {
                                *destination = chosen;
                                true
                            }
                            None => {
                                notify_download(&webview, &url, "canceled");
                                false
                            }
                        }
                    }
                    DownloadEvent::Finished { url, path, success } => {
                        notify_download(
                            &webview,
                            &url,
                            if success { "finished" } else { "failed" },
                        );
                        if !success {
                            let shown = path.map(|p| p.display().to_string()).unwrap_or_default();
                            std::thread::spawn(move || {
                                rfd::MessageDialog::new()
                                    .set_level(rfd::MessageLevel::Error)
                                    .set_title("NightBloom")
                                    .set_description(format!(
                                        "The image could not be saved. {shown}"
                                    ))
                                    .show();
                            });
                        }
                        true
                    }
                    _ => true,
                })
                .build()?;
            Ok(())
        })
        .build(context)
        .context(
            "Starting the desktop window (is the Microsoft Edge WebView2 Runtime installed?)",
        )?;
    app.run(move |_app, event| {
        // Closing the last window ends the app; stop the server before the process exits.
        if matches!(event, RunEvent::Exit) {
            server.shutdown();
        }
    });
    Ok(())
}

/// Outcome book-keeping for `--headless-smoke`. The first caller of [`Smoke::finish`] wins and
/// writes the report, so the page result, the watchdog and a startup failure never race.
struct Smoke {
    report: PathBuf,
    started: Instant,
    context: Mutex<serde_json::Value>,
    done: AtomicBool,
    code: AtomicI32,
    handle: OnceLock<tauri::AppHandle>,
}

const SMOKE_SCHEME: &str = "astroimp-smoke";
const SMOKE_PAYLOAD_LIMIT: usize = 64 * 1024;
const SMOKE_WATCHDOG: Duration = Duration::from_secs(30);

impl Smoke {
    fn new(report: PathBuf) -> Arc<Self> {
        Arc::new(Smoke {
            report,
            started: Instant::now(),
            context: Mutex::new(serde_json::json!({})),
            done: AtomicBool::new(false),
            code: AtomicI32::new(1),
            handle: OnceLock::new(),
        })
    }

    fn describe(&self, key: &str, value: serde_json::Value) {
        if let Ok(mut context) = self.context.lock() {
            context[key] = value;
        }
    }

    /// Records the outcome (first call only), writes the report and returns the exit code.
    /// `failure_code` is used when the run did not pass (2 for the watchdog, otherwise 1).
    fn finish(
        &self,
        source: &str,
        page: Option<serde_json::Value>,
        error: Option<String>,
        failure_code: i32,
    ) -> i32 {
        if self.done.swap(true, Ordering::SeqCst) {
            return self.code.load(Ordering::SeqCst);
        }
        let passed = error.is_none() && page.as_ref().is_some_and(|page| page["ok"] == true);
        let code = if passed { 0 } else { failure_code };
        self.code.store(code, Ordering::SeqCst);
        let mut report = self
            .context
            .lock()
            .map(|context| context.clone())
            .unwrap_or_default();
        report["ok"] = passed.into();
        report["exit_code"] = code.into();
        report["source"] = source.into();
        report["elapsed_ms"] = (self.started.elapsed().as_millis() as u64).into();
        report["error"] = error
            .or_else(|| {
                page.as_ref()
                    .and_then(|page| page["error"].as_str().map(str::to_owned))
            })
            .into();
        report["page"] = page.unwrap_or(serde_json::Value::Null);
        let text = serde_json::to_string_pretty(&report).unwrap_or_default();
        if let Err(problem) = support::write_report(&self.report, &text) {
            eprintln!("NightBloom smoke: could not write the report: {problem:#}");
            self.code.store(1, Ordering::SeqCst);
            return 1;
        }
        eprintln!(
            "NightBloom smoke: {} (exit {code}), report {}",
            if passed { "passed" } else { "failed" },
            self.report.display()
        );
        code
    }
}

/// Headless smoke run: same server, resources and frontend as the real app, but with a hidden,
/// unfocused window, a throwaway WebView profile and no dialog of any kind. Returns the exit code.
// Design decision: verification of the compiled shell needs a real WebView2 (fonts, WebGL2, resource
// lookup), so this drives the unchanged frontend from injected script instead of adding any test UI.
// Rejected: showing a minimised window — a visible/focusable window is not allowed for tests.
fn smoke(args: Args, report: PathBuf) -> i32 {
    let state = Smoke::new(report);
    match smoke_run(&args, &state) {
        Ok(()) => state.code.load(Ordering::SeqCst),
        Err(error) => state.finish("startup", None, Some(format!("{error:#}")), 1),
    }
}

fn smoke_run(args: &Args, state: &Arc<Smoke>) -> Result<()> {
    if args.data.is_none() {
        bail!("--headless-smoke requires --data <a disposable copy of a data folder>.");
    }
    if args.import.is_some() {
        bail!("--headless-smoke cannot be combined with --import.");
    }
    if args.port == OWNER_PORT {
        bail!("Port {OWNER_PORT} belongs to the owner's Python app; choose another with --port.");
    }
    let data = data_dir(args)?.context("Smoke verification requires a library folder")?;
    let _lock = support::lock_data_dir(&data)?;
    let frontend = resolve_frontend(args)?;
    // The WebView2 profile lives under the temporary data folder, never in the user's profile.
    let profile = data.join(".webview-smoke");
    std::fs::create_dir_all(&profile).with_context(|| format!("Creating {}", profile.display()))?;
    state.describe("data", data.display().to_string().into());
    state.describe("frontend", frontend.display().to_string().into());
    state.describe("webview_profile", profile.display().to_string().into());

    let mut server = Server::start(data, frontend, args.port)?;
    state.describe("origin", server.origin.clone().into());
    let origin = Url::parse(&server.origin)?;
    let navigation_origin = origin.origin();

    // Backup for a wedged page or event loop: report, ask for a normal exit, then force it.
    let watchdog = state.clone();
    std::thread::spawn(move || {
        std::thread::sleep(SMOKE_WATCHDOG);
        let code = watchdog.finish(
            "watchdog",
            None,
            Some(format!("No result within {} s.", SMOKE_WATCHDOG.as_secs())),
            2,
        );
        if let Some(handle) = watchdog.handle.get() {
            handle.exit(code);
        }
        std::thread::sleep(Duration::from_secs(5));
        std::process::exit(code);
    });

    let setup_state = state.clone();
    let app = tauri::Builder::default()
        .setup(move |app| {
            let handle = app.handle().clone();
            let _ = setup_state.handle.set(handle.clone());
            let navigation_state = setup_state.clone();
            WebviewWindowBuilder::new(app, "smoke", WebviewUrl::External(origin))
                .title("NightBloom smoke test")
                .inner_size(1440.0, 900.0)
                .visible(false)
                .focused(false)
                .skip_taskbar(true)
                .data_directory(profile)
                .initialization_script(include_str!("astroimp-desktop/smoke.js"))
                .on_navigation(move |target| {
                    if target.scheme() != SMOKE_SCHEME {
                        return target.origin() == navigation_origin;
                    }
                    // The page's result: record it, end the app, and never let the navigation proceed
                    // (so no external-protocol handler can ever be launched).
                    let payload = target
                        .query_pairs()
                        .find(|(key, _)| key == "payload")
                        .map(|(_, value)| value);
                    let (page, error) = match payload {
                        Some(text) if text.len() <= SMOKE_PAYLOAD_LIMIT => {
                            match serde_json::from_str(&text) {
                                Ok(page) => (Some(page), None),
                                Err(problem) => (
                                    None,
                                    Some(format!("The page result was not JSON: {problem}")),
                                ),
                            }
                        }
                        Some(_) => (None, Some("The page result was too large.".to_owned())),
                        None => (None, Some("The page result had no payload.".to_owned())),
                    };
                    let code = navigation_state.finish("page", page, error, 1);
                    handle.exit(code);
                    false
                })
                // No native Save dialog in smoke mode: every download is refused.
                .on_download(|_webview, _event| false)
                .build()?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .context(
            "Starting the hidden WebView (is the Microsoft Edge WebView2 Runtime installed?)",
        )?;
    let exit_state = state.clone();
    app.run(move |_app, event| {
        if matches!(event, RunEvent::Exit) {
            // Cleanup may wait on workers; never let that hold the process (and a test run) open.
            let code = exit_state.code.load(Ordering::SeqCst);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(12));
                std::process::exit(code);
            });
            server.shutdown();
            std::process::exit(code);
        }
    });
    Ok(())
}

fn main() {
    let args = Args::parse();
    let import_only = args.import_only;
    if let Some(report) = args.headless_smoke.clone() {
        std::process::exit(smoke(args, report));
    }
    if let Err(error) = run(args) {
        if import_only {
            eprintln!("NightBloom import: {error:#}");
            std::process::exit(1);
        } else {
            fatal(&format!("{error:#}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vite_requires_explicit_development_mode_and_a_local_origin() {
        let url = Url::parse("http://127.0.0.1:5173").unwrap();
        assert!(development_url(false, Some(&url)).unwrap().is_none());
        assert!(development_url(true, None).is_err());
        for invalid in [
            "https://127.0.0.1:5173",
            "http://example.com:5173",
            "http://127.0.0.1.evil:5173",
            "http://user:password@127.0.0.1:5173",
            "http://127.0.0.1:5173/other",
            "http://127.0.0.1:5173/?token=x",
        ] {
            assert!(development_url(true, Some(&Url::parse(invalid).unwrap())).is_err());
        }
        if cfg!(debug_assertions) {
            assert_eq!(development_url(true, Some(&url)).unwrap(), Some(url));
        } else {
            assert!(development_url(true, Some(&url)).is_err());
        }
    }

    #[test]
    fn explicit_import_only_does_not_finalize_a_retained_transfer() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("original");
        let destination = tmp.path().join("destination");
        std::fs::create_dir_all(source.join("sessions/a")).unwrap();
        std::fs::create_dir_all(destination.join("sessions/a")).unwrap();
        for root in [&source, &destination] {
            std::fs::write(root.join("sessions/a/session.json"), b"{}").unwrap();
        }
        let marker = destination.join(".astroimp-transfer.json");
        astroimp_desktop::projects::write_json(
            &marker,
            &serde_json::json!({"source":source.canonicalize().unwrap()}),
        )
        .unwrap();
        let record = std::fs::read(&marker).unwrap();
        // Design decision: only saved-library bootstrap can finalize a relocation.
        // Explicit --data/import-only must refuse before importing or consulting owner settings.
        let args = Args::try_parse_from([
            std::ffi::OsStr::new("astroimp-desktop"),
            std::ffi::OsStr::new("--data"),
            destination.as_os_str(),
            std::ffi::OsStr::new("--import"),
            source.as_os_str(),
            std::ffi::OsStr::new("--import-only"),
            std::ffi::OsStr::new("--port"),
            std::ffi::OsStr::new("0"),
        ])
        .unwrap();
        let error = run(args).unwrap_err().to_string();
        assert!(
            error.contains("Explicit --data does not finalize"),
            "{error}"
        );
        assert_eq!(std::fs::read(&marker).unwrap(), record);
        assert!(!destination.join(".astroimp-import.json").exists());
        assert_eq!(
            std::fs::read(source.join("sessions/a/session.json")).unwrap(),
            b"{}"
        );
    }
}
