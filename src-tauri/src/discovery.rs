//! Public archive reconnaissance and science-only downloads for the native workflow.
//!
//! Every cache lives beneath the configured library root. Remote endpoints are kept
//! separate so failure of CDS never disables MAST, Harvard, STScI, or IRSA.
use crate::jobs::JobHandle;
use crate::sky::SkyWcs;
use anyhow::{Result, bail, ensure};
use reqwest::{Client, RequestBuilder};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

#[path = "discovery/fits.rs"]
mod fits;
#[path = "discovery/survey.rs"]
mod survey;
#[cfg(test)]
#[path = "discovery/tests.rs"]
mod tests;
#[path = "discovery/wcs.rs"]
mod wcs;

const JSON_LIMIT: usize = 8 * 1024 * 1024;
const SURVEY_LIMIT: usize = 128 * 1024 * 1024;
const SCIENCE_LIMIT: usize = 2 * 1024 * 1024 * 1024;
const DOWNLOAD_CONCURRENCY: usize = 4;

/// Service locations, injectable for deterministic, network-free fixture tests.
#[derive(Clone, Debug)]
pub struct Endpoints {
    pub mast: String,
    pub mast_download: String,
    pub harvard: String,
    pub sesame: String,
    pub moc: String,
    pub hips2fits: Vec<String>,
    pub dss: String,
    pub ps1: String,
    pub irsa: String,
    pub mdw: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            mast: "https://mast.stsci.edu/api/v0/invoke".into(),
            mast_download: "https://mast.stsci.edu/api/v0.1/Download/file".into(),
            harvard: "https://simbad.cfa.harvard.edu/simbad/sim-id".into(),
            sesame: "https://cds.unistra.fr/cgi-bin/nph-sesame/-oxp/SNV".into(),
            moc: "https://alasky.cds.unistra.fr/MocServer/query".into(),
            hips2fits: vec![
                "https://alasky.cds.unistra.fr/hips-image-services/hips2fits".into(),
                "https://alaskybis.cds.unistra.fr/hips-image-services/hips2fits".into(),
            ],
            dss: "https://archive.stsci.edu/cgi-bin/dss_search".into(),
            ps1: "https://ps1images.stsci.edu/cgi-bin".into(),
            irsa: "https://irsa.ipac.caltech.edu/data/hips/CDS".into(),
            mdw: "https://mdw.astro.columbia.edu".into(),
        }
    }
}

#[cfg(test)]
impl Endpoints {
    fn at(url: &str) -> Self {
        Self {
            mast: format!("{url}/mast"),
            mast_download: format!("{url}/download"),
            harvard: format!("{url}/harvard"),
            sesame: format!("{url}/sesame"),
            moc: format!("{url}/moc"),
            hips2fits: vec![format!("{url}/hips")],
            dss: format!("{url}/dss"),
            ps1: format!("{url}/ps1"),
            irsa: format!("{url}/irsa"),
            mdw: format!("{url}/mdw"),
        }
    }
}

pub(crate) fn survey_id(observation: &Value) -> Option<&str> {
    observation["survey_id"]
        .as_str()
        .or_else(|| observation["hips_id"].as_str())
}

/// Cloneable archive client. Files returned by `download` are absolute, portable
/// library-cache references, not preview/display images.
#[derive(Clone)]
pub struct Archive {
    root: PathBuf,
    client: Client,
    endpoints: Endpoints,
    slots: Arc<Semaphore>,
}

impl Archive {
    pub fn new(data_root: &Path) -> Result<Self> {
        Self::with_endpoints(data_root, Endpoints::default())
    }

