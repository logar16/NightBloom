use crate::{jobs::JobHandle, projects};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf, time::Instant};

#[derive(Clone)]
pub(super) struct Task {
    pub class: String,
    pub work: f64,
    pub step: usize,
    pub phase: &'static str,
}

impl Task {
    pub fn new(class: impl Into<String>, work: f64, step: usize) -> Self {
        Self {
            class: class.into(),
            work,
            step,
            phase: "align",
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Rate {
    work: f64,
    seconds: f64,
}

#[derive(Default, Serialize, Deserialize)]
struct History {
    version: String,
    rates: BTreeMap<String, Rate>,
}

fn version() -> String {
    format!(
        "1:{}:{}:{}:{}",
        super::BUILD_VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH,
        cfg!(debug_assertions)
    )
}

impl History {
    fn load(path: &std::path::Path) -> Result<Self> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    version: version(),
                    ..Self::default()
                });
            }
            Err(error) => return Err(error.into()),
        };
        let history: Self = serde_json::from_slice(&bytes)?;
        ensure!(
            history.version == version(),
            "Timing model changed; previous alignment samples cannot be reused"
        );
        ensure!(
            history.rates.values().all(|rate| rate.work.is_finite()
                && rate.work > 0.
                && rate.seconds.is_finite()
                && rate.seconds > 0.),
            "Invalid alignment timing samples"
        );
        Ok(history)
    }
}

pub(super) struct Timing<'a> {
    pub(super) job: &'a JobHandle,
    tasks: Vec<Task>,
    labels: Vec<String>,
    history: History,
    measured: BTreeMap<String, Rate>,
    path: PathBuf,
    cursor: usize,
    started: Option<Instant>,
    done: f64,
    bucket: u32,
    step_progress: Vec<f64>,
    phase: &'static str,
    progress: f64,
    message: String,
}

impl<'a> Timing<'a> {
    pub fn new(
        job: &'a JobHandle,
        tasks: Vec<Task>,
        labels: Vec<String>,
        path: PathBuf,
    ) -> Result<Self> {
        ensure!(
            !tasks.is_empty()
                && tasks.iter().all(|task| task.work.is_finite()
                    && task.work > 0.
                    && task.step < labels.len()),
            "Invalid alignment work plan"
        );
        let history = match History::load(&path) {
            Ok(history) => history,
            Err(error) => {
                job.note(&format!("Alignment ETA history unavailable: {error}"))?;
                History {
                    version: version(),
                    ..History::default()
                }
            }
        };
        Ok(Self {
            job,
            tasks,
            step_progress: vec![0.; labels.len()],
            labels,
            history,
            measured: BTreeMap::new(),
            path,
            cursor: 0,
            started: None,
            done: 0.,
            bucket: 0,
            phase: "align",
            progress: 0.,
            message: String::new(),
        })
    }

    pub fn revise(&mut self, class: &str, work: f64) -> Result<()> {
        ensure!(
            self.started.is_none() && work.is_finite() && work > 0.,
            "Invalid alignment work revision"
        );
        let task = self
            .tasks
            .get_mut(self.cursor)
            .context("Alignment work plan exhausted")?;
        task.class = class.into();
        task.work = work;
        Ok(())
    }

    pub fn start(&mut self, message: &str) -> Result<()> {
        ensure!(
            self.started.is_none() && self.cursor < self.tasks.len(),
            "Invalid alignment stage transition"
        );
        self.started = Some(Instant::now());
        self.done = 0.;
        self.bucket = 0;
        self.message = message.into();
        self.report()
    }

    pub fn advance(&mut self, work: f64) -> Result<()> {
        self.done = work.min(self.tasks[self.cursor].work);
        let bucket = (self.done / self.tasks[self.cursor].work * 100.) as u32;
        if bucket > self.bucket {
            self.bucket = bucket;
            self.report()?;
        }
        Ok(())
    }

    pub fn finish(&mut self) -> Result<()> {
        let seconds = self
            .started
            .take()
            .context("Alignment stage was not started")?
            .elapsed()
            .as_secs_f64();
        let task = &self.tasks[self.cursor];
        if seconds > 0. {
            let rate = self.measured.entry(task.class.clone()).or_default();
            rate.work += task.work;
            rate.seconds += seconds;
        }
        self.cursor += 1;
        self.done = 0.;
        self.report()
    }

    fn rate(&self, class: &str) -> Option<f64> {
        if let Some(started) = self.started
            && self.done > 0.
            && self.tasks[self.cursor].class == class
        {
            let previous = self.measured.get(class).cloned().unwrap_or_default();
            return Some(
                (previous.seconds + started.elapsed().as_secs_f64()) / (previous.work + self.done),
            );
        }
        self.measured
            .get(class)
            .or_else(|| self.history.rates.get(class))
            .map(|rate| rate.seconds / rate.work)
    }

    fn remaining(&self, index: usize) -> Option<f64> {
        if index < self.cursor {
            return Some(0.);
        }
        let task = &self.tasks[index];
        let seconds = self.rate(&task.class)?
            * (task.work - if index == self.cursor { self.done } else { 0. });
        if index == self.cursor
            && self.done == 0.
            && let Some(started) = self.started
        {
            let remaining = seconds - started.elapsed().as_secs_f64();
            return (remaining > 0.).then_some(remaining);
        }
        Some(seconds)
    }

