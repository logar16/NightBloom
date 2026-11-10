use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

struct TestJob {
    jobs: crate::jobs::Jobs,
    handle: JobHandle,
    release: Option<mpsc::Sender<()>>,
}

impl TestJob {
    fn new() -> Self {
        let jobs = crate::jobs::Jobs::default();
        let (send, receive) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        jobs.start("archive-test", "Archive test", move |handle| {
            send.send(handle).unwrap();
            let _ = wait.recv();
            Ok(())
        })
        .unwrap();
        Self {
            jobs,
            handle: receive.recv().unwrap(),
            release: Some(release),
        }
    }
}

impl Drop for TestJob {
    fn drop(&mut self) {
        self.release.take().unwrap().send(()).ok();
        self.jobs.cancel_and_wait();
    }
}

/// A bounded local HTTP fixture. Unmatched routes fail rather than contacting a service.
fn mock(routes: Vec<(&'static str, u16, Vec<u8>)>) -> (Endpoints, std::thread::JoinHandle<()>) {
    mock_response(move |path| {
        routes
            .iter()
            .find(|(prefix, _, _)| path.starts_with(prefix))
            .map(|(_, code, body)| (*code, body.clone()))
            .unwrap_or((503, b"service unavailable".to_vec()))
    })
}

fn mock_response(
    respond: impl Fn(&str) -> (u16, Vec<u8>) + Send + 'static,
) -> (Endpoints, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let endpoints = Endpoints::at(&url);
    let thread = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut last_request = std::time::Instant::now();
        while std::time::Instant::now() < deadline {
            let (mut socket, _) = match listener.accept() {
                Ok(pair) => pair,
                Err(_) => {
                    if last_request.elapsed() > Duration::from_secs(4) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
            };
            last_request = std::time::Instant::now();
            // Windows accepted sockets inherit the listener's nonblocking flag.
            // A premature response before reading a request is invalid HTTP.
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0; 4096];
                let length = socket.read(&mut chunk).unwrap_or(0);
                if length == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..length]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let body = headers
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + body {
                        break;
                    }
                }
                if request.len() > 65536 {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&request);
            let path = request.split_whitespace().nth(1).unwrap_or("");
            let (code, body) = respond(path);
            let _ = write!(
                socket,
                "HTTP/1.1 {code} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(&body);
        }
    });
    (endpoints, thread)
}

fn view() -> (Value, Value) {
    let capture = crate::sky::coordinate_capture(&json!({
        "ra": 10.0, "dec": 20.0, "width_arcmin": 10.0,
        "aspect": "1:1", "rotation_deg": 37.0
    }))
    .unwrap();
    let header = capture.wcs().unwrap().header;
    (capture.view, header)
}