    pub fn with_endpoints(data_root: &Path, endpoints: Endpoints) -> Result<Self> {
        std::fs::create_dir_all(data_root)?;
        let root = data_root.canonicalize()?;
        std::fs::create_dir_all(root.join("downloads"))?;
        std::fs::create_dir_all(root.join("cache"))?;
        let client = Client::builder()
            .user_agent("NightBloom/1.0 (native astronomy science archive)")
            // Account for compressed payloads before HTTP decompression.
            .no_gzip()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(90))
            .redirect(reqwest::redirect::Policy::limited(4))
            .build()?;
        Ok(Self {
            root,
            client,
            endpoints,
            slots: Arc::new(Semaphore::new(DOWNLOAD_CONCURRENCY)),
        })
    }

    /// Discover independently usable MAST and survey groups. Retry routes may use
    /// `mast_groups`/`survey_groups`; `force_mast` also overrides automatic limits.
    pub async fn discover(&self, view: &Value, header: &Value, job: &JobHandle) -> Result<Value> {
        check(Some(job))?;
        let search_mast = view["force_mast"].as_bool().unwrap_or(false)
            || (!view["surveys_only"].as_bool().unwrap_or(false)
                && field(view)?.iter().copied().fold(0., f64::max) <= 30.);
        job.stage("search", 0., "Checking astronomy archives independently")?;
        let (mast, surveys) = tokio::join!(
            async {
                if search_mast {
                    self.mast_groups(view, header, job).await
                } else {
                    Ok(
                        json!({"groups":[],"observations_found":0,"observations_in_view":0,"mast_searched":false}),
                    )
                }
            },
            self.survey_groups(view, header, job)
        );
        check(Some(job))?;
        let mut result = mast?;
        let surveys = surveys?;
        result["groups"]
            .as_array_mut()
            .unwrap()
            .extend(surveys["groups"].as_array().unwrap().iter().cloned());
        result["observations_in_view"] = json!(
            result["groups"]
                .as_array()
                .unwrap()
                .iter()
                .map(|g| g["observations"].as_array().map_or(0, Vec::len))
                .sum::<usize>()
        );
        if let Some(warning) = surveys.get("survey_warning") {
            result["survey_warning"] = warning.clone();
        }
        job.stage("search", 1., "Archive checks finished")?;
        Ok(result)
    }

    /// Always perform a MAST query, including on wide/surveys-only retry routes.
    /// Outages are explicit in `mast_warning` while successful empty searches are not.
    pub async fn mast_groups(
        &self,
        view: &Value,
        header: &Value,
        job: &JobHandle,
    ) -> Result<Value> {
        let (width, height) = dimensions(view, header)?;
        let wcs = SkyWcs::new(header, width, height)?;
        let center = pair(&view["center_icrs_deg"])?;
        let radius = search_radius(view)?;
        check(Some(job))?;
        let rows = self.mast_rows(center, radius / 60., Some(job)).await;
        check(Some(job))?;
        match rows {
            Ok(rows) => {
                let observations: Vec<_> = rows.iter().filter_map(normalize_observation).collect();
                let found = observations.len();
                let visible: Vec<_> = observations
                    .into_iter()
                    .filter_map(|mut row| {
                        let pixels = project(text(&row, "region"), &wcs, height);
                        let point = [row["ra"].as_f64()?, row["dec"].as_f64()?];
                        let inside = wcs.sky_to_pixel(point).is_some_and(|p| {
                            p[0] >= -0.5
                                && p[0] <= width as f64 - 0.5
                                && p[1] >= -0.5
                                && p[1] <= height as f64 - 0.5
                        });
                        let crossing = pixels
                            .iter()
                            .any(|polygon| intersects_rect(polygon, width, height));
                        if !inside && !crossing {
                            return None;
                        }
                        row["pixels"] = json!(pixels);
                        Some(row)
                    })
                    .collect();
                Ok(
                    json!({"groups":group_observations(&visible), "observations_found":found,
                    "observations_in_view":visible.len(), "radius_arcmin":radius,"mast_searched":true}),
                )
            }
            Err(error) => Ok(
                json!({"groups":[],"observations_found":0,"observations_in_view":0,
                "radius_arcmin":radius,"mast_searched":true,"mast_warning":error.to_string()}),
            ),
        }
    }

    /// Survey-only counterpart returning `{groups, survey_warning?}`.
    pub async fn survey_groups(
        &self,
        view: &Value,
        header: &Value,
        job: &JobHandle,
    ) -> Result<Value> {
        let (width, height) = dimensions(view, header)?;
        let wcs = SkyWcs::new(header, width, height)?;
        let (bands, warning) = self.available_surveys(view, header, Some(job)).await?;
        check(Some(job))?;
        let mut result = json!({"groups":survey::groups(view, &wcs, height, &bands)?});
        if let Some(warning) = warning {
            result["survey_warning"] = json!(warning);
        }
        Ok(result)
    }

    /// Python `/coordinates/resolve` response. Numeric inputs never use a network.
    pub async fn resolve(&self, query: &str) -> Result<Value> {
        let query = query.trim();
        ensure!(query.len() <= 200, "Target is too long.");
        if let Some([ra, dec]) = parse_position(query)? {
            return Ok(json!({"name":format!("RA {ra:.4} Dec {dec:.4}"),"ra":ra,"dec":dec}));
        }
        // Design decision: Harvard is independent of CDS; try it even if CDS DNS
        // or routing is down rather than letting a local CDS outage block names.
        let (mast, harvard) = tokio::join!(
            self.mast_request(
                "Mast.Name.Lookup",
                json!({"input":query,"format":"json"}),
                None
            ),
            self.http_bytes(
                self.client
                    .get(&self.endpoints.harvard)
                    .query(&[("Ident", query), ("output.format", "VOTable")])
                    .timeout(Duration::from_secs(7)),
                JSON_LIMIT,
                None,
                "Harvard SIMBAD"
            )
        );
        if let Ok(result) = mast
            && let Some(row) = result["resolvedCoordinate"]
                .as_array()
                .and_then(|a| a.first())
            && let (Some(ra), Some(dec)) = (row["ra"].as_f64(), row["decl"].as_f64())
        {
            validate_position([ra, dec])?;
            let name = row["canonicalName"]
                .as_str()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or(query);
            return Ok(
                json!({"name":name.split_whitespace().collect::<Vec<_>>().join(" "),"ra":ra,"dec":dec}),
            );
        }
        if let Ok(bytes) = harvard
            && let Some(position) = parse_resolver(&bytes)
        {
            return Ok(json!({"name":query,"ra":position[0],"dec":position[1]}));
        }
        let mut sesame_url = reqwest::Url::parse(&self.endpoints.sesame)?;
        let mut encoding = reqwest::Url::parse("http://localhost/")?;
        encoding.query_pairs_mut().append_pair("target", query);
        let encoded = encoding
            .query()
            .and_then(|s| s.strip_prefix("target="))
            .unwrap_or("")
            .replace('+', "%20");
        sesame_url.set_query(Some(&encoded));
        let bytes = self
            .http_bytes(
                self.client.get(sesame_url).timeout(Duration::from_secs(7)),
                JSON_LIMIT,
                None,
                "CDS Sesame",
            )
            .await;
        if let Ok(bytes) = &bytes
            && let Some(position) = parse_resolver(bytes)
        {
            return Ok(json!({"name":query,"ra":position[0],"dec":position[1]}));
        }
        if bytes.is_err() {
            bail!("Name lookup services are unavailable. Try decimal RA and Dec, or retry later.");
        }
        bail!("No matching astronomical target was found. Try a catalog name or RA and Dec.")
    }

    /// Python coverage response plus independent `survey_groups`/`survey_warning`.
    /// `pending` is false because each native request completes within bounded time.
    pub async fn coverage(&self, request: &Value) -> Result<Value> {
        let capture = crate::sky::coordinate_capture(request)?;
        let center = pair(&capture.view["center_icrs_deg"])?;
        let aspect = request["aspect"].as_str().unwrap_or("16:9");
        let (rows, surveys) = tokio::join!(
            self.mast_rows(center, 0.1, None),
            self.available_surveys(&capture.view, &capture.header, None)
        );
        let (rows, warning) = match rows {
            Ok(rows) => (rows, None),
            Err(error) => (vec![], Some(error.to_string())),
        };
        let mut counts = BTreeMap::from([("HST".to_string(), 0), ("JWST".to_string(), 0)]);
        let mut instruments = BTreeMap::new();
        let mut seen = BTreeSet::new();
        for row in &rows {
            if !seen.insert(text(row, "obs_id").to_string()) {
                continue;
            }
            let mission = text(row, "obs_collection");
            if let Some(count) = counts.get_mut(mission) {
                *count += 1;
            }
            *instruments
                .entry((
                    mission.to_string(),
                    text(row, "instrument_name").to_string(),
                ))
                .or_insert(0) += 1;
        }
        let instrument_rows: Vec<_> = instruments.iter().map(|((mission,instrument), count)|
            json!({"mission":mission,"instrument":instrument,"count":count})).collect();
        let frame = suggest_frame(center, &rows, aspect)?;
        let mut result = json!({"counts":counts,"instruments":instrument_rows,
            "frame":frame,"warning":warning,"pending":false});
        let (bands, warning) = surveys?;
        let wcs = capture.wcs()?;
        let (_, height) = dimensions(&capture.view, &capture.header)?;
        result["survey_groups"] = json!(survey::groups(&capture.view, &wcs, height, &bands)?);
        if let Some(warning) = warning {
            result["survey_warning"] = json!(warning);
        }
        Ok(result)
    }

    /// Cache-only science lookup for build plans. Performs no network requests;
    /// returns `None` when the selected source/grid has not been downloaded.
    pub fn cached_source(&self, observation: &Value, grid: &Value) -> Result<Option<PathBuf>> {
        if survey_id(observation).is_some() {
            return self.cached_survey(observation, grid);
        }
        let uri = text(observation, "data_uri");
        ensure!(
            uri.starts_with("mast:") && uri.len() <= 2048,
            "Expected a MAST science data reference."
        );
        let filename = uri.rsplit('/').next().unwrap_or("");
        ensure!(
            safe_filename(filename),
            "The archive supplied an unsafe FITS filename."
        );
        ensure!(
            !filename.to_ascii_lowercase().contains("_total"),
            "Detection images cannot be used as science layers."
        );
        let path = self.root.join("downloads").join(filename);
        if path.is_file() {
            fits::validate(&path)?;
            verify_size(observation, &path)?;
            Ok(Some(path))
        } else {
            Ok(None)
        }
    }

    /// Fetch calibrated archive FITS or exact-grid survey FITS. Staging files are
    /// private and are dropped on errors/cancellation; publication is atomic.
    pub async fn download(
        &self,
        observation: &Value,
        grid: &Value,
        job: &JobHandle,
    ) -> Result<PathBuf> {
        check(Some(job))?;
        if survey_id(observation).is_some() {
            let identifier = observation["obs_id"].as_str().unwrap_or("");
            return self
                .download_survey(observation, grid, &job.for_file(identifier))
                .await;
        }
        let uri = text(observation, "data_uri");
        ensure!(
            uri.starts_with("mast:") && uri.len() <= 2048,
            "Expected a MAST science data reference."
        );
        let filename = uri.rsplit('/').next().unwrap_or("");
        ensure!(
            safe_filename(filename),
            "The archive supplied an unsafe FITS filename."
        );
        ensure!(
            !filename.to_ascii_lowercase().contains("_total"),
            "Detection images cannot be used as science layers."
        );
        let path = self.root.join("downloads").join(filename);
        if path.is_file() {
            fits::validate(&path)?;
            verify_size(observation, &path)?;
            check(Some(job))?;
            return Ok(path);
        }
        job.stage(
            "download",
            0.,
            &format!("Downloading science FITS: {filename}"),
        )?;
        let mut staging = tempfile::Builder::new()
            .suffix(".part")
            .tempfile_in(path.parent().unwrap())?;
        let identifier = text(observation, "obs_id");
        let known_size = observation["size_bytes"].as_u64().filter(|n| *n > 0);
        let mut transferred = 0u64;
        job.transfer(identifier, 0, known_size)?;
        self.http_to(
            self.client
                .get(&self.endpoints.mast_download)
                .query(&[("uri", uri)])
                .timeout(Duration::from_secs(240)),
            SCIENCE_LIMIT,
            Some(job),
            "MAST science download",
            |bytes, total| {
                staging.write_all(bytes)?;
                transferred += bytes.len() as u64;
                job.transfer(identifier, transferred, total.or(known_size))
            },
        )
        .await?;
        staging.flush()?;
        fits::validate(staging.path())?;
        verify_size(observation, staging.path())?;
        job.transfer(identifier, transferred, Some(transferred))?;
        check(Some(job))?;
        publish(staging, &path)?;
        Ok(path)
    }

    async fn mast_rows(
        &self,
        center: [f64; 2],
        radius_deg: f64,
        job: Option<&JobHandle>,
    ) -> Result<Vec<Value>> {
        let mut rows = Vec::new();
        for page in 1..=8 {
            let params = json!({
                "columns":"obsid,obs_id,obs_collection,instrument_name,filters,proposal_id,target_name,obs_title,s_region,s_ra,s_dec,em_min,em_max,t_exptime,t_min,dataURL,jpegURL",
                "filters":[
                    {"paramName":"obs_collection","values":["HST","JWST"]},
                    {"paramName":"dataproduct_type","values":["image"]},
                    {"paramName":"intentType","values":["science"]},
                    {"paramName":"calib_level","values":["3"]},
                    {"paramName":"dataRights","values":["PUBLIC"]}
                ],
                "position":format!("{},{},{}",center[0],center[1],radius_deg)
            });
            let response = self
                .mast_request_page("Mast.Caom.Filtered.Position", params, page, job)
                .await?;
            let data = response["data"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("MAST returned invalid archive metadata."))?;
            rows.extend(data.iter().cloned());
            let pages = response["paging"]["pagesFiltered"]
                .as_u64()
                .or_else(|| response["paging"]["pages"].as_u64())
                .unwrap_or(1);
            if page >= pages {
                return Ok(rows);
            }
        }
        bail!("MAST returned too many observations. Narrow the sky frame and retry.")
    }

    async fn mast_request(
        &self,
        service: &str,
        params: Value,
        job: Option<&JobHandle>,
    ) -> Result<Value> {
        self.mast_request_page(service, params, 1, job).await
    }

    async fn mast_request_page(
        &self,
        service: &str,
        params: Value,
        page: u64,
        job: Option<&JobHandle>,
    ) -> Result<Value> {
        let payload =
            json!({"service":service,"params":params,"format":"json","pagesize":10000,"page":page});
        let bytes = self
            .http_bytes(
                self.client
                    .post(&self.endpoints.mast)
                    .form(&[("request", payload.to_string())])
                    .timeout(Duration::from_secs(12)),
                JSON_LIMIT,
                job,
                "MAST",
            )
            .await?;
        let result: Value = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("MAST returned invalid archive metadata."))?;
        ensure!(
            !matches!(result["status"].as_str(), Some("ERROR" | "EXECUTING")),
            "MAST is not responding with completed archive results. Retry later."
        );
        Ok(result)
    }

    async fn http_bytes(
        &self,
        request: RequestBuilder,
        limit: usize,
        job: Option<&JobHandle>,
        label: &str,
    ) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.http_to(request, limit, job, label, |chunk, _total| {
            bytes.extend_from_slice(chunk);
            Ok(())
        })
        .await?;
        Ok(bytes)
    }

    async fn http_to(
        &self,
        request: RequestBuilder,
        limit: usize,
        job: Option<&JobHandle>,
        label: &str,
        mut write: impl FnMut(&[u8], Option<u64>) -> Result<()>,
    ) -> Result<()> {
        let _slot = interruptible(self.slots.acquire(), job)
            .await?
            .map_err(|_| anyhow::anyhow!("Archive client is shutting down."))?;
        let mut response = interruptible(
            request
                .header(reqwest::header::ACCEPT_ENCODING, "identity")
                .send(),
            job,
        )
        .await?
        .map_err(|e| remote_error(label, &e))?;
        ensure!(
            response.status().is_success(),
            "{label} is unavailable (HTTP {}). Retry later.",
            response.status().as_u16()
        );
        ensure!(
            response.content_length().is_none_or(|n| n <= limit as u64),
            "{label} exceeds the safe download size limit."
        );
        let total = response.content_length();
        let mut received = 0usize;
        while let Some(chunk) = interruptible(response.chunk(), job)
            .await?
            .map_err(|e| remote_error(label, &e))?
        {
            received = received
                .checked_add(chunk.len())
                .ok_or_else(|| anyhow::anyhow!("Download exceeds the size limit."))?;
            ensure!(
                received <= limit,
                "{label} exceeds the safe download size limit."
            );
            check(job)?;
            if let Some(job) = job {
                job.received(chunk.len() as u64)?;
            }
            write(&chunk, total)?;
        }
        check(job)
    }
}