    fn fraction(&self, indices: &[usize]) -> f64 {
        let weights: Option<Vec<_>> = indices
            .iter()
            .map(|i| {
                self.rate(&self.tasks[*i].class)
                    .map(|rate| rate * self.tasks[*i].work)
            })
            .collect();
        let weights = weights.unwrap_or_else(|| vec![1.; indices.len()]);
        let total: f64 = weights.iter().sum();
        indices
            .iter()
            .zip(weights)
            .map(|(i, weight)| {
                weight
                    * if *i < self.cursor {
                        1.
                    } else if *i == self.cursor {
                        self.done / self.tasks[*i].work
                    } else {
                        0.
                    }
            })
            .sum::<f64>()
            / total
    }

    fn report(&mut self) -> Result<()> {
        let phase = self
            .tasks
            .get(self.cursor)
            .map_or("publish", |task| task.phase);
        let mut steps = Vec::new();
        for (step, label) in self.labels.iter().enumerate() {
            let indices: Vec<_> = (0..self.tasks.len())
                .filter(|i| self.tasks[*i].step == step)
                .collect();
            let complete = indices.iter().all(|i| *i < self.cursor);
            let active = self.started.is_some() && indices.contains(&self.cursor);
            let fraction = if complete {
                1.
            } else {
                self.fraction(&indices).min(0.99)
            };
            self.step_progress[step] = self.step_progress[step].max(fraction);
            let eta: Option<Vec<_>> = indices.iter().map(|i| self.remaining(*i)).collect();
            steps.push(json!({"key":format!("alignment-{step}"),"label":label,
                "state":if complete { "done" } else if active { "running" } else { "pending" },
                "progress":self.step_progress[step],"eta_s":eta.map(|values| values.iter().sum::<f64>())}));
        }
        let indices: Vec<_> = (0..self.tasks.len())
            .filter(|i| self.tasks[*i].phase == phase)
            .collect();
        let complete = indices.iter().all(|i| *i < self.cursor);
        let fraction = if complete {
            1.
        } else {
            self.fraction(&indices).min(0.99)
        };
        self.progress = if self.phase == phase {
            self.progress.max(fraction)
        } else {
            fraction
        };
        self.phase = phase;
        let eta: Option<Vec<_>> = indices.iter().map(|i| self.remaining(*i)).collect();
        self.job.alignment(
            phase,
            self.progress,
            &self.message,
            steps,
            eta.map(|values| values.iter().sum()),
        )
    }

    pub fn save(&mut self) -> Result<()> {
        ensure!(
            self.cursor == self.tasks.len(),
            "Cannot retain incomplete alignment timings"
        );
        if self.job.cancelled() {
            anyhow::bail!("Cancelled");
        }
        self.history.rates.extend(self.measured.clone());
        if let Err(error) = projects::write_json(&self.path, &serde_json::to_value(&self.history)?)
        {
            self.job.note(&format!(
                "Alignment ETA history could not be saved: {error}"
            ))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_scale_measured_work_keep_unknown_costs_unknown_and_reload_samples() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("timings.json");
        let job = JobHandle::untracked();
        let tasks = vec![
            Task::new("warp", 200., 0),
            Task::new("warp", 600., 1),
            Task::new("blend", 400., 1),
        ];
        let mut timing = Timing::new(
            &job,
            tasks,
            vec!["Align A".into(), "Align mosaic".into()],
            path.clone(),
        )
        .unwrap();
        assert_eq!(timing.remaining(0), None);
        timing.measured.insert(
            "warp".into(),
            Rate {
                work: 100.,
                seconds: 2.,
            },
        );
        assert_eq!(timing.remaining(0), Some(4.));
        assert_eq!(timing.remaining(1), Some(12.));
        assert_eq!(timing.remaining(2), None);
        timing.measured.insert(
            "blend".into(),
            Rate {
                work: 200.,
                seconds: 3.,
            },
        );
        assert_eq!(timing.remaining(2), Some(6.));
        assert!(timing.save().is_err());
        assert!(!path.exists());
        timing.cursor = timing.tasks.len();
        timing.save().unwrap();
        let loaded = Timing::new(
            &job,
            vec![Task::new("warp", 500., 0)],
            vec!["Align".into()],
            path.clone(),
        )
        .unwrap();
        assert_eq!(loaded.remaining(0), Some(10.));
        let saved = std::fs::read(&path).unwrap();
        let cancelled = job.fork();
        cancelled.stop();
        let mut interrupted = Timing::new(
            &cancelled,
            vec![Task::new("warp", 500., 0)],
            vec!["Align".into()],
            path.clone(),
        )
        .unwrap();
        interrupted.cursor = interrupted.tasks.len();
        assert!(interrupted.save().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), saved);
        let mut history = History::load(&path).unwrap();
        history.version = "obsolete".into();
        projects::write_json(&path, &serde_json::to_value(history).unwrap()).unwrap();
        assert!(History::load(&path).is_err());
    }
}
