use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;
use uuid::Uuid;

#[derive(Clone, Serialize)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub session: String,
    pub name: String,
    pub state: String,
    pub stage: String,
    pub phase: String,
    pub progress: f64,
    pub message: String,
    pub log: Vec<String>,
    pub error: Option<String>,
    pub result: Option<Value>,
    pub cancel_requested: bool,
    pub eta_s: Option<f64>,
    pub files: Vec<Value>,
    pub steps: Vec<Value>,
    pub waiting_for: Option<Value>,
    #[serde(skip)]
    eta_at: Option<Instant>,
}

impl Job {
    fn new(kind: &str, session: &str, name: &str, queued: bool) -> Self {
        let (stage, message) = initial(kind);
        Self {
            id: Uuid::new_v4().simple().to_string(),
            kind: kind.into(),
            session: session.into(),
            name: name.into(),
            state: if queued { "queued" } else { "running" }.into(),
            stage: if queued { "queued" } else { stage }.into(),
            phase: if queued { "queued" } else { stage }.into(),
            progress: 0.,
            message: if queued {
                if kind == "export-full" {
                    "Queued for export"
                } else {
                    "Queued for build"
                }
            } else {
                message
            }
            .into(),
            log: vec![],
            error: None,
            result: None,
            cancel_requested: false,
            eta_s: None,
            files: vec![],
            steps: vec![],
            waiting_for: None,
            eta_at: None,
        }
    }

    fn snapshot(&self, now: Instant) -> Self {
        let mut job = self.clone();
        if self.state == "running"
            && let Some(at) = self.eta_at
        {
            let elapsed = now.duration_since(at).as_secs_f64();
            let remaining = |eta: f64| (eta > elapsed).then_some(eta - elapsed);
            job.eta_s = job.eta_s.and_then(remaining);
            for step in &mut job.steps {
                if step["state"] == "running"
                    && let Some(eta) = step["eta_s"].as_f64()
                {
                    step["eta_s"] = serde_json::json!(remaining(eta));
                }
            }
        }
        job
    }

    fn mark_cancelled(&mut self) {
        if self.state == "cancelled" {
            return;
        }
        self.state = "cancelled".into();
        self.stage = "cancelled".into();
        self.phase = "cancelled".into();
        self.message = "Cancelled".into();
        self.log.push("Cancelled".into());
    }

    fn download_progress(&mut self) {
        if self.kind != "build" || self.phase != "download" || self.files.is_empty() {
            return;
        }
        let complete = |file: &Value| matches!(file["state"].as_str(), Some("cached" | "done"));
        let total = |file: &Value| {
            if file["cached"] == true {
                Some(0)
            } else {
                file.get("transfer_bytes")
                    .unwrap_or(&file["size_bytes"])
                    .as_u64()
            }
        };
        let transferred = |file: &Value| file["transferred_bytes"].as_u64().unwrap_or(0);
        let totals: Option<Vec<_>> = self.files.iter().map(total).collect();
        self.progress = if self.files.iter().all(complete) {
            1.
        } else if let Some(total) = totals
            .map(|sizes| sizes.iter().sum::<u64>())
            .filter(|n| *n > 0)
        {
            (self
                .files
                .iter()
                .filter(|file| file["cached"] != true)
                .map(transferred)
                .sum::<u64>() as f64
                / total as f64)
                .min(0.99)
        } else {
            self.files
                .iter()
                .map(|file| {
                    if complete(file) {
                        1.
                    } else if let Some(total) = total(file).filter(|n| *n > 0) {
                        (transferred(file) as f64 / total as f64).min(0.99)
                    } else {
                        file["progress"].as_f64().unwrap_or(0.).clamp(0., 0.99)
                    }
                })
                .sum::<f64>()
                / self.files.len() as f64
        };
    }
}

#[derive(Clone, Default)]
pub struct Jobs {
    queue: Arc<Mutex<Queue>>,
    finished: Arc<Condvar>,
    stopping: Arc<AtomicBool>,
}

type Work = Box<dyn FnOnce(JobHandle) -> Result<()> + Send>;

#[derive(Default)]
struct Queue {
    jobs: Vec<Arc<Mutex<Job>>>,
    pending: VecDeque<(JobHandle, Work)>,
    running: bool,
    light_running: usize,
}