pub(crate) async fn interruptible<T>(
    future: impl Future<Output = T>,
    job: Option<&JobHandle>,
) -> Result<T> {
    tokio::pin!(future);
    loop {
        check(job)?;
        tokio::select! {
            result = &mut future => { check(job)?; return Ok(result); },
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
}

pub(crate) async fn parallel_downloads<T, R, F, Fut>(
    items: impl IntoIterator<Item = T>,
    job: &JobHandle,
    download: F,
) -> Result<Vec<R>>
where
    T: Send + 'static,
    R: Send + 'static,
    F: Fn(T, JobHandle) -> Fut,
    Fut: Future<Output = Result<R>> + Send + 'static,
{
    let group = job.fork();
    let mut pending = items.into_iter().enumerate();
    let mut tasks = tokio::task::JoinSet::new();
    let mut results = Vec::new();
    let mut error = None;
    loop {
        while tasks.len() < DOWNLOAD_CONCURRENCY && !group.cancelled() {
            let Some((index, item)) = pending.next() else {
                break;
            };
            let future = download(item, group.clone());
            tasks.spawn(async move { (index, future.await) });
        }
        let Some(result) = tasks.join_next().await else {
            break;
        };
        match result {
            Ok((index, Ok(result))) => results.push((index, result)),
            result => {
                if error.is_none() {
                    error = Some(match result {
                        Ok((_, Err(error))) => error,
                        Err(error) => error.into(),
                        _ => unreachable!(),
                    });
                    group.stop();
                }
            }
        }
    }
    // Drain cooperatively: aborting a task awaiting spawn_blocking could leave
    // science processing or cache publication running after the build ends.
    if let Some(error) = error {
        return Err(error);
    }
    check(Some(job))?;
    results.sort_by_key(|(index, _)| *index);
    Ok(results.into_iter().map(|(_, result)| result).collect())
}

fn check(job: Option<&JobHandle>) -> Result<()> {
    if job.is_some_and(JobHandle::cancelled) {
        bail!("Cancelled");
    }
    Ok(())
}

fn remote_error(label: &str, error: &reqwest::Error) -> anyhow::Error {
    // Never echo request URLs, credentials, or remote response bodies.
    anyhow::anyhow!(
        "{label} {}. Retry later or use an independent source.",
        if error.is_timeout() {
            "timed out"
        } else {
            "could not be reached"
        }
    )
}

fn publish(staging: tempfile::NamedTempFile, destination: &Path) -> Result<()> {
    staging.as_file().sync_all()?;
    match staging.persist_noclobber(destination) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            fits::validate(destination)
        }
        Err(_) => bail!("Could not publish the downloaded science cache file."),
    }
}