#[tokio::test]
async fn panstarrs_preview_uses_only_display_requests_and_reuses_its_separate_cache() {
    let mut metadata = json!({
        "imsize":[64,256],"crpix":[0.5,128.5],"crval":[10.,20.],
        "cdmatrix":[-1./3600.,0.,0.,1./3600.],"proj":"TAN","refframe":"ICRS"
    });
    let mut png = std::io::Cursor::new(Vec::new());
    image::GrayImage::from_pixel(8, 8, image::Luma([180]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let mut routes = vec![
        (
            "/ps1/ps1filenames.py?",
            200,
            b"filename filter\n/stack/inner2.fits r\n".to_vec(),
        ),
        (
            "/ps1/ps1filenames.py",
            200,
            b"filename filter\n/stack/left.fits r\n/stack/inner1.fits r\n/stack/inner2.fits r\n/stack/right.fits r\n".to_vec(),
        ),
    ];
    for (prefix, crpix) in [
        (
            "/ps1/fitscut.cgi?red=%2Fstack%2Fleft.fits&getWCS=yes",
            128.5,
        ),
        (
            "/ps1/fitscut.cgi?red=%2Fstack%2Finner1.fits&getWCS=yes",
            64.5,
        ),
        (
            "/ps1/fitscut.cgi?red=%2Fstack%2Finner2.fits&getWCS=yes",
            0.5,
        ),
        (
            "/ps1/fitscut.cgi?red=%2Fstack%2Fright.fits&getWCS=yes",
            -63.5,
        ),
    ] {
        metadata["crpix"][0] = json!(crpix);
        routes.push((prefix, 200, serde_json::to_vec(&metadata).unwrap()));
    }
    routes.push(("/ps1/fitscut.cgi?red=", 200, png.into_inner()));
    let (endpoints, server) = mock(routes);
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    let capture = crate::sky::coordinate_capture(&json!({
        "ra":10.,"dec":20.,"width_arcmin":4.,"aspect":"16:9"
    }))
    .unwrap();
    let grid = crate::grid::view_grid(&capture.view, 0.25, 8).unwrap();
    let observation = json!({"hips_id":"CDS/P/PanSTARRS/DR1/r","instrument":"Pan-STARRS"});
    let bytes = archive
        .panstarrs_preview(&observation, &grid)
        .await
        .unwrap();
    let image = image::load_from_memory(&bytes).unwrap().to_luma8();
    assert_eq!(image.dimensions(), (8, 5));
    assert!(image.as_raw().iter().all(|value| *value >= 175));
    assert!(
        archive
            .cached_source(&observation, &grid)
            .unwrap()
            .is_none()
    );
    server.join().unwrap();
    assert_eq!(
        archive
            .panstarrs_preview(&observation, &grid)
            .await
            .unwrap(),
        bytes
    );
}

#[test]
fn filters_and_field_families_follow_science_catalog() {
    assert_eq!(
        normalize_filter("CLEAR1L; F658N + F658N"),
        Some("F658N".into())
    );
    assert_eq!(normalize_filter("CLEAR;OPEN"), None);
    assert_eq!(normalize_filter("F200W;DETECTION"), None);
    assert_eq!(describe("F658N", Some(658.0))["line"], "[N II]");
    assert_eq!(
        describe("F1130W", Some(11300.0))["shows"],
        "Hydrocarbon dust (PAHs)"
    );
    assert_eq!(
        field_family("jw02731-o001_t001_nircam_clear-f200w_i2d.fits", "JWST", "x"),
        ("o001_t001".into(), "JWST mosaic".into())
    );
    assert_eq!(
        field_family("hst_skycell-p1234x01y02_wfc3_ir_f160w_drz.fits", "HST", "x"),
        ("skycell-p1234x01y02".into(), "HAP skycell".into())
    );
    assert_ne!(
        field_family("unknown-a.fits", "HST", "a").0,
        field_family("unknown-b.fits", "HST", "b").0
    );
}

#[test]
fn position_parser_is_offline_and_handles_signed_sexagesimal() {
    assert_eq!(
        parse_position("359.9, -89.9").unwrap(),
        Some([359.9, -89.9])
    );
    assert_eq!(
        parse_position("12:30:00 -00:30:00").unwrap(),
        Some([187.5, -0.5])
    );
    assert_eq!(
        parse_position("12h30m00s +20d15m00s").unwrap(),
        Some([187.5, 20.25])
    );
    assert_eq!(parse_position("M 31").unwrap(), None);
    assert!(parse_position("360 0").is_err());
    assert!(parse_position("12:99:00 +20:00:00").is_err());
    assert_eq!(
        parse_position("24:00:00 +00:00:00").unwrap(),
        Some([0., 0.])
    );
    assert_eq!(
        parse_position("12d00m00s -00d30m00s").unwrap(),
        Some([12., -0.5])
    );
}

#[tokio::test]
async fn mast_outage_preserves_real_survey_coverage_and_warnings() {
    let (endpoints, server) = mock(vec![
        ("/moc", 200, br#"[{"ID":"CDS/P/DSS2/red"}]"#.to_vec()),
        ("/mast", 503, vec![]),
    ]);
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    let job = TestJob::new();
    let (view, header) = view();
    let result = archive.discover(&view, &header, &job.handle).await.unwrap();
    assert_eq!(result["mast_searched"], true);
    assert!(result["mast_warning"].as_str().unwrap().contains("MAST"));
    assert_eq!(
        result["groups"][0]["observations"][0]["hips_id"],
        "CDS/P/DSS2/red"
    );
    server.join().unwrap();
}

#[tokio::test]
async fn wide_view_does_not_query_mast_but_explicit_retry_does() {
    let (endpoints, server) = mock(vec![("/moc", 200, b"[]".to_vec())]);
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    let job = TestJob::new();
    let (mut view, header) = view();
    view["field_arcmin"] = json!([31.0, 10.0]);
    let result = archive.discover(&view, &header, &job.handle).await.unwrap();
    assert_eq!(result["mast_searched"], false);
    assert!(result.get("mast_warning").is_none());
    let retry = archive
        .mast_groups(&view, &header, &job.handle)
        .await
        .unwrap();
    assert_eq!(retry["mast_searched"], true);
    assert!(retry.get("mast_warning").is_some());
    server.join().unwrap();
}

#[tokio::test]
async fn decimal_resolution_never_uses_network_and_harvard_survives_mast_outage() {
    let (endpoints, server) = mock(vec![(
        "/harvard",
        200,
        br#"<VOTABLE><RESOURCE><TABLE>
            <FIELD name="RA_d" ucd="pos.eq.ra;meta.main" datatype="double"/>
            <FIELD name="DEC_d" ucd="pos.eq.dec;meta.main" datatype="double"/>
            <DATA><TABLEDATA><TR><TD>10.25</TD><TD>-20.5</TD></TR></TABLEDATA></DATA>
            </TABLE></RESOURCE></VOTABLE>"#
            .to_vec(),
    )]);
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    assert_eq!(archive.resolve("10.2,-20.4").await.unwrap()["ra"], 10.2);
    let result = archive.resolve("Example Nebula").await.unwrap();
    assert_eq!(result["ra"], 10.25);
    assert_eq!(result["dec"], -20.5);
    server.join().unwrap();
}

#[tokio::test]
async fn science_download_is_atomic_cached_and_cancellation_does_not_publish() {
    let image = fits::Image::new(
        2,
        2,
        vec![1., 2., 3., 4.],
        json!({
            "CTYPE1":"RA---TAN","CTYPE2":"DEC--TAN","CRVAL1":10.,"CRVAL2":20.,
            "CRPIX1":1.5,"CRPIX2":1.5,"CDELT1":-0.01,"CDELT2":0.01
        }),
    )
    .unwrap();
    let bytes = image.encode("mast").unwrap();
    let (endpoints, server) = mock(vec![("/download", 200, bytes.clone())]);
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    let job = TestJob::new();
    let obs =
        json!({"data_uri":"mast:JWST/product/test_i2d.fits", "obs_id":"test", "collection":"JWST"});
    let path = archive
        .download(&obs, &json!({}), &job.handle)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    // Windows temp roots can use a short path while the archive returns its
    // canonical configured-library path; compare the actual file identity.
    assert_eq!(
        path,
        root.path()
            .join("downloads/test_i2d.fits")
            .canonicalize()
            .unwrap()
    );
    assert_eq!(
        archive.cached_source(&obs, &json!({})).unwrap(),
        Some(path.clone())
    );
    assert_eq!(
        archive
            .download(&obs, &json!({}), &job.handle)
            .await
            .unwrap(),
        path
    );
    job.jobs.cancel(None, Some("archive-test"));
    let other = json!({"data_uri":"mast:JWST/product/cancelled_i2d.fits"});
    assert!(
        archive
            .download(&other, &json!({}), &job.handle)
            .await
            .is_err()
    );
    assert!(!root.path().join("downloads/cancelled_i2d.fits").exists());
    assert!(
        std::fs::read_dir(root.path().join("downloads"))
            .unwrap()
            .all(|p| !p.unwrap().file_name().to_string_lossy().ends_with(".part"))
    );
    server.join().unwrap();
}

#[tokio::test]
async fn invalid_remote_fits_is_not_published_or_exposed_in_error() {
    let (endpoints, server) = mock(vec![(
        "/download",
        200,
        b"secret=do-not-expose <html>not science</html>".to_vec(),
    )]);
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    let job = TestJob::new();
    let err = archive
        .download(
            &json!({"data_uri":"mast:HST/product/bad_drz.fits"}),
            &json!({}),
            &job.handle,
        )
        .await
        .unwrap_err();
    assert!(!err.to_string().contains("secret"));
    assert!(!root.path().join("downloads/bad_drz.fits").exists());
    server.join().unwrap();
}

fn tiny_grid() -> Value {
    let theta = 37f64.to_radians();
    let scale = 1.0 / 3600.0;
    json!({"width":8,"height":6,"pixel_scale_arcsec":1.0,"wcs":{
        "CTYPE1":"RA---TAN","CTYPE2":"DEC--TAN","CRVAL1":10.0,"CRVAL2":20.0,
        "CRPIX1":4.5,"CRPIX2":3.5,"CD1_1":-scale*theta.cos(),"CD1_2":scale*theta.sin(),
        "CD2_1":scale*theta.sin(),"CD2_2":scale*theta.cos(),"RADESYS":"ICRS"
    }})
}

fn gradient_fits() -> Vec<u8> {
    fits::Image::new(
        8,
        6,
        (0..48).map(|p| p as f32 + 1.).collect(),
        tiny_grid()["wcs"].clone(),
    )
    .unwrap()
    .encode("fixture")
    .unwrap()
}

#[test]
fn observation_normalization_group_ids_and_top_down_footprints_are_stable() {
    let row = json!({
        "obsid":123,"obs_id":"jw-a","obs_collection":"JWST","instrument_name":"NIRCAM",
        "filters":"CLEAR;F200W","proposal_id":2731,"target_name":"Target","obs_title":"Title",
        "s_ra":10.,"s_dec":20.,"em_min":1700.,"em_max":2300.,"t_exptime":120.,
        "t_min":60000.,"dataURL":"mast:JWST/product/jw02731-o001_t001_nircam_clear-f200w_i2d.fits"
    });
    let first = normalize_observation(&row).unwrap();
    assert_eq!(first["obsid"], "123");
    assert_eq!(first["proposal"], "2731");
    assert_eq!(first["date"], "2023-02-25");
    assert_eq!(first["filter"], "F200W");
    assert_eq!(first["wavelength_nm"], 2000.);
    let mut second_row = row.clone();
    second_row["obs_id"] = json!("jw-b");
    second_row["filters"] = json!("F444W");
    second_row["em_min"] = json!(4000.);
    second_row["em_max"] = json!(4880.);
    second_row["dataURL"] =
        json!("mast:JWST/product/jw02731-o001_t001_nircam_clear-f444w_i2d.fits");
    let second = normalize_observation(&second_row).unwrap();
    let a = group_observations(&[first.clone(), second.clone()]);
    let b = group_observations(&[second, first]);
    assert_eq!(a, b);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0]["filters"], json!(["F200W", "F444W"]));
    second_row["filters"] = json!("DETECTION");
    assert!(normalize_observation(&second_row).is_none());
    let grid = tiny_grid();
    let wcs = SkyWcs::new(&grid["wcs"], 8, 6).unwrap();
    let coordinates =
        [[0., 0.], [7., 0.], [7., 5.], [0., 5.]].map(|p| wcs.pixel_to_sky(p).unwrap());
    let region = format!(
        "POLYGON ICRS {}",
        coordinates
            .iter()
            .map(|p| format!("{} {}", p[0], p[1]))
            .collect::<Vec<_>>()
            .join(" ")
    );
    assert_eq!(
        project(&region, &wcs, 6),
        vec![vec![[0., 5.], [7., 5.], [7., 0.], [0., 0.]]]
    );
    assert_eq!(
        projected_pixels(&region, &grid["wcs"], 8, 6).unwrap(),
        json!([[[0., 5.], [7., 5.], [7., 0.], [0., 0.]]])
    );
    assert_eq!(projected_pixels("", &grid["wcs"], 8, 6).unwrap(), json!([]));
    assert!(projected_pixels(&region, &Value::Null, 8, 6).is_err());
    assert!(intersects_rect(&project(&region, &wcs, 6)[0], 8, 6));
}

#[tokio::test]
async fn cds_outage_keeps_real_irsa_dss_coverage_without_offering_retired_surveys() {
    let moc = fits::moc_fixture(&(4..16).collect::<Vec<_>>());
    let (endpoints, server) = mock(vec![
        ("/irsa", 200, moc),
        ("/dss", 200, gradient_fits()),
        (
            "/ps1/ps1filenames.py",
            200,
            b"filename filter\n/stack/science.fits g\n".to_vec(),
        ),
    ]);
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    let job = TestJob::new();
    let (view, header) = view();
    let result = archive
        .survey_groups(&view, &header, &job.handle)
        .await
        .unwrap();
    let ids: BTreeSet<_> = result["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["observations"].as_array().unwrap())
        .map(|r| r["hips_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 7, "{ids:?}: {}", result["survey_warning"]);
    assert!(ids.contains("CDS/P/allWISE/W4"));
    assert!(ids.contains("CDS/P/DSS2/red"));
    assert!(ids.contains("CDS/P/DSS2/blue"));
    assert!(!ids.iter().any(|id| id.contains("PanSTARRS")));
    assert!(!ids.contains("CDS/P/IPHAS/DR2/halpha"));
    assert!(
        result["survey_warning"]
            .as_str()
            .unwrap()
            .contains("CDS is unavailable")
    );
    server.join().unwrap();
}

#[tokio::test]
async fn independent_dss_and_pan_starrs_science_are_reprojected_to_exact_rotated_grid() {
    let (endpoints, server) = mock(vec![
        ("/dss", 200, gradient_fits()),
        ("/ps1/fitscut.cgi", 200, gradient_fits()),
        (
            "/ps1/ps1filenames.py",
            200,
            b"filename filter\n/stack/science.fits g\n".to_vec(),
        ),
    ]);
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    let job = TestJob::new();
    let grid = tiny_grid();
    for (id, source) in [
        ("CDS/P/DSS2/red", "stsci-dss-v1"),
        ("CDS/P/PanSTARRS/DR1/g", "stsci-ps1-v1"),
    ] {
        let observation = json!({"hips_id":id});
        assert_eq!(archive.cached_source(&observation, &grid).unwrap(), None);
        let path = archive
            .download(&observation, &grid, &job.handle)
            .await
            .unwrap();
        assert_eq!(
            archive.cached_source(&observation, &grid).unwrap(),
            Some(path.clone())
        );
        let image = fits::Image::decode(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!((image.width, image.height), (8, 6));
        assert_eq!(image.header["AIPROV"], source);
        for (index, pixel) in image.pixels.iter().enumerate() {
            assert!(
                (*pixel - index as f32 - 1.).abs() < 1e-3,
                "{index}: {pixel}"
            );
        }

        let actual = SkyWcs::new(&image.header, 8, 6).unwrap();
        let target = SkyWcs::new(&grid["wcs"], 8, 6).unwrap();
        for p in [[0., 0.], [7., 5.], [4., 3.]] {
            assert!(
                separation(
                    actual.pixel_to_sky(p).unwrap(),
                    target.pixel_to_sky(p).unwrap()
                ) < 1e-10
            );
        }
        let mut other = grid.clone();
        other["wcs"]["CRVAL1"] = json!(10.1);
        assert_eq!(archive.cached_source(&observation, &other).unwrap(), None);
    }
    server.join().unwrap();
}

#[tokio::test]
async fn dss_overlap_mosaic_preserves_rotated_geometry_and_rejects_legacy_join_cache() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let capture = crate::sky::coordinate_capture(&json!({
        "ra":10.,"dec":20.,"width_arcmin":80.,"aspect":"3:2","rotation_deg":37.
    }))
    .unwrap();
    let grid = crate::grid::view_grid(&capture.view, 12., 400).unwrap();
    let (width, height) = (
        grid["width"].as_u64().unwrap() as u32,
        grid["height"].as_u64().unwrap() as u32,
    );
    let header = grid["wcs"].clone();
    let wcs = SkyWcs::new(&header, width, height).unwrap();
    let signal: Vec<_> = (0..height)
        .flat_map(|y| (0..width).map(move |x| 10000. + (x * 13 + y * 7 + x * y % 17) as f32))
        .collect();
    let original = fits::Image::new(width, height, signal.clone(), header.clone())
        .unwrap()
        .encode("fixture")
        .unwrap();
    let shifted = fits::Image::new(
        width,
        height,
        signal.iter().map(|value| (value - 700.) / 1.8).collect(),
        header.clone(),
    )
    .unwrap()
    .encode("fixture")
    .unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    let first = original.clone();
    let (endpoints, server) = mock_response(move |path| {
        let url = reqwest::Url::parse(&format!("http://fixture{path}")).unwrap();
        assert_eq!(url.path(), "/dss");
        seen.fetch_add(1, Ordering::SeqCst);
        let params: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert!(params["h"].parse::<f64>().unwrap() <= 60.);
        assert!(params["w"].parse::<f64>().unwrap() <= 60.);
        let center = [params["r"].parse().unwrap(), params["d"].parse().unwrap()];
        let x = wcs.sky_to_pixel(center).unwrap()[0];
        (
            200,
            if x < width as f64 / 2. {
                first.clone()
            } else {
                shifted.clone()
            },
        )
    });
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    let observation = json!({"hips_id":"CDS/P/DSS2/red","survey_source":"stsci-dss-v1"});
    let grid_key = hash(serde_json::to_vec(&json!([width, height, header])).unwrap());
    let legacy = root
        .path()
        .join("downloads/surveys/CDS-P-DSS2-red/stsci-dss-v1")
        .join(format!("{}.fits", &grid_key[..32]));
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, &original).unwrap();
    assert_eq!(archive.cached_source(&observation, &grid).unwrap(), None);
    let job = TestJob::new();
    let path = archive
        .download(&observation, &grid, &job.handle)
        .await
        .unwrap();
    assert_ne!(path, legacy);
    assert_eq!(std::fs::read(&legacy).unwrap(), original);
    let image = fits::Image::decode(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!((image.width, image.height), (width, height));
    assert_eq!(image.header["AICALBAD"], 0);
    for (actual, expected) in image.pixels.iter().zip(signal) {
        assert!((*actual - expected).abs() < 0.03, "{actual} != {expected}");
    }
    let count = requests.load(Ordering::SeqCst);
    assert!(count > 1);
    assert_eq!(
        archive
            .download(&observation, &grid, &job.handle)
            .await
            .unwrap(),
        path
    );
    assert_eq!(requests.load(Ordering::SeqCst), count);
    server.join().unwrap();
}

#[tokio::test]
async fn mdw_discovers_real_fields_blends_calibrated_science_and_reuses_raw_sources() {
    let index = b"Image preview,Field,RA,DEC,Instrumental,Calibrated,Star-Removed\n\
        preview.png,2141,0.6666667,20,wrong.fits,data_release/Fields/2141/calibrated.fits,wrong.fits\n\
        preview.png,2142,0.66667,20,wrong.fits,data_release/Fields/2142/calibrated.fits,wrong.fits\n".to_vec();
    let mut header = tiny_grid()["wcs"].clone();
    header["CTYPE1"] = json!("RA---TAN-SIP");
    header["CTYPE2"] = json!("DEC--TAN-SIP");
    header["A_ORDER"] = json!(2);
    header["B_ORDER"] = json!(2);
    header["A_2_0"] = json!(0.001);
    header["B_0_2"] = json!(-0.002);
    let image = |value| {
        fits::Image::new(8, 6, vec![value; 48], header.clone())
            .unwrap()
            .encode("MDW fixture")
            .unwrap()
    };
    let (endpoints, server) = mock(vec![
        ("/mdw/js/", 200, index),
        (
            "/mdw/data_release/Fields/2141/calibrated.fits",
            200,
            image(10.),
        ),
        (
            "/mdw/data_release/Fields/2142/calibrated.fits",
            200,
            image(30.),
        ),
    ]);
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    let job = TestJob::new();
    let (view, header) = view();
    let result = archive
        .survey_groups(&view, &header, &job.handle)
        .await
        .unwrap();
    let mdw = result["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["instrument"] == "MDW")
        .unwrap();
    let observation = &mdw["observations"][0];
    assert_eq!(observation["survey_id"], "MDW/Halpha");
    assert!(observation["hips_id"].is_null());
    let grid = tiny_grid();
    let transfer = archive.survey_transfer(observation, &grid).await.unwrap();
    assert_eq!(transfer["transfer_bytes"], (image(10.).len() * 2) as u64);
    let fields = transfer["source_fields"].as_array().unwrap();
    assert_eq!(fields.len(), 2);
    assert!(fields.iter().all(|field| field["size_bytes"] == image(10.).len() as u64 && field["cached"] == false));
    let path = archive
        .download(observation, &grid, &job.handle)
        .await
        .unwrap();
    let science = fits::Image::decode(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(science.header["AIPROV"], "mdw-v1");
    assert_eq!(science.header["CTYPE1"], "RA---TAN");
    assert!(science.pixels.iter().all(|p| (*p - 20.).abs() < 1e-4));
    assert_eq!(
        archive.cached_source(observation, &grid).unwrap(),
        Some(path.clone())
    );
    server.join().unwrap();
    let mut shifted = grid.clone();
    shifted["wcs"]["CRVAL1"] = json!(10.00001);
    let cached = archive
        .survey_transfer(observation, &shifted)
        .await
        .unwrap();
    assert_eq!(cached["transfer_bytes"], 0);
    assert!(
        cached["source_fields"]
            .as_array()
            .unwrap()
            .iter()
            .all(|field| field["cached"] == true)
    );
    let previous_log = job.jobs.latest(Some("archive-test")).unwrap().log.len();
    let other = archive
        .download(observation, &shifted, &job.handle)
        .await
        .unwrap();
    assert_ne!(other, path);
    let current = job.jobs.latest(Some("archive-test")).unwrap();
    assert!(
        current.log[previous_log..]
            .iter()
            .any(|message| message.contains("cached MDW"))
    );
    assert!(
        !current.log[previous_log..]
            .iter()
            .any(|message| message.starts_with("Downloading MDW"))
    );
    let mut outside = grid.clone();
    outside["wcs"]["CRVAL1"] = json!(100.);
    assert!(
        archive
            .download(observation, &outside, &job.handle)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn spitzer_galactic_coverage_and_gzip_science_match_independent_astropy_oracles() {
    let pixels = (0..512 * 512)
        .map(|p| (p % 512 + 2 * (p / 512)) as f32)
        .collect();
    let tile = fits::Image::new(512, 512, pixels, json!({}))
        .unwrap()
        .encode("IRAC fixture")
        .unwrap();
    // IRAC1-3 tiles are gzip streams even without HTTP Content-Encoding.
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(&tile).unwrap();
    let tile = gzip.finish().unwrap();
    // Astropy ICRS->Galactic and nested HEALPix at order 5 for M16.
    let (endpoints, server) = mock(vec![
        (
            "/irsa/SPITZER/IRAC1/Moc.fits",
            200,
            fits::moc_fixture_frame(&[8604], true),
        ),
        ("/irsa/SPITZER/IRAC1/Norder", 200, tile),
    ]);
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    assert!(
        archive
            .irsa_coverage("SPITZER/IRAC1", [274.7, -13.8], 0.001, None)
            .await
            .unwrap()
    );
    assert!(
        !archive
            .irsa_coverage("SPITZER/IRAC1", [10., 70.], 0.001, None)
            .await
            .unwrap()
    );
    let scale = (std::f64::consts::PI / 3.).sqrt().to_degrees() * 3600. / (512. * 512.);
    let grid = json!({"width":1,"height":1,"pixel_scale_arcsec":scale,"wcs":{
        "CTYPE1":"RA---TAN","CTYPE2":"DEC--TAN","CRVAL1":274.7,"CRVAL2":-13.8,
        "CRPIX1":1.,"CRPIX2":1.,"CDELT1":-scale/3600.,"CDELT2":scale/3600.
    }});
    let job = TestJob::new();
    let path = archive
        .download(
            &json!({"hips_id":"CDS/P/SPITZER/IRAC1"}),
            &grid,
            &job.handle,
        )
        .await
        .unwrap();
    let image = fits::Image::decode(&std::fs::read(path).unwrap()).unwrap();
    assert!((f64::from(image.pixels[0]) - 619.3401779692791).abs() < 0.001);
    server.join().unwrap();
}

#[tokio::test]
async fn spitzer_is_not_offered_when_coverage_claims_have_only_blank_science() {
    let tile = fits::Image::new(512, 512, vec![f32::NAN; 512 * 512], json!({}))
        .unwrap()
        .encode("blank")
        .unwrap();
    let (endpoints, server) = mock(vec![
        (
            "/irsa/SPITZER/IRAC4/Moc.fits",
            200,
            fits::moc_fixture_frame(&(4..16).collect::<Vec<_>>(), true),
        ),
        ("/irsa/SPITZER/IRAC4/Norder", 200, tile),
    ]);
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    let job = TestJob::new();
    let (view, header) = view();
    let result = archive
        .survey_groups(&view, &header, &job.handle)
        .await
        .unwrap();
    assert!(
        result["groups"]
            .as_array()
            .unwrap()
            .iter()
            .all(|group| group["instrument"] != "Spitzer")
    );
    assert!(result["survey_warning"].as_str().unwrap().contains("IRAC4"));
    server.join().unwrap();
}

#[tokio::test]
async fn wise_uses_irsa_science_tiles_without_cds_or_preview_pixels() {
    let tile = fits::Image::new(512, 512, vec![42.; 512 * 512], json!({}))
        .unwrap()
        .encode("fixture")
        .unwrap();
    let tile_size = tile.len() as u64;
    let (endpoints, server) = mock(vec![("/irsa", 200, tile)]);
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    let job = TestJob::new();
    let observation = json!({"obs_id":"wise","hips_id":"CDS/P/allWISE/W1"});
    let transfer = archive
        .survey_transfer(&observation, &tiny_grid())
        .await
        .unwrap();
    assert_eq!(transfer["transfer_estimated"], true);
    job.handle
        .files(vec![json!({"obs_id":"wise","transferred_bytes":0})])
        .unwrap();
    let path = archive
        .download(&observation, &tiny_grid(), &job.handle)
        .await
        .unwrap();
    let image = fits::Image::decode(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(image.header["AIPROV"], "irsa-hips-v1");
    assert_eq!(image.header["AIINTERP"], "bilinear");
    assert!(
        path.file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("-bilinear-v2.fits")
    );
    assert!(image.pixels.iter().all(|n| *n == 42.));
    assert_eq!(
        job.handle.transferred("wise"),
        transfer["transfer_bytes"].as_u64().unwrap()
    );
    assert!(job.handle.transferred("wise") >= tile_size);
    assert!(job.handle.transferred("wise") > std::fs::metadata(&path).unwrap().len());
    let mut shifted = tiny_grid();
    shifted["wcs"]["CRVAL1"] = json!(10.00001);
    assert_eq!(archive.cached_source(&observation, &shifted).unwrap(), None);
    assert_eq!(
        archive
            .survey_transfer(&observation, &shifted)
            .await
            .unwrap()["transfer_bytes"],
        0
    );
    assert_eq!(
        archive.cached_source(&observation, &tiny_grid()).unwrap(),
        Some(path)
    );
    server.join().unwrap();
}

#[tokio::test]
async fn irsa_fits_orientation_agrees_with_published_hpx_astrometry() {
    // Published HiPS HPX WCS, Norder=3/Npix=448: column=31, row=129.
    // https://hipspy.readthedocs.io/en/latest/drawing_algo.html#wcs-for-fits-tiles
    let pixels = (0..512)
        .flat_map(|row| (0..512).map(move |col| (1000 * row + col) as f32))
        .collect();
    let tile = fits::Image::new(512, 512, pixels, json!({}))
        .unwrap()
        .encode("fixture")
        .unwrap();
    let (endpoints, server) = mock(vec![(
        "/irsa/AllWISE/W1/Norder3/Dir0/Npix448.fits",
        200,
        tile,
    )]);
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    let job = TestJob::new();
    let grid = json!({"width":1,"height":1,"pixel_scale_arcsec":90.,
        "wcs":{"CTYPE1":"RA---TAN","CTYPE2":"DEC--TAN","CRPIX1":1.,"CRPIX2":1.,
            "CRVAL1":273.856201171875,"CRVAL2":-36.818624678015205,
            "CDELT1":-0.025,"CDELT2":0.025}});
    let path = archive
        .download(&json!({"hips_id":"CDS/P/allWISE/W1"}), &grid, &job.handle)
        .await
        .unwrap();
    let image = fits::Image::decode(&std::fs::read(path).unwrap()).unwrap();
    assert!(
        (image.pixels[0] - 129031.).abs() < 0.25,
        "{:?}",
        image.pixels
    );
    server.join().unwrap();
}

#[tokio::test]
async fn survey_build_replaces_estimates_with_received_payload_not_the_smaller_output_file() {
    let mut source_header = tiny_grid()["wcs"].clone();
    source_header["CRPIX1"] = json!(64.5);
    source_header["CRPIX2"] = json!(48.5);
    let source = fits::Image::new(
        128,
        96,
        (0..128 * 96).map(|p| p as f32).collect(),
        source_header,
    )
    .unwrap()
    .encode("source fixture")
    .unwrap();
    let received = source.len() as u64;
    let (endpoints, server) = mock(vec![("/dss", 200, source)]);
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    let session = root.path().join("sessions/transfer");
    std::fs::create_dir_all(&session).unwrap();
    let observation = json!({"obs_id":"survey-red","hips_id":"CDS/P/DSS2/red","survey_source":"stsci-dss-v1",
        "collection":"Survey","instrument":"DSS2","filter":"red","name":"DSS2 red","wavelength_nm":649.});
    let (view, _) = view();
    for (name, value) in [
        ("view.json", view),
        ("grid.json", tiny_grid()),
        ("session.json", json!({})),
        (
            "catalog.json",
            json!({"groups":[{"observations":[observation.clone()]}]}),
        ),
        ("selection.json", json!({"obs_ids":["survey-red"]})),
    ] {
        std::fs::write(session.join(name), serde_json::to_vec(&value).unwrap()).unwrap();
    }
    let mut plan = crate::build::plan(&session, &archive).unwrap();
    crate::build::transfer_plan(&session, &archive, &mut plan)
        .await
        .unwrap();
    assert!(plan["files"][0]["transfer_bytes"].as_u64().unwrap() > 0);
    assert_eq!(plan["files"][0]["transfer_estimated"], true);
    let job = TestJob::new();
    let _prepared = crate::build::prepare(&session, &archive, &job.handle)
        .await
        .unwrap();
    let complete = job
        .jobs
        .latest(Some("archive-test"))
        .unwrap()
        .files
        .remove(0);
    assert_eq!(complete["transfer_bytes"], received);
    assert_eq!(complete["transferred_bytes"], received);
    assert_eq!(complete["transfer_estimated"], false);
    assert!(complete["size_bytes"].as_u64().unwrap() < received);
    let cached = crate::build::plan(&session, &archive).unwrap();
    assert_eq!(cached["files"][0]["transfer_bytes"], 0);

    let unavailable = json!({"obs_id":"wise","hips_id":"CDS/P/allWISE/W1","filter":"W1"});
    std::fs::write(
        session.join("catalog.json"),
        serde_json::to_vec(&json!({"groups":[{"observations":[unavailable]}]})).unwrap(),
    )
    .unwrap();
    std::fs::write(session.join("selection.json"), br#"{"obs_ids":["wise"]}"#).unwrap();
    let mut missing = crate::build::plan(&session, &archive).unwrap();
    crate::build::transfer_plan(&session, &archive, &mut missing)
        .await
        .unwrap();
    assert!(missing["files"][0]["transfer_bytes"].is_null());
    assert!(
        missing["files"][0]["transfer_note"]
            .as_str()
            .unwrap()
            .contains("503")
    );
    server.join().unwrap();
}

#[tokio::test]
async fn build_overlaps_survey_requests_and_mdw_fields_within_the_shared_http_bound() {
    use axum::{
        Router,
        extract::{OriginalUri, Query},
        http::HeaderMap,
        routing::get,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Fixture {
        image: Vec<u8>,
        index: Vec<u8>,
        dss: tokio::sync::Barrier,
        mdw: tokio::sync::Barrier,
        mdw_requests: AtomicUsize,
        active: AtomicUsize,
        peak: AtomicUsize,
    }
    let mut index =
        "Image preview,Field,RA(hours),DEC(degrees),Instrumental,Calibrated,Star-Removed\n"
            .to_owned();
    for i in 0..8 {
        index.push_str(&format!("preview.png,{}, {},20,instrumental.fits,data_release/Fields/{}/calibrated.fits,starless.fits\n",
            2000 + i, 10. / 15. + i as f64 * 0.0000001, 2000 + i));
    }
    let fixture = Arc::new(Fixture {
        image: fits::Image::new(
            8,
            6,
            (0..48).map(|p| p as f32 + 1.).collect(),
            tiny_grid()["wcs"].clone(),
        )
        .unwrap()
        .encode("parallel fixture")
        .unwrap(),
        index: index.into_bytes(),
        dss: tokio::sync::Barrier::new(3),
        mdw: tokio::sync::Barrier::new(DOWNLOAD_CONCURRENCY),
        mdw_requests: AtomicUsize::new(0),
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
    });
    let shared = fixture.clone();
    let router = Router::new().fallback(get(
        move |OriginalUri(uri): OriginalUri,
              headers: HeaderMap,
              Query(query): Query<std::collections::HashMap<String, String>>| {
            let fixture = shared.clone();
            async move {
                if uri.path().starts_with("/mdw/js/") {
                    return fixture.index.clone();
                }
                if headers.contains_key(reqwest::header::RANGE)
                    || query.get("w").and_then(|v| v.parse::<f64>().ok()) == Some(0.1)
                {
                    return fixture.image.clone();
                }
                let active = fixture.active.fetch_add(1, Ordering::SeqCst) + 1;
                fixture.peak.fetch_max(active, Ordering::SeqCst);
                if uri.path() == "/dss" {
                    fixture.dss.wait().await;
                } else if fixture.mdw_requests.fetch_add(1, Ordering::SeqCst) < DOWNLOAD_CONCURRENCY
                {
                    fixture.mdw.wait().await;
                }
                fixture.active.fetch_sub(1, Ordering::SeqCst);
                fixture.image.clone()
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoints = Endpoints::at(&format!("http://{}", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    let session = root.path().join("sessions/parallel");
    std::fs::create_dir_all(&session).unwrap();
    let mut observations: Vec<_> = ["red", "blue", "NIR"].into_iter().map(|band| json!({
        "obs_id":format!("survey-{band}"),"hips_id":format!("CDS/P/DSS2/{band}"),
        "survey_source":"stsci-dss-v1","collection":"Survey","instrument":"DSS2","filter":band
    })).collect();
    observations.push(
        json!({"obs_id":"survey-mdw","survey_id":"MDW/Halpha","survey_source":"mdw-v1",
        "collection":"Survey","instrument":"MDW","filter":"Hα"}),
    );
    let ids: Vec<_> = observations
        .iter()
        .map(|row| row["obs_id"].clone())
        .collect();
    for (name, value) in [
        ("view.json", view().0),
        ("grid.json", tiny_grid()),
        ("session.json", json!({})),
        (
            "catalog.json",
            json!({"groups":[{"observations":observations}]}),
        ),
        ("selection.json", json!({"obs_ids":ids})),
    ] {
        std::fs::write(session.join(name), serde_json::to_vec(&value).unwrap()).unwrap();
    }
    let job = TestJob::new();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        crate::build::prepare(&session, &archive, &job.handle),
    )
    .await;
    server.abort();
    let _ = server.await;
    let prepared = result
        .expect("Science HTTP requests must overlap, not await one source at a time")
        .unwrap();
    drop(prepared);
    assert_eq!(fixture.mdw_requests.load(Ordering::SeqCst), 8);
    assert_eq!(fixture.peak.load(Ordering::SeqCst), DOWNLOAD_CONCURRENCY);
    let complete = job.jobs.latest(Some("archive-test")).unwrap();
    assert!(complete.files.iter().all(|file| file["state"] == "done"));
    for file in complete.files {
        let count = if file["obs_id"] == "survey-mdw" { 8 } else { 1 };
        assert_eq!(
            file["transferred_bytes"],
            fixture.image.len() as u64 * count
        );
    }
}

#[tokio::test]
async fn failed_download_group_drains_peers_without_cancelling_the_project_job() {
    let job = JobHandle::untracked();
    let ready = Arc::new(tokio::sync::Barrier::new(DOWNLOAD_CONCURRENCY));
    let exited = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let barrier = ready.clone();
    let finished = exited.clone();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        parallel_downloads(0..8, &job, move |index, handle| {
            let ready = barrier.clone();
            let exited = finished.clone();
            async move {
                ready.wait().await;
                if index == 0 {
                    bail!("Source fixture failed");
                }
                let result = interruptible(std::future::pending::<()>(), Some(&handle)).await;
                exited.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                result
            }
        }),
    )
    .await
    .unwrap();
    assert_eq!(result.unwrap_err().to_string(), "Source fixture failed");
    assert_eq!(
        exited.load(std::sync::atomic::Ordering::SeqCst),
        DOWNLOAD_CONCURRENCY - 1
    );
    assert!(!job.cancelled());
}

#[tokio::test]
async fn compressed_http_tiles_count_wire_payload_not_inflated_fits_bytes() {
    use axum::{Router, http::header, routing::get};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let tile = fits::Image::new(512, 512, vec![42.; 512 * 512], json!({}))
        .unwrap()
        .encode("gzip fixture")
        .unwrap();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tile).unwrap();
    let encoded = encoder.finish().unwrap();
    let payload = encoded.len() as u64;
    assert!(payload < tile.len() as u64);
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    let router = Router::new().route(
        "/{*path}",
        get(move || {
            let encoded = encoded.clone();
            let seen = seen.clone();
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                ([(header::CONTENT_ENCODING, "gzip")], encoded)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoints = Endpoints::at(&format!("http://{}", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    let job = TestJob::new();
    job.handle
        .files(vec![json!({"obs_id":"gzip","transferred_bytes":0})])
        .unwrap();
    let path = archive
        .download(
            &json!({"obs_id":"gzip","hips_id":"CDS/P/allWISE/W1"}),
            &tiny_grid(),
            &job.handle,
        )
        .await
        .unwrap();
    let image = fits::Image::decode(&std::fs::read(path).unwrap()).unwrap();
    assert!(image.pixels.iter().all(|value| *value == 42.));
    assert_eq!(
        job.handle.transferred("gzip"),
        payload * requests.load(Ordering::SeqCst) as u64
    );
    server.abort();
}

#[tokio::test]
async fn dss_transfer_uses_each_bands_source_sampling_and_full_fits_overhead() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let response = |scale: f64, extra: bool| {
        let mut header = tiny_grid()["wcs"].clone();
        for key in ["CD1_1", "CD1_2", "CD2_1", "CD2_2"] {
            header[key] = json!(header[key].as_f64().unwrap() * scale);
        }
        if extra {
            for i in 0..40 {
                header[format!("EXTRA{i}")] = json!(i);
            }
        }
        fits::Image::new(8, 6, vec![42.; 48], header)
            .unwrap()
            .encode("DSS fixture")
            .unwrap()
    };
    let red = response(1., false);
    let nir = response(1.7, false);
    let blue = response(1.7, true);
    let extra = blue.len() as u64 - nir.len() as u64;
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let (endpoints, server) = mock_response(move |path| {
        let url = reqwest::Url::parse(&format!("http://fixture{path}")).unwrap();
        let params: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(params["h"], "0.1");
        seen.fetch_add(1, Ordering::SeqCst);
        (
            200,
            match params["v"].as_str() {
                "poss2ukstu_red" => red.clone(),
                "poss2ukstu_blue" => blue.clone(),
                "poss2ukstu_ir" => nir.clone(),
                _ => panic!("Unexpected DSS band"),
            },
        )
    });
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    let capture = crate::sky::coordinate_capture(
        &json!({"ra":10.,"dec":20.,"width_arcmin":30.,"aspect":"3:2"}),
    )
    .unwrap();
    let grid = crate::grid::view_grid(&capture.view, 1., 128).unwrap();
    let observation =
        |band| json!({"hips_id":format!("CDS/P/DSS2/{band}"),"survey_source":"stsci-dss-v1"});
    let mut sizes = Vec::new();
    for band in ["red", "blue", "NIR"] {
        sizes.push(
            archive
                .survey_transfer(&observation(band), &grid)
                .await
                .unwrap()["transfer_bytes"]
                .as_u64()
                .unwrap(),
        );
    }
    assert!(sizes[2] < sizes[0] / 2);
    assert_eq!(sizes[1] - sizes[2], extra);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    archive
        .survey_transfer(&observation("blue"), &grid)
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    server.join().unwrap();
}

#[tokio::test]
async fn coverage_keeps_survey_results_when_mast_is_unavailable() {
    let (endpoints, server) = mock(vec![(
        "/moc",
        200,
        br#"[{"ID":"CDS/P/DSS2/red"}]"#.to_vec(),
    )]);
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    let result = archive
        .coverage(&json!({"ra":10.0,"dec":20.0,"width_arcmin":10.0,
        "aspect":"16:9","rotation_deg":30.0}))
        .await
        .unwrap();
    assert_eq!(result["counts"], json!({"HST":0,"JWST":0}));
    assert_eq!(result["pending"], false);
    assert!(result["warning"].as_str().unwrap().contains("MAST"));
    assert_eq!(
        result["survey_groups"][0]["observations"][0]["hips_id"],
        "CDS/P/DSS2/red"
    );
    assert_eq!(result["frame"]["width_arcmin"], 10.);
    server.join().unwrap();
}

#[tokio::test]
async fn cancellation_during_chunked_science_transfer_removes_staging_and_cache() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoints = Endpoints::at(&format!("http://{}", listener.local_addr().unwrap()));
    let (started, receive) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let bytes = gradient_fits();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|p| p == b"\r\n\r\n") {
            let mut chunk = [0; 4096];
            let length = socket.read(&mut chunk).unwrap();
            if length == 0 {
                return;
            }
            request.extend_from_slice(&chunk[..length]);
        }
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nB40\r\n"
        )
        .unwrap();
        socket.write_all(&bytes[..2880]).unwrap();
        socket.write_all(b"\r\n").unwrap();
        started.send(()).unwrap();
        let _ = wait.recv_timeout(Duration::from_secs(4));
    });
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::with_endpoints(root.path(), endpoints).unwrap();
    let job = TestJob::new();
    let handle = job.handle.clone();
    let transfer = tokio::spawn(async move {
        archive
            .download(
                &json!({"data_uri":"mast:HST/product/interrupted_drz.fits"}),
                &json!({}),
                &handle,
            )
            .await
    });
    tokio::task::spawn_blocking(move || receive.recv_timeout(Duration::from_secs(3)).unwrap())
        .await
        .unwrap();
    job.jobs.cancel(None, Some("archive-test"));
    let error = tokio::time::timeout(Duration::from_secs(2), transfer)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("Cancelled"));
    release.send(()).unwrap();
    server.join().unwrap();
    assert_eq!(
        std::fs::read_dir(root.path().join("downloads"))
            .unwrap()
            .count(),
        0
    );
}