#[derive(Clone)]
pub struct JobHandle(Arc<Mutex<Job>>, Option<String>, Vec<Arc<AtomicBool>>);

impl JobHandle {
    /// Read-only media requests report progress without creating a project job.
    pub fn untracked() -> Self {
        Self(
            Arc::new(Mutex::new(Job::new("preview", "", "", false))),
            None,
            vec![],
        )
    }

    pub fn cancelled(&self) -> bool {
        self.0.lock().unwrap().cancel_requested
            || self.2.iter().any(|stop| stop.load(Ordering::Acquire))
    }

    pub(crate) fn fork(&self) -> Self {
        let mut group = self.clone();
        group.2.push(Arc::new(AtomicBool::new(false)));
        group
    }

    pub(crate) fn stop(&self) {
        self.2
            .last()
            .expect("Only a forked work group can be stopped")
            .store(true, Ordering::Release);
    }

    fn check(&self, job: &Job) -> Result<()> {
        if job.cancel_requested || self.2.iter().any(|stop| stop.load(Ordering::Acquire)) {
            bail!("Cancelled");
        }
        Ok(())
    }

    fn cancel(&self) -> Job {
        let mut job = self.0.lock().unwrap();
        if matches!(job.state.as_str(), "running" | "queued") {
            job.cancel_requested = true;
            if job.state == "queued" {
                job.mark_cancelled();
            }
        }
        job.clone()
    }

    pub fn stage(&self, stage: &str, progress: f64, message: &str) -> Result<()> {
        let mut job = self.0.lock().unwrap();
        self.check(&job)?;
        if job.phase != stage {
            job.eta_s = None;
            job.eta_at = None;
        }
        if stage == "download"
            && let Some(identifier) = &self.1
            && let Some(file) = job
                .files
                .iter_mut()
                .find(|file| file["obs_id"] == *identifier)
        {
            file["progress"] = serde_json::json!(progress.clamp(0., 1.));
        }
        job.stage = stage.into();
        job.phase = stage.into();
        job.progress = progress.clamp(0., 1.);
        job.download_progress();
        job.message = message.into();
        if job.log.last().is_none_or(|last| last != message) {
            job.log.push(message.into());
        }
        Ok(())
    }

    pub fn files(&self, files: Vec<Value>) -> Result<()> {
        let mut job = self.0.lock().unwrap();
        self.check(&job)?;
        job.files = files;
        job.download_progress();
        Ok(())
    }

    pub(crate) fn note(&self, message: &str) -> Result<()> {
        let mut job = self.0.lock().unwrap();
        self.check(&job)?;
        job.log.push(message.into());
        Ok(())
    }

    pub(crate) fn alignment(
        &self,
        phase: &str,
        progress: f64,
        message: &str,
        steps: Vec<Value>,
        eta_s: Option<f64>,
    ) -> Result<()> {
        let mut job = self.0.lock().unwrap();
        self.check(&job)?;
        job.stage = phase.into();
        job.phase = phase.into();
        job.progress = progress;
        job.steps = steps;
        job.eta_s = eta_s;
        job.eta_at = Some(Instant::now());
        job.message = message.into();
        if job.log.last().is_none_or(|last| last != message) {
            job.log.push(message.into());
        }
        Ok(())
    }

    pub(crate) fn update_file(
        &self,
        identifier: &str,
        update: impl FnOnce(&mut Value),
    ) -> Result<()> {
        let mut job = self.0.lock().unwrap();
        self.check(&job)?;
        let file = job
            .files
            .iter_mut()
            .find(|file| file["obs_id"] == identifier)
            .context("Missing planned download file")?;
        update(file);
        job.download_progress();
        Ok(())
    }

    pub(crate) fn assembling(&self, progress: f64, message: &str) -> Result<()> {
        let tracked = self
            .0
            .lock()
            .unwrap()
            .files
            .iter()
            .any(|file| Some(file["obs_id"].as_str().unwrap_or("")) == self.1.as_deref());
        if tracked && let Some(identifier) = &self.1 {
            self.update_file(identifier, |file| {
                file["state"] = serde_json::json!("assembling")
            })?;
        }
        self.stage("download", progress, message)
    }