fn hash(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value.as_ref()))
}

fn safe_filename(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 240
        && name.to_ascii_lowercase().ends_with(".fits")
        && !name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        && !name.ends_with([' ', '.'])
        && !name.starts_with('.')
        && !["CON", "PRN", "AUX", "NUL", "COM1", "LPT1"].contains(
            &name
                .split('.')
                .next()
                .unwrap_or("")
                .to_ascii_uppercase()
                .as_str(),
        )
}

fn verify_size(observation: &Value, path: &Path) -> Result<()> {
    if let Some(size) = observation["size_bytes"].as_u64().filter(|n| *n > 0) {
        ensure!(
            path.metadata()?.len() == size,
            "The MAST science FITS file has an unexpected size. Clear this cached file and retry."
        );
    }
    Ok(())
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("").trim()
}

fn numeric(value: &Value, key: &str) -> Option<f64> {
    value[key]
        .as_f64()
        .or_else(|| value[key].as_str()?.parse().ok())
        .filter(|n| n.is_finite())
}

fn pair(value: &Value) -> Result<[f64; 2]> {
    let a = value
        .as_array()
        .filter(|a| a.len() == 2)
        .ok_or_else(|| anyhow::anyhow!("Invalid sky coordinate pair."))?;
    let p = [
        a[0].as_f64().unwrap_or(f64::NAN),
        a[1].as_f64().unwrap_or(f64::NAN),
    ];
    ensure!(
        p.iter().all(|n| n.is_finite()),
        "Sky coordinates must be finite."
    );
    Ok(p)
}

