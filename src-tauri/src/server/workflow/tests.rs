use super::*;
use crate::discovery::{Archive, Endpoints};
use axum::{body::Bytes, extract::State, routing::get};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{
    io::Cursor,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};
use tokio::sync::Notify;

struct Services {
    fits: Vec<u8>,
    snapshot: Mutex<String>,
    mast_calls: AtomicUsize,
    registrations: AtomicUsize,
    invalid_fits: AtomicBool,
    block_search: AtomicBool,
    searching: Notify,
    release_search: Notify,
    block_download: AtomicBool,
    downloading: Notify,
    release: Notify,
}

async fn hub(State(services): State<Arc<Services>>, bytes: Bytes) -> String {
    let xml = std::str::from_utf8(&bytes).unwrap();
    let document = roxmltree::Document::parse(xml).unwrap();
    let method = document
        .descendants()
        .find(|node| node.has_tag_name("methodName"))
        .unwrap()
        .text()
        .unwrap();
    let params: Vec<_> = document
        .descendants()
        .filter(|node| node.has_tag_name("param"))
        .map(|node| {
            crate::samp::decode(
                node.children()
                    .find(|node| node.has_tag_name("value"))
                    .unwrap(),
                0,
            )
            .unwrap()
        })
        .collect();
    let result = match method {
        "samp.hub.register" => {
            services.registrations.fetch_add(1, Ordering::SeqCst);
            assert_eq!(params[0], "synthetic-secret");
            json!({"samp.private-key":"synthetic-key"})
        }
        "samp.hub.declareMetadata" | "samp.hub.unregister" | "samp.hub.notify" => json!(""),
        "samp.hub.getSubscribedClients" => json!({"aladin":{}}),
        "samp.hub.getMetadata" => json!({"samp.name":"Aladin desktop"}),
        "samp.hub.callAndWait" => {
            if params[2]["samp.mtype"] == "snapshot.get.jpg" {
                json!({"samp.status":"samp.ok","samp.result":{"data":services.snapshot.lock().unwrap().clone()}})
            } else {
                assert_eq!(params[2]["samp.mtype"], "samp.app.ping");
                json!({"samp.status":"samp.ok"})
            }
        }
        other => panic!("Unexpected SAMP method: {other}"),
    };
    format!(
        "<methodResponse><params><param>{}</param></params></methodResponse>",
        crate::samp::encode(&result).unwrap()
    )
}

async fn mast(State(services): State<Arc<Services>>) -> Json<Value> {
    services.mast_calls.fetch_add(1, Ordering::SeqCst);
    if services.block_search.load(Ordering::SeqCst) {
        services.searching.notify_one();
        services.release_search.notified().await;
    }
    let rows = ["a", "b"].map(|key| {
        json!({
            "obsid":key,"obs_id":format!("mock-{key}"),"obs_collection":"JWST",
            "instrument_name":"NIRCAM","filters":if key=="a"{"F200W"}else{"F300M"},
            "s_ra":10.,"s_dec":20.,"target_name":"Synthetic field","t_exptime":100.,
            "s_region":"POLYGON ICRS 9.995 19.995 10.005 19.995 10.005 20.005 9.995 20.005",
            "dataURL":format!("mast:JWST/product/mock_{key}_i2d.fits")
        })
    });
    Json(json!({"status":"COMPLETE","data":rows}))
}

async fn science(State(services): State<Arc<Services>>) -> Vec<u8> {
    if services.block_download.load(Ordering::SeqCst) {
        services.downloading.notify_one();
        services.release.notified().await;
    }
    if services.invalid_fits.load(Ordering::SeqCst) {
        b"Not a science FITS file".to_vec()
    } else {
        services.fits.clone()
    }
}

fn header(width: u32, height: u32, scale: f64, projection: &str) -> Value {
    json!({"NAXIS":2,"NAXIS1":width,"NAXIS2":height,
        "CTYPE1":format!("RA---{projection}"),"CTYPE2":format!("DEC--{projection}"),
        "CUNIT1":"deg","CUNIT2":"deg","RADESYS":"ICRS",
        "CRVAL1":10.,"CRVAL2":20.,"CRPIX1":(width as f64+1.)/2.,"CRPIX2":(height as f64+1.)/2.,
        "CDELT1":-scale,"CDELT2":scale})
}