    pub fn transfer(&self, identifier: &str, done: u64, total: Option<u64>) -> Result<()> {
        let mut job = self.0.lock().unwrap();
        self.check(&job)?;
        if let Some(file) = job
            .files
            .iter_mut()
            .find(|file| file["obs_id"] == identifier)
        {
            file["done_bytes"] = serde_json::json!(done);
            file["transferred_bytes"] = serde_json::json!(done);
            if let Some(total) = total {
                file["size_bytes"] = serde_json::json!(total);
                file["transfer_bytes"] = serde_json::json!(total);
                file["transfer_estimated"] = serde_json::json!(false);
            }
        }
        job.download_progress();
        Ok(())
    }

    pub(crate) fn for_file(&self, identifier: &str) -> Self {
        Self(self.0.clone(), Some(identifier.into()), self.2.clone())
    }

    pub(crate) fn received(&self, bytes: u64) -> Result<()> {
        let Some(identifier) = &self.1 else {
            return Ok(());
        };
        let mut job = self.0.lock().unwrap();
        self.check(&job)?;
        if let Some(file) = job
            .files
            .iter_mut()
            .find(|file| file["obs_id"] == *identifier)
        {
            let done = file["transferred_bytes"]
                .as_u64()
                .unwrap_or(0)
                .saturating_add(bytes);
            file["transferred_bytes"] = serde_json::json!(done);
        }
        job.download_progress();
        Ok(())
    }

    pub(crate) fn transferred(&self, identifier: &str) -> u64 {
        self.0
            .lock()
            .unwrap()
            .files
            .iter()
            .find(|file| file["obs_id"] == identifier)
            .and_then(|file| file["transferred_bytes"].as_u64())
            .unwrap_or(0)
    }

    pub fn name(&self, name: &str) {
        self.0.lock().unwrap().name = name.into();
    }

    /// Atomically arbitrate cancellation against publishing the final file.
    pub fn publish(&self, publish: impl FnOnce() -> Result<Value>) -> Result<()> {
        let mut job = self.0.lock().unwrap();
        if job.cancel_requested {
            bail!("Cancelled");
        }
        // A publication panic is still a worker failure, but must not poison
        // the job mutex and prevent cancellation/status/shutdown from finishing.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(publish));
        let result = match outcome {
            Ok(result) => result?,
            Err(panic) => {
                drop(job);
                std::panic::resume_unwind(panic);
            }
        };
        job.result = Some(result);
        job.progress = 1.;
        job.state = "done".into();
        job.stage = "done".into();
        job.phase = "done".into();
        job.message = match job.kind.as_str() {
            "export-full" => "Export complete",
            "build" => "Build complete",
            "capture" => "Capture complete",
            _ => "Search complete",
        }
        .into();
        job.eta_s = Some(0.);
        job.eta_at = None;
        Ok(())
    }
}

impl Jobs {
    pub fn all(&self) -> Vec<Job> {
        let handles = self.queue.lock().unwrap().jobs.clone();
        handles
            .iter()
            .map(|j| j.lock().unwrap().snapshot(Instant::now()))
            .collect()
    }
    pub fn busy(&self, session: &str) -> bool {
        self.all()
            .iter()
            .any(|j| j.session == session && matches!(j.state.as_str(), "running" | "queued"))
    }
    pub fn latest(&self, session: Option<&str>) -> Option<Job> {
        self.all()
            .into_iter()
            .rev()
            .find(|j| session.is_none_or(|s| j.session == s))
    }
    pub fn cancel(&self, id: Option<&str>, session: Option<&str>) -> Option<Job> {
        let handles = self.queue.lock().unwrap().jobs.clone();
        let found = handles.iter().rev().find(|j| {
            let job = j.lock().unwrap();
            if let Some(id) = id {
                job.id == id
            } else {
                session.is_none_or(|s| job.session == s)
            }
        })?;
        Some(JobHandle(found.clone(), None, vec![]).cancel())
    }

    pub fn cancel_and_wait(&self) {
        let mut queue = self.queue.lock().unwrap();
        self.stopping.store(true, Ordering::Release);
        for handle in &queue.jobs {
            JobHandle(handle.clone(), None, vec![]).cancel();
        }
        // Publication may mark a job done before its worker finishes cleanup.
        // Shutdown waits for the drain worker, not just terminal job statuses.
        while queue.running || queue.light_running > 0 {
            queue = self.finished.wait(queue).unwrap();
        }
    }
    /// Caller serializes start with project writes using the application lock.
    pub fn start(
        &self,
        session: &str,
        name: &str,
        work: impl FnOnce(JobHandle) -> Result<()> + Send + 'static,
    ) -> Result<Job> {
        self.start_kind("export-full", true, session, name, work)
    }