fn field(view: &Value) -> Result<[f64; 2]> {
    let f = pair(&view["field_arcmin"])?;
    ensure!(f.iter().all(|n| *n > 0.), "Invalid sky frame dimensions.");
    Ok(f)
}

fn dimensions(view: &Value, header: &Value) -> Result<(u32, u32)> {
    let array = view["pixel_dimensions"].as_array();
    let width = array
        .and_then(|a| a.first())
        .and_then(Value::as_u64)
        .or_else(|| header["NAXIS1"].as_u64())
        .unwrap_or(0);
    let height = array
        .and_then(|a| a.get(1))
        .and_then(Value::as_u64)
        .or_else(|| header["NAXIS2"].as_u64())
        .unwrap_or(0);
    ensure!(
        width > 0 && height > 0 && width <= 65536 && height <= 65536,
        "Invalid captured frame size."
    );
    Ok((width as u32, height as u32))
}

fn normalize_filter(value: &str) -> Option<String> {
    let mut labels = Vec::new();
    for label in value
        .split([';', '+'])
        .map(|s| s.trim().to_ascii_uppercase())
    {
        if ["DETECTION", "TOTAL", "UNKNOWN"].contains(&label.as_str()) {
            return None;
        }
        if ["", "CLEAR", "OPEN", "N/A", "NONE"].contains(&label.as_str())
            || label.starts_with("CLEAR")
            || label.starts_with("OPEN")
        {
            continue;
        }
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    (!labels.is_empty()).then(|| labels.join("+"))
}

fn normalize_observation(row: &Value) -> Option<Value> {
    let label = normalize_filter(text(row, "filters"))?;
    let uri = text(row, "dataURL");
    if !uri.starts_with("mast:")
        || !uri.to_ascii_lowercase().ends_with(".fits")
        || uri.to_ascii_lowercase().contains("_total")
    {
        return None;
    }
    let collection = text(row, "obs_collection");
    if !["HST", "JWST"].contains(&collection) {
        return None;
    }
    let ra = numeric(row, "s_ra")?;
    let dec = numeric(row, "s_dec")?;
    validate_position([ra, dec]).ok()?;
    let wavelength = numeric(row, "em_min")
        .zip(numeric(row, "em_max"))
        .map(|(a, b)| (a + b) / 2.);
    let date = numeric(row, "t_min")
        .filter(|d| *d >= -100000. && *d <= 200000.)
        .and_then(|d| {
            chrono::NaiveDate::from_ymd_opt(1858, 11, 17)?
                .checked_add_signed(chrono::Duration::days(d.floor() as i64))
        })
        .map(|d| d.to_string())
        .unwrap_or_default();
    let obsid = row["obsid"]
        .as_str()
        .map(str::to_string)
        .or_else(|| row["obsid"].as_u64().map(|n| n.to_string()))
        .unwrap_or_else(|| hash(uri)[..20].into());
    let obs_id = if text(row, "obs_id").is_empty() {
        obsid.as_str()
    } else {
        text(row, "obs_id")
    };
    Some(json!({
        "obsid":obsid,"obs_id":obs_id,"collection":collection,"instrument":text(row,"instrument_name"),
        "filter":label,"proposal":row["proposal_id"].as_str().map(str::to_string)
            .or_else(|| row["proposal_id"].as_u64().map(|n| n.to_string())).unwrap_or_default(),
        "target":text(row,"target_name"),"title":text(row,"obs_title"),"region":text(row,"s_region"),
        "ra":ra,"dec":dec,"wavelength_nm":wavelength,"exptime_s":numeric(row,"t_exptime").unwrap_or(0.),
        "date":date,"data_uri":uri,"preview_uri":text(row,"jpegURL"),"info":describe(&label,wavelength),"pixels":[]
    }))
}

fn field_family(filename: &str, collection: &str, observation: &str) -> (String, String) {
    let filename = filename.to_ascii_lowercase();
    let name = ["_i2d.fits", "_drc.fits", "_drz.fits"]
        .iter()
        .find_map(|s| filename.strip_suffix(s))
        .unwrap_or(&filename);
    let parts: Vec<_> = name.split('_').collect();
    if let Some((program, field)) = parts.first().and_then(|s| s.split_once('-'))
        && program.len() == 7
        && program.starts_with("jw")
        && program[2..].bytes().all(|c| c.is_ascii_digit())
        && (field.starts_with('o') || field.starts_with('c'))
        && parts.get(1).is_some_and(|p| p.starts_with('t'))
    {
        return (format!("{field}_{}", parts[1]), "JWST mosaic".into());
    }
    if parts.first() == Some(&"hst") && parts.get(1).is_some_and(|p| p.starts_with("skycell-")) {
        return (parts[1].into(), "HAP skycell".into());
    }
    if parts.first() == Some(&"hst")
        && parts.len() >= 4
        && parts[1].bytes().all(|c| c.is_ascii_digit())
    {
        let family = match parts.last().copied() {
            Some("pc") => "HLA PC",
            Some("wf") => "HLA WF",
            _ => "HAP visit mosaic",
        };
        return (format!("visit-{}", parts[2]), family.into());
    }
    if collection == "HST" && name.len() == 9 && name.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return (format!("visit-{}", &name[4..6]), "Standard".into());
    }
    (
        if observation.is_empty() {
            name.into()
        } else {
            observation.into()
        },
        "Other".into(),
    )
}