fn snapshot(header: &Value) -> String {
    let cards = header
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| {
            let value = if let Some(value) = value.as_str() {
                format!("'{value}'")
            } else {
                value.to_string()
            };
            format!("{key:<8}= {value}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut cards = format!("SIMPLE  = T\nBITPIX  = 8\n{cards}");
    cards.push_str("\nEND");
    let mut original = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(
        header["NAXIS1"].as_u64().unwrap() as u32,
        header["NAXIS2"].as_u64().unwrap() as u32,
    )
    .write_to(&mut original, image::ImageFormat::Jpeg)
    .unwrap();
    let mut jpeg = vec![255, 216, 255, 254];
    jpeg.extend_from_slice(&((cards.len() + 2) as u16).to_be_bytes());
    jpeg.extend_from_slice(cards.as_bytes());
    jpeg.extend_from_slice(&original.into_inner()[2..]);
    format!("data:image/jpeg;base64,{}", STANDARD.encode(jpeg))
}

async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (base, task)
}

struct Http {
    base: String,
    client: reqwest::Client,
}

impl Http {
    async fn response(
        &self,
        method: Method,
        path: &str,
        session: Option<&str>,
        body: Option<Value>,
    ) -> reqwest::Response {
        let mut request = self.client.request(method, format!("{}{path}", self.base));
        if let Some(session) = session {
            request = request.header("x-astroimp-session", session);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        request.send().await.unwrap()
    }

    async fn json(
        &self,
        method: Method,
        path: &str,
        session: Option<&str>,
        body: Option<Value>,
    ) -> Value {
        let response = self.response(method, path, session, body).await;
        let status = response.status();
        let text = response.text().await.unwrap();
        assert_eq!(status, StatusCode::OK, "{path}: {text}");
        serde_json::from_str(&text).unwrap()
    }

    async fn done(&self, session: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let job = self
                    .json(Method::GET, "/api/job", Some(session), None)
                    .await;
                if matches!(job["state"].as_str(), Some("done" | "error" | "cancelled")) {
                    return job;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("Native workflow job did not finish")
    }

    async fn success(&self, session: &str) -> Value {
        let job = self.done(session).await;
        assert_eq!(job["state"], "done", "{job}");
        job
    }

    async fn select(&self, session: &str, observation: &str) {
        self.json(
            Method::PUT,
            "/api/selection",
            Some(session),
            Some(json!({"obs_ids":[observation]})),
        )
        .await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cached_survey_exports_plan_encode_and_download_real_source_formats() {
    let root = tempfile::tempdir().unwrap();
    let session_id = "survey-export";
    let session = root.path().join("sessions").join(session_id);
    let project = session.join("project");
    fs::create_dir_all(&project).unwrap();
    let source = session.join("survey.fits");
    let wcs = header(48, 32, 2. / 3600., "TAN");
    let values: Vec<_> = (0..48 * 32)
        .map(|index| (index % 48) as f32 * 0.0731 + (index / 48) as f32 * 0.0137)
        .collect();
    native::tests::write_fits(&source, &values, &wcs, false);
    let grid = json!({"width":48,"height":32,"wcs":wcs,"pixel_scale_arcsec":2.});
    native::prepare_build(
        &session,
        &project,
        &grid,
        &[vec![native::BuildSource {
            path: source.clone(),
            observation: json!({"obs_id":"wise","hips_id":"CDS/P/allWISE/W3","filter":"W3"}),
        }]],
        false,
        0.,
        &JobHandle::untracked(),
    )
    .unwrap();
    // Export must read science, not encode or enlarge the cached editor plane.
    fs::write(project.join("wise.f16"), vec![0; 48 * 32 * 2]).unwrap();
    projects::write_json(
        &session.join("session.json"),
        &json!({"name":"Survey export"}),
    )
    .unwrap();
    projects::write_json(&session.join("catalog.json"), &json!({"groups":[]})).unwrap();
    let state = json!({"version":1,"sources":["wise"],"selectedLayer":"art",
        "layers":[{"id":"art","source":"wise","mode":"tint","color":"#b2d8f3",
            "stretch":"asinh","blend":"add"}],"global":{}});
    projects::write_json(&session.join("edits.json"), &state).unwrap();
    let originals: Vec<_> = [
        project.join("meta.json"),
        session.join("edits.json"),
        project.join("wise.f16"),
        source.clone(),
    ]
    .map(|path| {
        let before = fs::read(&path).unwrap();
        (path, before)
    })
    .into();
    let (app, shutdown) =
        router_with_shutdown(root.path().to_owned(), root.path().join("frontend")).unwrap();
    let (base, server) = serve(app).await;
    let http = Http {
        base,
        client: reqwest::Client::builder().no_proxy().build().unwrap(),
    };
    let plan = http
        .json(Method::GET, "/api/export/full/plan", Some(session_id), None)
        .await;
    assert_eq!(plan["width"], 48);
    assert_eq!(plan["max_long_side"], 48);
    for (format, mime, color) in [
        ("png16", "image/png", image::ColorType::Rgb16),
        ("tiff16", "image/tiff", image::ColorType::Rgb16),
        ("jpeg", "image/jpeg", image::ColorType::Rgb8),
    ] {
        http.json(
            Method::POST,
            "/api/export/full",
            Some(session_id),
            Some(json!({"format":format,"long_side":24})),
        )
        .await;
        let job = http.success(session_id).await;
        let response = http
            .response(
                Method::GET,
                job["result"]["download_url"].as_str().unwrap(),
                Some(session_id),
                None,
            )
            .await;
        assert_eq!(response.headers()[header::CONTENT_TYPE], mime);
        let image = image::load_from_memory(&response.bytes().await.unwrap()).unwrap();
        assert_eq!((image.width(), image.height()), (24, 16));
        assert_eq!(image.color(), color);
        assert!(image.to_rgb8().as_raw().iter().any(|value| *value > 30));
        if color == image::ColorType::Rgb16 {
            assert!(
                image
                    .to_rgb16()
                    .as_raw()
                    .iter()
                    .any(|value| value % 257 != 0)
            );
        }
    }
    for (path, before) in originals {
        assert_eq!(fs::read(path).unwrap(), before);
    }
    let export_names = || {
        fs::read_dir(session.join("exports"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let published = export_names();
    fs::remove_file(source).unwrap();
    let missing = http
        .response(
            Method::POST,
            "/api/export/full",
            Some(session_id),
            Some(json!({"format":"png16"})),
        )
        .await;
    assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
    assert_eq!(export_names(), published);
    shutdown.cancel_and_wait();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_new_capture_bookmark_build_edit_export_and_recovery_contract() {
    let root = tempfile::tempdir().unwrap();
    let fits = root.path().join("synthetic_i2d.fits");
    let values: Vec<_> = (0..64 * 48)
        .map(|index| {
            let x = (index % 64) as f32;
            let y = (index / 64) as f32;
            0.1 * x + 0.02 * y + 100. * (-((x - 32.).powi(2) + (y - 24.).powi(2)) / 80.).exp()
        })
        .collect();
    native::tests::write_fits(&fits, &values, &header(64, 48, 0.0005, "TAN"), true);
    let services = Arc::new(Services {
        fits: fs::read(fits).unwrap(),
        snapshot: Mutex::new(snapshot(&header(64, 48, 0.0005, "AIT"))),
        mast_calls: AtomicUsize::new(0),
        registrations: AtomicUsize::new(0),
        invalid_fits: AtomicBool::new(false),
        block_search: AtomicBool::new(false),
        searching: Notify::new(),
        release_search: Notify::new(),
        block_download: AtomicBool::new(false),
        downloading: Notify::new(),
        release: Notify::new(),
    });
    let mock = Router::new()
        .route("/samp", post(hub))
        .route("/mast", post(mast))
        .route("/download", get(science))
        .route("/dss", get(science))
        .fallback(|| async { StatusCode::SERVICE_UNAVAILABLE })
        .with_state(services.clone());
    let (base, mock_server) = serve(mock).await;
    let endpoints = Endpoints {
        mast: format!("{base}/mast"),
        mast_download: format!("{base}/download"),
        harvard: format!("{base}/harvard"),
        sesame: format!("{base}/sesame"),
        moc: format!("{base}/moc"),
        hips2fits: vec![format!("{base}/hips2fits")],
        dss: format!("{base}/dss"),
        ps1: base.clone(),
        irsa: base.clone(),
        mdw: format!("{base}/mdw"),
    };
    let library = root.path().join("library");
    fs::create_dir(&library).unwrap();
    let lock = library.join(".samp");
    fs::write(
        &lock,
        format!("samp.secret=synthetic-secret\nsamp.hub.xmlrpc.url={base}/samp\n"),
    )
    .unwrap();
    let samp = Samp::at(lock);
    assert!(samp.ping().await.unwrap());
    let archive = Archive::with_endpoints(&library, endpoints).unwrap();
    let (router, shutdown) = super::super::router_with_services(
        library.clone(),
        root.path().join("frontend"),
        archive,
        samp,
    )
    .unwrap();
    let (base, server) = serve(router).await;
    let http = Http {
        base,
        client: reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap(),
    };

    let capture = http.json(Method::POST, "/api/capture", None, None).await;
    let session = capture["session"].as_str().unwrap();
    http.success(session).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while http
            .json(Method::GET, "/api/status", Some(session), None)
            .await["aladin"]
            != true
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Status never detected the responding Aladin");
    let first = http
        .json(Method::GET, "/api/session", Some(session), None)
        .await;
    assert_eq!(first["view"]["projection"], json!(["RA---AIT", "DEC--AIT"]));
    let rows: Vec<_> = first["catalog"]["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group["observations"].as_array().unwrap())
        .collect();
    assert!(rows.iter().any(|row| row["obs_id"] == "mock-a"));
    assert!(
        rows.iter().any(|row| row["hips_id"] == "CDS/P/DSS2/red"),
        "CDS outage lost independent DSS: {}",
        first["catalog"]
    );
    assert!(first["catalog"]["survey_warning"].is_string());
    let path = library
        .join("sessions")
        .join(session)
        .canonicalize()
        .unwrap();

    let calls = services.mast_calls.load(Ordering::SeqCst);
    *services.snapshot.lock().unwrap() = snapshot(&header(80, 60, 0.0004, "AIT"));
    let refresh = http
        .json(
            Method::POST,
            "/api/session/snapshot/refresh",
            Some(session),
            None,
        )
        .await;
    assert_eq!(refresh["view"]["pixel_dimensions"], json!([80, 60]));
    assert_eq!(services.mast_calls.load(Ordering::SeqCst), calls);
    let response = http
        .response(
            Method::GET,
            "/api/session/snapshot.jpg",
            Some(session),
            None,
        )
        .await;
    assert_eq!(
        image::load_from_memory(&response.bytes().await.unwrap())
            .unwrap()
            .width(),
        80
    );

    let before = http
        .json(Method::GET, "/api/session", Some(session), None)
        .await;
    let preview = "/api/preview/survey-CDS-P-DSS2-red?size=thumb";
    let old_preview = http
        .response(Method::GET, preview, Some(session), None)
        .await;
    assert_eq!(old_preview.status(), StatusCode::OK);
    assert_eq!(
        old_preview.headers()[header::CACHE_CONTROL],
        "private, max-age=86400"
    );
    let old_preview = old_preview.bytes().await.unwrap();
    http.json(
        Method::PUT,
        "/api/selection",
        Some(session),
        Some(json!({"obs_ids":["mock-a"]})),
    )
    .await;
    let mut moved = header(80, 60, 0.0004, "AIT");
    moved["CRVAL1"] = json!(10. + 80. * 0.0004 / 4.);
    *services.snapshot.lock().unwrap() = snapshot(&moved);
    assert_eq!(
        http.response(
            Method::POST,
            "/api/session/snapshot/refresh",
            Some(session),
            None
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    http.json(Method::POST, "/api/session/recapture", Some(session), None)
        .await;
    http.success(session).await;
    let recaptured = http
        .json(Method::GET, "/api/session", Some(session), None)
        .await;
    assert_ne!(
        recaptured["view"]["center_icrs_deg"],
        before["view"]["center_icrs_deg"]
    );
    assert_ne!(
        recaptured["view"]["captured_at_utc"],
        before["view"]["captured_at_utc"]
    );
    assert_eq!(recaptured["selection"]["obs_ids"], json!([]));
    assert!(services.mast_calls.load(Ordering::SeqCst) > calls);
    let new_preview = http
        .response(Method::GET, preview, Some(session), None)
        .await;
    assert_eq!(new_preview.status(), StatusCode::OK);
    assert_ne!(new_preview.bytes().await.unwrap(), old_preview);

    let bookmark = http
        .json(
            Method::POST,
            "/api/bookmarks",
            None,
            Some(json!({"session":session,"obs_ids":["mock-a"]})),
        )
        .await;
    assert_eq!(bookmark.as_array().unwrap().len(), 1);
    let saved = http.json(Method::GET, "/api/bookmarks", None, None).await;
    assert_eq!(saved, bookmark);
    let frame = http
        .json(
            Method::POST,
            "/api/bookmarks/frame",
            None,
            Some(json!({"ids":[bookmark[0]["id"]],"aspect":"3:2"})),
        )
        .await;
    assert!(frame["ra"].as_f64().unwrap().is_finite());
    http.json(
        Method::PUT,
        "/api/triage",
        Some(session),
        Some(json!({"favorites":["mock-a"],"dismissed":["mock-b"]})),
    )
    .await;
    assert_eq!(
        http.response(
            Method::PUT,
            "/api/selection",
            Some(session),
            Some(json!({"obs_ids":["missing"]}))
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    http.select(session, "mock-a").await;
    let plan = http
        .json(Method::GET, "/api/build/plan", Some(session), None)
        .await;
    assert_eq!(plan["layer_count"], 1);
    assert_eq!(plan["files"][0]["cached"], false);
    http.json(Method::POST, "/api/build", Some(session), None)
        .await;
    let built = http.success(session).await;
    assert_eq!(built["files"][0]["state"], "done");
    assert_eq!(
        built["files"][0]["done_bytes"],
        built["files"][0]["size_bytes"]
    );
    let payload = http
        .json(Method::GET, "/api/session", Some(session), None)
        .await;
    assert_eq!(payload["build_version"], payload["meta"]["build_version"]);
    assert_eq!(payload["rebuild_required"], false);
    let key = payload["meta"]["layers"][0]["key"].as_str().unwrap();
    let plane = http
        .response(
            Method::GET,
            &format!("/api/project/{key}.f16"),
            Some(session),
            None,
        )
        .await;
    assert_eq!(plane.status(), StatusCode::OK);
    assert_eq!(
        plane.bytes().await.unwrap().len() as u64,
        payload["meta"]["width"].as_u64().unwrap()
            * payload["meta"]["height"].as_u64().unwrap()
            * 2
    );
    let state = json!({"version":1,"sources":[key],"selectedLayer":"art",
        "layers":[{"id":"art","source":key,"color":"#9acbff","stretch":"linear","blend":"add","mode":"tint","opacity":1.}],"global":{}});
    http.json(
        Method::PUT,
        "/api/edits",
        Some(session),
        Some(state.clone()),
    )
    .await;
    assert_eq!(
        http.json(Method::GET, "/api/session", Some(session), None)
            .await["edits"],
        state
    );
    let cached = http
        .json(Method::GET, "/api/build/plan", Some(session), None)
        .await;
    assert_eq!(cached["files"][0]["cached"], true);
    assert_eq!(cached["grid_locked"], true);
    http.json(
        Method::POST,
        "/api/export/full",
        Some(session),
        Some(json!({"format":"png8"})),
    )
    .await;
    let export = http.success(session).await;
    let url = export["result"]["download_url"].as_str().unwrap();
    let bytes = http
        .response(Method::GET, url, Some(session), None)
        .await
        .bytes()
        .await
        .unwrap();
    let image = image::load_from_memory(&bytes).unwrap().to_rgb8();
    assert_eq!(
        image.width() as u64,
        export["result"]["width"].as_u64().unwrap()
    );
    assert!(
        image
            .pixels()
            .any(|pixel| pixel.0.iter().any(|value| *value > 0))
    );
    let original_meta = fs::read(path.join("project").join("meta.json")).unwrap();
    let original_edits = fs::read(path.join("edits.json")).unwrap();
    let original_plane = fs::read(path.join("project").join(format!("{key}.f16"))).unwrap();

    http.select(session, "mock-b").await;
    services.invalid_fits.store(true, Ordering::SeqCst);
    http.json(Method::POST, "/api/build", Some(session), None)
        .await;
    assert_eq!(http.done(session).await["state"], "error");
    services.invalid_fits.store(false, Ordering::SeqCst);
    services.block_download.store(true, Ordering::SeqCst);
    http.json(Method::POST, "/api/build", Some(session), None)
        .await;
    tokio::time::timeout(Duration::from_secs(5), services.downloading.notified())
        .await
        .unwrap();
    http.json(Method::POST, "/api/job/cancel", Some(session), None)
        .await;
    services.release.notify_one();
    assert_eq!(http.done(session).await["state"], "cancelled");
    services.block_download.store(false, Ordering::SeqCst);
    assert_eq!(
        fs::read(path.join("project").join("meta.json")).unwrap(),
        original_meta
    );
    assert_eq!(fs::read(path.join("edits.json")).unwrap(), original_edits);
    assert_eq!(
        fs::read(path.join("project").join(format!("{key}.f16"))).unwrap(),
        original_plane
    );

    http.select(session, "mock-a").await;
    let saved_view = fs::read(path.join("view.json")).unwrap();
    let saved_catalog = fs::read(path.join("catalog.json")).unwrap();
    let saved_selection = fs::read(path.join("selection.json")).unwrap();
    services.block_search.store(true, Ordering::SeqCst);
    let rotation = http
        .json(
            Method::PUT,
            "/api/session/rotation",
            Some(session),
            Some(json!({"rotation_deg":90.})),
        )
        .await;
    tokio::time::timeout(Duration::from_secs(5), services.searching.notified())
        .await
        .unwrap();
    assert_eq!(
        http.response(
            Method::PUT,
            "/api/selection",
            Some(session),
            Some(json!({"obs_ids":["mock-b"]}))
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    http.json(
        Method::POST,
        &format!("/api/job/cancel?id={}", rotation["id"].as_str().unwrap()),
        Some(session),
        None,
    )
    .await;
    let cancelled = tokio::time::timeout(Duration::from_secs(2), http.done(session))
        .await
        .expect("Rotation cancellation must not wait for the stalled archive");
    assert_eq!(cancelled["state"], "cancelled");
    assert_eq!(fs::read(path.join("view.json")).unwrap(), saved_view);
    assert_eq!(fs::read(path.join("catalog.json")).unwrap(), saved_catalog);
    assert_eq!(
        fs::read(path.join("selection.json")).unwrap(),
        saved_selection
    );
    services.block_search.store(false, Ordering::SeqCst);
    services.release_search.notify_one();
    http.json(
        Method::PUT,
        "/api/session/rotation",
        Some(session),
        Some(json!({"rotation_deg":30.})),
    )
    .await;
    http.success(session).await;
    let rotated = http
        .json(Method::GET, "/api/session", Some(session), None)
        .await;
    assert_eq!(rotated["rebuild_required"], true);
    let capture = capture_saved(&path).unwrap();
    let expected = capture
        .wcs()
        .unwrap()
        .sky_to_pixel([9.995, 19.995])
        .unwrap();
    let rows: Vec<_> = rotated["catalog"]["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group["observations"].as_array().unwrap())
        .collect();
    let observation = rows.iter().find(|row| row["obs_id"] == "mock-a").unwrap();
    let corner = &observation["pixels"][0][0];
    assert!(
        (corner[0].as_f64().unwrap() - expected[0]).abs() < 1.,
        "Footprint x: {corner}, expected {expected:?}"
    );
    assert!(
        (corner[1].as_f64().unwrap() - (60. - 1. - expected[1])).abs() < 1.,
        "Footprint y: {corner}, expected {expected:?}"
    );
    http.json(Method::POST, "/api/build", Some(session), None)
        .await;
    http.success(session).await;
    let history = http
        .json(Method::GET, "/api/builds", Some(session), None)
        .await;
    assert_eq!(history.as_array().unwrap().len(), 2);
    let archived = history
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["current"] == false)
        .unwrap()["id"]
        .as_str()
        .unwrap();
    http.json(
        Method::POST,
        &format!("/api/builds/{archived}/restore"),
        Some(session),
        None,
    )
    .await;
    assert_eq!(
        fs::read(path.join("project").join("meta.json")).unwrap(),
        original_meta
    );
    assert_eq!(fs::read(path.join("edits.json")).unwrap(), original_edits);
    assert_eq!(
        http.json(Method::GET, "/api/builds", Some(session), None)
            .await
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        http.response(Method::POST, "/api/session/recapture", Some(session), None)
            .await
            .status(),
        StatusCode::CONFLICT
    );

    let resolved = http
        .json(
            Method::GET,
            "/api/coordinates/resolve?target=10%2020",
            None,
            None,
        )
        .await;
    assert_eq!(resolved["ra"], 10.);
    assert_eq!(resolved["dec"], 20.);
    let coverage = http
        .json(
            Method::GET,
            "/api/coordinates/coverage?ra=10&dec=20&aspect=3:2",
            None,
            None,
        )
        .await;
    assert!(coverage["counts"]["JWST"].as_u64().unwrap() > 0);
    let registrations = services.registrations.load(Ordering::SeqCst);
    let coordinate=http.json(Method::POST,"/api/capture/coordinates",None,Some(json!({
        "ra":10.,"dec":20.,"name":"Coordinate field","width_arcmin":1.,"aspect":"3:2","rotation_deg":0.
    }))).await;
    let coordinate = coordinate["session"].as_str().unwrap();
    http.success(coordinate).await;
    let payload = http
        .json(Method::GET, "/api/session", Some(coordinate), None)
        .await;
    assert_eq!(payload["name"], "Coordinate field");
    assert_eq!(payload["view"]["source"], "coordinates");
    assert_eq!(services.registrations.load(Ordering::SeqCst), registrations);
    let rows: Vec<_> = payload["catalog"]["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group["observations"].as_array().unwrap())
        .collect();
    let survey = rows
        .iter()
        .find(|row| row["hips_id"] == "CDS/P/DSS2/red")
        .unwrap()["obs_id"]
        .as_str()
        .unwrap();
    let preview = http
        .response(
            Method::GET,
            &format!("/api/preview/{survey}"),
            Some(coordinate),
            None,
        )
        .await;
    assert_eq!(preview.status(), StatusCode::OK);
    assert!(
        image::load_from_memory(&preview.bytes().await.unwrap())
            .unwrap()
            .width()
            <= 320
    );
    let backdrop = http
        .response(
            Method::GET,
            "/api/session/backdrop.jpg",
            Some(coordinate),
            None,
        )
        .await;
    assert_eq!(backdrop.status(), StatusCode::OK);
    assert!(
        image::load_from_memory(&backdrop.bytes().await.unwrap())
            .unwrap()
            .width()
            <= 1000
    );
    http.select(coordinate, survey).await;
    http.json(Method::POST, "/api/build", Some(coordinate), None)
        .await;
    http.success(coordinate).await;
    assert_eq!(
        http.json(Method::GET, "/api/session", Some(session), None)
            .await["edits"],
        state
    );

    tokio::task::spawn_blocking(move || shutdown.cancel_and_wait())
        .await
        .unwrap();
    server.abort();
    mock_server.abort();
}