    pub fn start_kind(
        &self,
        kind: &str,
        heavy: bool,
        session: &str,
        name: &str,
        work: impl FnOnce(JobHandle) -> Result<()> + Send + 'static,
    ) -> Result<Job> {
        let mut queue = self.queue.lock().unwrap();
        if self.stopping.load(Ordering::Acquire) {
            bail!("NightBloom is shutting down.");
        }
        if queue.jobs.iter().any(|handle| {
            let job = handle.lock().unwrap();
            job.session == session && matches!(job.state.as_str(), "running" | "queued")
        }) {
            bail!("This project has an active job. Wait or cancel it first.");
        }
        let queued = heavy && queue.running;
        let job = Job::new(kind, session, name, queued);
        let handle = JobHandle(Arc::new(Mutex::new(job.clone())), None, vec![]);
        if queue.jobs.len() >= 40
            && let Some(index) = queue
                .jobs
                .iter()
                .position(|j| !matches!(j.lock().unwrap().state.as_str(), "running" | "queued"))
        {
            queue.jobs.remove(index);
        }
        queue.jobs.push(handle.0.clone());
        if heavy {
            queue.pending.push_back((handle, Box::new(work)));
            queue.running = true;
            drop(queue);
            if !queued {
                let jobs = self.clone();
                tokio::task::spawn_blocking(move || jobs.run());
            }
        } else {
            queue.light_running += 1;
            drop(queue);
            let jobs = self.clone();
            tokio::task::spawn_blocking(move || {
                Self::run_work(handle, Box::new(work));
                let mut queue = jobs.queue.lock().unwrap();
                queue.light_running -= 1;
                jobs.finished.notify_all();
            });
        }
        Ok(job)
    }

    fn run(&self) {
        loop {
            let next = {
                let mut queue = self.queue.lock().unwrap();
                match queue.pending.pop_front() {
                    Some(next) => next,
                    None => {
                        queue.running = false;
                        self.finished.notify_all();
                        return;
                    }
                }
            };
            let (handle, work) = next;
            {
                let mut job = handle.0.lock().unwrap();
                if job.cancel_requested {
                    job.mark_cancelled();
                    // Dropping the queued callback is also outside all locks.
                    continue;
                }
                job.state = "running".into();
                let (stage, message) = initial(&job.kind);
                job.stage = stage.into();
                job.phase = stage.into();
                job.message = message.into();
            }
            Self::run_work(handle, work);
        }
    }

    fn run_work(handle: JobHandle, work: Work) {
        if handle.cancelled() {
            handle.0.lock().unwrap().mark_cancelled();
            return;
        }
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(handle.clone())));
        let mut job = handle.0.lock().unwrap();
        if job.state == "done" {
            // Publication has already completed atomically.
        } else if job.cancel_requested {
            job.mark_cancelled();
        } else {
            let label = if job.kind == "export-full" {
                "Export"
            } else {
                "Job"
            };
            let error = match outcome {
                Ok(Err(e)) => format!("{e:#}"),
                Ok(Ok(())) => format!("{label} ended without publishing a result"),
                Err(_) => format!("{label} worker panicked"),
            };
            eprintln!("{} job {} failed: {}", job.kind, job.id, error);
            job.state = "error".into();
            job.phase = "error".into();
            job.error = Some(error.clone());
            job.message = error.clone();
            job.log.push(error);
        }
    }
}