fn group_observations(observations: &[Value]) -> Vec<Value> {
    let mut buckets: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for row in observations {
        let filename = text(row, "data_uri").rsplit('/').next().unwrap_or("");
        let (field, family) = field_family(filename, text(row, "collection"), text(row, "obs_id"));
        let key = [
            text(row, "collection"),
            text(row, "proposal"),
            text(row, "instrument"),
            &field,
            &family,
        ]
        .join("|");
        buckets.entry(key).or_default().push(row.clone());
    }
    let mut groups = Vec::new();
    for (key, mut members) in buckets {
        members.sort_by(|a, b| {
            a["wavelength_nm"]
                .as_f64()
                .unwrap_or(f64::INFINITY)
                .total_cmp(&b["wavelength_nm"].as_f64().unwrap_or(f64::INFINITY))
                .then_with(|| {
                    b["exptime_s"]
                        .as_f64()
                        .unwrap_or(0.)
                        .total_cmp(&a["exptime_s"].as_f64().unwrap_or(0.))
                })
                .then_with(|| text(a, "obs_id").cmp(text(b, "obs_id")))
        });
        members.dedup_by(|a, b| a["obs_id"] == b["obs_id"]);
        let first = &members[0];
        let filename = text(first, "data_uri").rsplit('/').next().unwrap_or("");
        let (field, family) =
            field_family(filename, text(first, "collection"), text(first, "obs_id"));
        let mut filters = Vec::new();
        let mut targets = Vec::new();
        for row in &members {
            if !filters.contains(&row["filter"]) {
                filters.push(row["filter"].clone());
            }
            if !text(row, "target").is_empty()
                && targets.len() < 3
                && !targets.contains(&row["target"])
            {
                targets.push(row["target"].clone());
            }
        }
        groups.push(json!({"id":format!("G-{}",&hash(&key)[..16]),"key":key,
            "collection":first["collection"],"proposal":first["proposal"],"instrument":first["instrument"],
            "field":field,"family":family,"targets":targets,"title":first["title"],
            "date":members.iter().map(|r| text(r,"date")).max().unwrap_or(""),
            "filters":filters,"observations":members}));
    }
    groups.sort_by(|a, b| {
        b["filters"]
            .as_array()
            .unwrap()
            .len()
            .cmp(&a["filters"].as_array().unwrap().len())
            .then_with(|| (a["collection"] != "JWST").cmp(&(b["collection"] != "JWST")))
            .then_with(|| text(a, "date").cmp(text(b, "date")))
            .then_with(|| text(a, "key").cmp(text(b, "key")))
    });
    groups
}

fn describe(name: &str, wavelength: Option<f64>) -> Value {
    let key = name.split('+').next().unwrap_or("").to_ascii_uppercase();
    let line = match key.as_str() {
        "F373N" => Some(("[O II]", "Singly ionized oxygen")),
        "F395N" => Some(("Ca II", "Ionized calcium")),
        "F469N" => Some(("He II", "Ionized helium")),
        "F487N" => Some(("H-beta", "Hydrogen (H-beta)")),
        "F501N" | "F502N" => Some(("[O III]", "Doubly ionized oxygen")),
        "F631N" => Some(("[O I]", "Neutral oxygen")),
        "F645N" => Some(("Continuum", "Red continuum (no lines)")),
        "F656N" => Some(("H-alpha", "Hydrogen (H-alpha)")),
        "F657N" => Some(("H-alpha + [N II]", "Hydrogen + nitrogen")),
        "F658N" | "F660N" => Some(("[N II]", "Ionized nitrogen")),
        "F665N" => Some(("H-alpha (redshifted)", "Hydrogen, receding gas")),
        "F673N" => Some(("[S II]", "Ionized sulfur")),
        "F680N" => Some(("[S II] + continuum", "Ionized sulfur")),
        "F953N" => Some(("[S III]", "Doubly ionized sulfur")),
        "F126N" | "F164N" => Some(("[Fe II]", "Ionized iron (shocks)")),
        "F128N" => Some(("Paschen-beta", "Hydrogen (Paschen-beta)")),
        "F130N" | "F167N" | "F190N" | "F215N" => Some(("Continuum", "Near-IR continuum")),
        "F187N" => Some(("Paschen-alpha", "Hydrogen (Paschen-alpha)")),
        "F212N" => Some(("H2", "Molecular hydrogen (warm gas)")),
        "F323N" => Some(("H2", "Molecular hydrogen")),
        "F335M" => Some(("PAH 3.3 um", "Hydrocarbon dust (PAHs)")),
        "F405N" => Some(("Brackett-alpha", "Hydrogen (Brackett-alpha)")),
        "F466N" => Some(("CO", "Carbon monoxide")),
        "F470N" => Some(("H2", "Molecular hydrogen")),
        "F770W" => Some(("PAH 7.7 um", "Hydrocarbon dust (PAHs)")),
        "F1130W" => Some(("PAH 11.3 um", "Hydrocarbon dust (PAHs)")),
        _ => None,
    };
    let band = wavelength
        .map(|nm| {
            [
                (320., "Ultraviolet"),
                (450., "Visible blue"),
                (500., "Visible blue-green"),
                (570., "Visible green"),
                (620., "Visible yellow-orange"),
                (700., "Visible red"),
                (1000., "Far red / near-IR"),
                (2500., "Near-infrared"),
                (5500., "Infrared"),
            ]
            .iter()
            .find(|(limit, _)| nm < *limit)
            .map(|(_, label)| *label)
            .unwrap_or("Mid-infrared")
        })
        .unwrap_or("Unknown band");
    let suffix = key
        .strip_prefix('F')
        .unwrap_or("")
        .trim_start_matches(|c: char| c.is_ascii_digit());
    let kind = match suffix {
        "N" => "Narrowband",
        "M" => "Medium band",
        "W" | "W2" => "Wideband",
        "LP" => "Longpass",
        _ => "Filter",
    };
    json!({"line":line.map(|p| p.0).unwrap_or(""),
        "shows":line.map(|p| p.1.to_string()).unwrap_or_else(|| format!("{band} light")),"kind":kind,"band":band})
}

fn validate_position(position: [f64; 2]) -> Result<()> {
    ensure!(
        position.iter().all(|n| n.is_finite())
            && (0. ..360.).contains(&position[0])
            && (-90. ..=90.).contains(&position[1]),
        "Coordinates must have RA 0–360° and Dec −90–90°."
    );
    Ok(())
}

fn parse_position(query: &str) -> Result<Option<[f64; 2]>> {
    ensure!(!query.trim().is_empty(), "Enter a target or coordinates.");
    let query = query.replace(',', " ");
    let parts: Vec<_> = query.split_whitespace().collect();
    if parts.len() == 2 {
        if let (Ok(ra), Ok(dec)) = (parts[0].parse::<f64>(), parts[1].parse::<f64>()) {
            validate_position([ra, dec])?;
            return Ok(Some([ra, dec]));
        }
        if query.contains(':')
            || parts.iter().any(|s| {
                s.as_bytes()
                    .windows(2)
                    .any(|p| p[0].is_ascii_digit() && b"hHdD".contains(&p[1]))
            })
        {
            ensure!(
                !parts[1].contains(['h', 'H']),
                "Declination must be in degrees."
            );
            let ra = (sexagesimal(parts[0])?
                * if parts[0].contains(['d', 'D']) {
                    1.
                } else {
                    15.
                })
            .rem_euclid(360.);
            let dec = sexagesimal(parts[1])?;
            validate_position([ra, dec])?;
            return Ok(Some([ra, dec]));
        }
    }
    if parts.len() == 6 && parts.iter().all(|s| s.parse::<f64>().is_ok()) {
        let ra = (sexagesimal(&parts[..3].join(":"))? * 15.).rem_euclid(360.);
        let dec = sexagesimal(&parts[3..].join(":"))?;
        validate_position([ra, dec])?;
        return Ok(Some([ra, dec]));
    }
    Ok(None)
}

fn sexagesimal(value: &str) -> Result<f64> {
    let normalized: String = value
        .chars()
        .map(|c| if "hHdDmMsS".contains(c) { ':' } else { c })
        .collect();
    let parts: Vec<_> = normalized.trim_end_matches(':').split(':').collect();
    ensure!(
        (1..=3).contains(&parts.len()),
        "Invalid sexagesimal coordinates."
    );
    let first: f64 = parts[0]
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid sexagesimal coordinates."))?;
    let minutes: f64 = if parts.len() >= 2 {
        parts[1]
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid sexagesimal coordinates."))?
    } else {
        0.
    };
    let seconds: f64 = if parts.len() == 3 {
        parts[2]
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid sexagesimal coordinates."))?
    } else {
        0.
    };
    ensure!(
        first.is_finite() && (0. ..60.).contains(&minutes) && (0. ..60.).contains(&seconds),
        "Invalid sexagesimal coordinates."
    );
    Ok((first.abs() + minutes / 60. + seconds / 3600.)
        * if value.starts_with('-') { -1. } else { 1. })
}

fn parse_resolver(bytes: &[u8]) -> Option<[f64; 2]> {
    let body = std::str::from_utf8(bytes).ok()?;
    let doc = roxmltree::Document::parse(body).ok()?;
    let scalar = |name| {
        doc.descendants()
            .find(|n| n.has_tag_name(name))?
            .text()?
            .trim()
            .parse::<f64>()
            .ok()
    };
    if let (Some(ra), Some(dec)) = (scalar("jradeg"), scalar("jdedeg")) {
        return validate_position([ra, dec]).is_ok().then_some([ra, dec]);
    }
    let fields: Vec<_> = doc
        .descendants()
        .filter(|n| n.has_tag_name("FIELD"))
        .collect();
    let column = |ra: bool| {
        fields.iter().position(|f| {
            let name = f.attribute("name").unwrap_or("").to_ascii_lowercase();
            let ucd = f.attribute("ucd").unwrap_or("").to_ascii_lowercase();
            if ra {
                name == "ra_d" || name == "ra" || ucd.contains("pos.eq.ra")
            } else {
                name == "dec_d" || name == "dec" || ucd.contains("pos.eq.dec")
            }
        })
    };
    let row = doc.descendants().find(|n| n.has_tag_name("TR"))?;
    let cells: Vec<_> = row.children().filter(|n| n.has_tag_name("TD")).collect();
    let ra = cells
        .get(column(true)?)?
        .text()?
        .trim()
        .parse::<f64>()
        .ok()?;
    let dec = cells
        .get(column(false)?)?
        .text()?
        .trim()
        .parse::<f64>()
        .ok()?;
    validate_position([ra, dec]).is_ok().then_some([ra, dec])
}

fn separation(a: [f64; 2], b: [f64; 2]) -> f64 {
    let (a0, a1, b0, b1) = (
        a[0].to_radians(),
        a[1].to_radians(),
        b[0].to_radians(),
        b[1].to_radians(),
    );
    (2. * (((a1 - b1) / 2.).sin().powi(2) + a1.cos() * b1.cos() * ((a0 - b0) / 2.).sin().powi(2))
        .clamp(0., 1.)
        .sqrt()
        .asin())
    .to_degrees()
}