fn initial(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "export-full" => ("render", "Starting export"),
        "build" => ("download", "Starting build"),
        "capture" => ("snapshot", "Reading captured view"),
        _ => ("search", "Searching observations"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(5);

    #[test]
    fn alignment_eta_counts_down_without_expiring_pending_steps_or_claiming_completion() {
        let mut job = Job::new("build", "one", "One", false);
        let now = Instant::now();
        job.eta_at = Some(now);
        job.eta_s = Some(10.);
        job.steps = vec![
            serde_json::json!({"state":"running","eta_s":6.}),
            serde_json::json!({"state":"pending","eta_s":8.}),
        ];
        let snapshot = job.snapshot(now + Duration::from_secs(3));
        assert_eq!(snapshot.eta_s, Some(7.));
        assert_eq!(snapshot.steps[0]["eta_s"], 3.);
        assert_eq!(snapshot.steps[1]["eta_s"], 8.);
        let overdue = job.snapshot(now + Duration::from_secs(11));
        assert_eq!(overdue.eta_s, None);
        assert!(overdue.steps[0]["eta_s"].is_null());
        assert_eq!(overdue.state, "running");
    }

    #[test]
    fn download_progress_tracks_payload_and_survives_provider_stage_messages() {
        let job = Arc::new(Mutex::new(Job::new("build", "one", "One", false)));
        let handle = JobHandle(job.clone(), None, vec![]);
        handle
            .files(vec![
                serde_json::json!({"obs_id":"survey","state":"downloading","transfer_bytes":100}),
                serde_json::json!({"obs_id":"cached","state":"cached","size_bytes":1000,"cached":true}),
            ])
            .unwrap();
        let source = handle.for_file("survey");
        source.received(90).unwrap();
        assert_eq!(job.lock().unwrap().progress, 0.9);
        source.stage("download", 0., "Fetching next field").unwrap();
        assert_eq!(job.lock().unwrap().progress, 0.9);
        source.received(20).unwrap();
        assert_eq!(job.lock().unwrap().progress, 0.99);
        handle.stage("align", 0., "Aligning").unwrap();
        assert_eq!(job.lock().unwrap().progress, 0.);
        handle.stage("download", 0., "Preparing sources").unwrap();
        handle
            .files(vec![
                serde_json::json!({"obs_id":"survey","state":"assembling","transfer_bytes":0}),
            ])
            .unwrap();
        source
            .stage("download", 0.5, "Assembling cached fields")
            .unwrap();
        assert_eq!(job.lock().unwrap().progress, 0.5);
        handle
            .files(vec![
                serde_json::json!({"obs_id":"finished","state":"done","transfer_bytes":100}),
                serde_json::json!({"obs_id":"unknown","state":"to download","transfer_bytes":null}),
            ])
            .unwrap();
        assert_eq!(job.lock().unwrap().progress, 0.5);
    }

    #[tokio::test]
    async fn capture_bypasses_heavy_work_and_shutdown_waits_for_light_cleanup() {
        let jobs = Jobs::default();
        let (release_heavy, heavy_ready) = mpsc::channel();
        jobs.start("heavy", "Export", move |handle| {
            heavy_ready.recv_timeout(WAIT).unwrap();
            handle.publish(|| Ok(serde_json::json!({})))
        })
        .unwrap();
        let (published, publication) = mpsc::channel();
        let (release_light, light_ready) = mpsc::channel();
        let capture = jobs
            .start_kind("capture", false, "light", "Capture", move |handle| {
                handle.publish(|| Ok(serde_json::json!({"session":"light"})))?;
                published.send(()).unwrap();
                light_ready.recv_timeout(WAIT).unwrap();
                Ok(())
            })
            .unwrap();
        assert_eq!(capture.kind, "capture");
        assert_eq!(capture.state, "running");
        publication.recv_timeout(WAIT).unwrap();
        release_heavy.send(()).unwrap();
        let (stopped, shutdown) = mpsc::channel();
        std::thread::spawn(move || {
            jobs.cancel_and_wait();
            stopped.send(()).unwrap();
        });
        assert!(shutdown.recv_timeout(Duration::from_millis(100)).is_err());
        release_light.send(()).unwrap();
        shutdown.recv_timeout(WAIT).unwrap();
    }

    #[tokio::test]
    async fn exports_are_global_fifo_and_same_project_remains_busy() {
        // Design decision: all projects share the promised heavy queue; native
        // preparation budgets must not multiply with simultaneously open projects.
        let jobs = Jobs::default();
        let (entered, ready) = mpsc::channel();
        let (completed, done) = mpsc::channel();
        let mut releases = Vec::new();
        for session in ["one", "two", "three"] {
            let (release, proceed) = mpsc::channel();
            releases.push(release);
            let entered = entered.clone();
            let completed = completed.clone();
            let worker_jobs = jobs.clone();
            let job = jobs
                .start(session, session, move |handle| {
                    assert_eq!(worker_jobs.latest(Some(session)).unwrap().state, "running");
                    handle.stage("render", 0.5, "Rendering")?;
                    assert!(!handle.cancelled());
                    entered.send(session).unwrap();
                    proceed.recv_timeout(WAIT).unwrap();
                    handle.publish(|| Ok(serde_json::json!({"session": session})))?;
                    completed.send(()).unwrap();
                    Ok(())
                })
                .unwrap();
            if session == "one" {
                assert_eq!(ready.recv_timeout(WAIT).unwrap(), "one");
                assert_eq!(job.state, "running");
            } else {
                assert_eq!(job.state, "queued");
                assert_eq!(job.stage, "queued");
                assert_eq!(job.phase, "queued");
            }
            assert!(jobs.busy(session));
            assert!(jobs.start(session, session, |_| Ok(())).is_err());
        }
        assert!(ready.recv_timeout(Duration::from_millis(100)).is_err());
        releases[0].send(()).unwrap();
        assert_eq!(ready.recv_timeout(WAIT).unwrap(), "two");
        assert!(ready.recv_timeout(Duration::from_millis(100)).is_err());
        releases[1].send(()).unwrap();
        assert_eq!(ready.recv_timeout(WAIT).unwrap(), "three");
        releases[2].send(()).unwrap();
        for _ in 0..3 {
            done.recv_timeout(WAIT).unwrap();
        }
        jobs.cancel_and_wait();
        assert!(jobs.all().iter().all(|job| job.state == "done"));
    }

    #[tokio::test]
    async fn cancelled_queued_export_never_runs_and_does_not_block_next_project() {
        let jobs = Jobs::default();
        let (entered, ready) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        jobs.start("one", "One", move |handle| {
            entered.send(()).unwrap();
            proceed.recv_timeout(WAIT).unwrap();
            handle.publish(|| Ok(serde_json::json!({})))
        })
        .unwrap();
        ready.recv_timeout(WAIT).unwrap();
        let called = Arc::new(AtomicBool::new(false));
        let worker_called = called.clone();
        let queued = jobs
            .start("two", "Two", move |_| {
                worker_called.store(true, Ordering::Release);
                Ok(())
            })
            .unwrap();
        let cancelled = jobs.cancel(Some(&queued.id), None).unwrap();
        assert_eq!(cancelled.state, "cancelled");
        assert!(!jobs.busy("two"));
        let (entered, ready) = mpsc::channel();
        jobs.start("three", "Three", move |handle| {
            entered.send(()).unwrap();
            handle.publish(|| Ok(serde_json::json!({})))
        })
        .unwrap();
        release.send(()).unwrap();
        ready.recv_timeout(WAIT).unwrap();
        jobs.cancel_and_wait();
        assert!(!called.load(Ordering::Acquire));
        assert!(jobs.latest(Some("two")).unwrap().result.is_none());
    }

    #[tokio::test]
    async fn errors_and_panics_release_the_fifo_slot() {
        let jobs = Jobs::default();
        let (entered, ready) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        jobs.start("one", "One", move |_| {
            entered.send(()).unwrap();
            proceed.recv_timeout(WAIT).unwrap();
            bail!("synthetic failure")
        })
        .unwrap();
        ready.recv_timeout(WAIT).unwrap();
        jobs.start("two", "Two", |_| panic!("synthetic panic"))
            .unwrap();
        jobs.start("three", "Three", |handle| {
            handle.publish(|| panic!("synthetic publication panic"))
        })
        .unwrap();
        let (entered, ready) = mpsc::channel();
        jobs.start("four", "Four", move |handle| {
            handle.publish(|| Ok(serde_json::json!({})))?;
            entered.send(()).unwrap();
            Ok(())
        })
        .unwrap();
        release.send(()).unwrap();
        ready.recv_timeout(WAIT).unwrap();
        jobs.cancel_and_wait();
        assert_eq!(jobs.latest(Some("one")).unwrap().state, "error");
        assert_eq!(
            jobs.latest(Some("one")).unwrap().error.as_deref(),
            Some("synthetic failure")
        );
        assert_eq!(
            jobs.latest(Some("two")).unwrap().error.as_deref(),
            Some("Export worker panicked")
        );
        assert_eq!(
            jobs.latest(Some("three")).unwrap().error.as_deref(),
            Some("Export worker panicked")
        );
        assert_eq!(jobs.latest(Some("four")).unwrap().state, "done");
    }

    #[tokio::test]
    async fn cancellation_prevents_publication() {
        let jobs = Jobs::default();
        let (send, receive) = std::sync::mpsc::channel();
        let (entered, ready) = mpsc::channel();
        let job = jobs
            .start("one", "One", move |handle| {
                entered.send(()).unwrap();
                receive.recv_timeout(WAIT).unwrap();
                handle.publish(|| Ok(serde_json::json!({"published":true})))
            })
            .unwrap();
        ready.recv_timeout(WAIT).unwrap();
        assert!(jobs.busy("one"));
        jobs.cancel(Some(&job.id), None);
        send.send(()).unwrap();
        jobs.cancel_and_wait();
        let state = jobs.latest(Some("one")).unwrap();
        assert_eq!(state.state, "cancelled");
        assert!(state.result.is_none());
    }

    #[tokio::test]
    async fn shutdown_waits_for_cleanup_even_after_atomic_publication() {
        // Design decision: publication wins against later cancellation, but a
        // done status is not permission to overlap heavy work or exit mid-cleanup.
        let jobs = Jobs::default();
        let (entered, ready) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        jobs.start("one", "One", move |handle| {
            handle.publish(|| Ok(serde_json::json!({"published":true})))?;
            entered.send(()).unwrap();
            proceed.recv_timeout(WAIT).unwrap();
            Ok(())
        })
        .unwrap();
        ready.recv_timeout(WAIT).unwrap();
        assert_eq!(jobs.latest(Some("one")).unwrap().state, "done");
        let stopping = jobs.clone();
        let (finished, stopped) = mpsc::channel();
        let shutdown = std::thread::spawn(move || {
            stopping.cancel_and_wait();
            finished.send(()).unwrap();
        });
        assert!(stopped.recv_timeout(Duration::from_millis(100)).is_err());
        release.send(()).unwrap();
        stopped.recv_timeout(WAIT).unwrap();
        shutdown.join().unwrap();
        let job = jobs.latest(Some("one")).unwrap();
        assert_eq!(job.state, "done");
        assert_eq!(job.result.unwrap(), serde_json::json!({"published":true}));
    }

    #[tokio::test]
    async fn shutdown_cancels_workers_waits_for_cleanup_and_refuses_new_jobs() {
        let jobs = Jobs::default();
        let (entered, ready) = std::sync::mpsc::channel();
        let cleaned = Arc::new(AtomicBool::new(false));
        let worker_cleaned = cleaned.clone();
        let (release, proceed) = mpsc::channel();
        jobs.start("one", "One", move |handle| {
            entered.send(()).unwrap();
            while !handle.cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            entered.send(()).unwrap();
            proceed.recv_timeout(WAIT).unwrap();
            worker_cleaned.store(true, Ordering::Release);
            bail!("Cancelled")
        })
        .unwrap();
        ready.recv_timeout(WAIT).unwrap();
        let called = Arc::new(AtomicBool::new(false));
        let worker_called = called.clone();
        jobs.start("two", "Two", move |_| {
            worker_called.store(true, Ordering::Release);
            Ok(())
        })
        .unwrap();
        let stopping = jobs.clone();
        let (finished, stopped) = mpsc::channel();
        let shutdown = std::thread::spawn(move || {
            stopping.cancel_and_wait();
            finished.send(()).unwrap();
        });
        ready.recv_timeout(WAIT).unwrap();
        assert!(stopped.recv_timeout(Duration::from_millis(100)).is_err());
        assert!(jobs.start("three", "Three", |_| Ok(())).is_err());
        release.send(()).unwrap();
        stopped.recv_timeout(WAIT).unwrap();
        shutdown.join().unwrap();
        assert!(cleaned.load(Ordering::Acquire));
        assert_eq!(jobs.latest(Some("one")).unwrap().state, "cancelled");
        assert_eq!(jobs.latest(Some("two")).unwrap().state, "cancelled");
        assert!(!called.load(Ordering::Acquire));
        assert!(jobs.start("two", "Two", |_| Ok(())).is_err());
    }
}