fn search_radius(view: &Value) -> Result<f64> {
    let center = pair(&view["center_icrs_deg"])?;
    let radius = view["corners_icrs_deg"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| pair(p).ok())
        .map(|p| separation(center, p) * 60. * 1.02)
        .fold(0., f64::max);
    Ok(if radius > 0. {
        radius.clamp(1., 180.)
    } else {
        let f = field(view)?;
        (f[0].hypot(f[1]) / 2. * 1.02).clamp(1., 180.)
    })
}

fn polygons(region: &str) -> Vec<Vec<[f64; 2]>> {
    let mut result = Vec::new();
    let upper = region.to_ascii_uppercase().replace(['(', ')', ','], " ");
    for chunk in upper.split("POLYGON").skip(1) {
        let numbers: Vec<_> = chunk
            .split_whitespace()
            .skip_while(|s| s.parse::<f64>().is_err())
            .map_while(|s| s.parse::<f64>().ok())
            .collect();
        if numbers.len() >= 6 && numbers.len() % 2 == 0 && numbers.iter().all(|n| n.is_finite()) {
            result.push(numbers.as_chunks::<2>().0.to_vec());
        }
    }
    result
}

/// Reproject an existing observation region onto a snapshot without archive I/O.
/// Returns the catalog's `Observation.pixels` format: finite polygons in top-down
/// image coordinates, rounded to one decimal place by the shared projection.
pub fn projected_pixels(region: &str, header: &Value, width: u32, height: u32) -> Result<Value> {
    let wcs = SkyWcs::new(header, width, height)?;
    Ok(json!(project(region, &wcs, height)))
}

fn project(region: &str, wcs: &SkyWcs, height: u32) -> Vec<Vec<[f64; 2]>> {
    polygons(region)
        .into_iter()
        .filter_map(|polygon| {
            polygon
                .into_iter()
                .map(|p| {
                    let [x, y] = wcs.sky_to_pixel(p)?;
                    (x.is_finite() && y.is_finite()).then_some([
                        (x * 10.).round() / 10.,
                        ((height as f64 - 1. - y) * 10.).round() / 10.,
                    ])
                })
                .collect::<Option<Vec<_>>>()
        })
        .collect()
}

fn contains(polygon: &[[f64; 2]], p: [f64; 2]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let mut inside = false;
    for i in 0..polygon.len() {
        let a = polygon[i];
        let b = polygon[(i + 1) % polygon.len()];
        let cross = (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
        if cross.abs() < 1e-10
            && p[0] >= a[0].min(b[0])
            && p[0] <= a[0].max(b[0])
            && p[1] >= a[1].min(b[1])
            && p[1] <= a[1].max(b[1])
        {
            return true;
        }
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
        {
            inside = !inside;
        }
    }
    inside
}

fn intersects_rect(polygon: &[[f64; 2]], width: u32, height: u32) -> bool {
    let rect = [
        [-0.5, -0.5],
        [width as f64 - 0.5, -0.5],
        [width as f64 - 0.5, height as f64 - 0.5],
        [-0.5, height as f64 - 0.5],
    ];
    if polygon.iter().any(|p| {
        p[0] >= -0.5 && p[0] <= width as f64 - 0.5 && p[1] >= -0.5 && p[1] <= height as f64 - 0.5
    }) || rect.iter().any(|p| contains(polygon, *p))
    {
        return true;
    }
    let cross = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| {
        (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
    };
    for i in 0..polygon.len() {
        for j in 0..4 {
            let (a, b, c, d) = (
                polygon[i],
                polygon[(i + 1) % polygon.len()],
                rect[j],
                rect[(j + 1) % 4],
            );
            if cross(a, b, c) * cross(a, b, d) < 0. && cross(c, d, a) * cross(c, d, b) < 0. {
                return true;
            }
        }
    }
    false
}

fn tangent(center: [f64; 2], point: [f64; 2]) -> Option<[f64; 2]> {
    let dec = point[1].to_radians();
    let dec0 = center[1].to_radians();
    let delta = (point[0] - center[0]).to_radians();
    let divisor = dec0.sin() * dec.sin() + dec0.cos() * dec.cos() * delta.cos();
    (divisor > 0.).then(|| {
        [
            -dec.cos() * delta.sin() / divisor,
            (dec0.cos() * dec.sin() - dec0.sin() * dec.cos() * delta.cos()) / divisor,
        ]
    })
}

fn suggest_frame(center: [f64; 2], rows: &[Value], aspect: &str) -> Result<Value> {
    let ratio: f64 = match aspect {
        "16:9" => 16. / 9.,
        "3:2" => 1.5,
        "1:1" => 1.,
        "9:16" => 9. / 16.,
        _ => bail!("Unknown aspect ratio."),
    };
    let mut extent = [0f64; 2];
    let mut covering = 0;
    for row in rows {
        for polygon in polygons(text(row, "s_region")) {
            if let Some(points) = polygon
                .into_iter()
                .map(|p| tangent(center, p))
                .collect::<Option<Vec<_>>>()
                && contains(&points, [0., 0.])
            {
                covering += 1;
                for p in points {
                    for axis in 0..2 {
                        extent[axis] =
                            extent[axis].max(p[axis].abs().to_degrees() * 60. * 2. * 1.1);
                    }
                }
            }
        }
    }
    let width = if covering > 0 {
        extent[0].max(extent[1] * ratio)
    } else {
        10.
    };
    let width = width.clamp(1f64.max(ratio), 30. * 1f64.min(ratio));
    Ok(
        json!({"width_arcmin":(width*1000.).round()/1000.,"height_arcmin":(width/ratio*1000.).round()/1000.,"aspect":aspect,"covering":covering}),
    )
}
