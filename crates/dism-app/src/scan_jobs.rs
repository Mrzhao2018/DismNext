use anyhow::{anyhow, Result};
use dism_core::maintenance::{MaintenanceCancelled, MaintenanceProgress};
use dism_core::scan_progress::{ScanCancelled, ScanProgress};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

const MAX_FINISHED_JOBS: usize = 4;

#[derive(Debug)]
pub struct ScanBusy(pub(crate) &'static str);

impl std::fmt::Display for ScanBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Another {} is already running", self.0)
    }
}

impl std::error::Error for ScanBusy {}

pub trait JobProgress: Default + Send + Sync + 'static {
    const KIND: &'static str;
    fn cancel(&self);
    fn is_cancelled(&self) -> bool;
    fn describe(&self) -> Value;
    fn result_cancelled(&self, result: &Result<Value>) -> bool;
}

impl JobProgress for ScanProgress {
    const KIND: &'static str = "scan";
    fn cancel(&self) {
        self.cancel();
    }
    fn is_cancelled(&self) -> bool {
        self.is_cancelled()
    }
    fn describe(&self) -> Value {
        json!(self.snapshot())
    }
    fn result_cancelled(&self, result: &Result<Value>) -> bool {
        self.is_cancelled()
            || result
                .as_ref()
                .is_err_and(|error| error.is::<ScanCancelled>())
    }
}

impl JobProgress for MaintenanceProgress {
    const KIND: &'static str = "maintenance task";
    fn cancel(&self) {
        self.cancel();
    }
    fn is_cancelled(&self) -> bool {
        self.is_cancelled()
    }
    fn describe(&self) -> Value {
        json!(self.snapshot())
    }
    fn result_cancelled(&self, result: &Result<Value>) -> bool {
        result
            .as_ref()
            .is_err_and(|error| error.is::<MaintenanceCancelled>())
    }
}

#[derive(Default)]
pub struct ScanJobs<P: JobProgress = ScanProgress> {
    state: Mutex<JobState<P>>,
}

#[derive(Default)]
struct JobState<P> {
    sequence: u64,
    active: Option<Arc<Job<P>>>,
    finished: VecDeque<(u64, Arc<Finished>)>,
}

struct Job<P> {
    id: u64,
    observable: bool,
    progress: P,
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Finished {
    Completed { result: Value },
    Cancelled,
    Failed { error: String },
}

struct Reservation<P: JobProgress> {
    jobs: Arc<ScanJobs<P>>,
    job: Arc<Job<P>>,
}

impl<P: JobProgress> Reservation<P> {
    fn finish(&self, result: Result<Value>) {
        let mut state = self.jobs.state.lock().unwrap();
        let outcome = if self.job.progress.result_cancelled(&result) {
            Finished::Cancelled
        } else {
            match result {
                Ok(result) => Finished::Completed { result },
                Err(err) => Finished::Failed {
                    error: err.to_string(),
                },
            }
        };
        state.finished.push_back((self.job.id, Arc::new(outcome)));
        if state.finished.len() > MAX_FINISHED_JOBS {
            state.finished.pop_front();
        }
        state.active = None;
    }
}

impl<P: JobProgress> Drop for Reservation<P> {
    fn drop(&mut self) {
        let mut state = self.jobs.state.lock().unwrap();
        if state
            .active
            .as_ref()
            .is_some_and(|job| job.id == self.job.id)
        {
            state.active = None;
        }
    }
}

impl<P: JobProgress> ScanJobs<P> {
    pub fn is_active(&self) -> bool {
        self.state.lock().unwrap().active.is_some()
    }

    fn reserve(self: &Arc<Self>, observable: bool) -> Result<Reservation<P>> {
        let mut state = self.state.lock().unwrap();
        if state.active.is_some() {
            return Err(ScanBusy(P::KIND).into());
        }
        state.sequence += 1;
        let job = Arc::new(Job {
            id: state.sequence,
            observable,
            progress: P::default(),
        });
        state.active = Some(Arc::clone(&job));
        Ok(Reservation {
            jobs: Arc::clone(self),
            job,
        })
    }

    pub fn run_inline<T>(self: &Arc<Self>, task: impl FnOnce() -> Result<T>) -> Result<T> {
        self.run_inline_with_progress(|_| task())
    }

    pub fn run_inline_with_progress<T>(
        self: &Arc<Self>,
        task: impl FnOnce(&P) -> Result<T>,
    ) -> Result<T> {
        let reservation = self.reserve(false)?;
        task(&reservation.job.progress)
    }

    pub fn start(
        self: &Arc<Self>,
        task: impl FnOnce(&P) -> Result<Value> + Send + 'static,
    ) -> Result<u64> {
        let reservation = self.reserve(true)?;
        let id = reservation.job.id;
        std::thread::Builder::new()
            .name(format!("{}-{id}", P::KIND))
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| task(&reservation.job.progress)))
                    .unwrap_or_else(|_| Err(anyhow!("{} worker stopped unexpectedly", P::KIND)));
                reservation.finish(result);
            })?;
        Ok(id)
    }

    pub fn status(&self, id: u64) -> Result<Value> {
        let state = self.state.lock().unwrap();
        if let Some(job) = state
            .active
            .as_ref()
            .filter(|job| job.observable && job.id == id)
        {
            let phase = if job.progress.is_cancelled() {
                "cancelling"
            } else {
                "running"
            };
            return Ok(json!({ "state": phase, "progress": job.progress.describe() }));
        }
        let finished = state
            .finished
            .iter()
            .find(|(job_id, _)| *job_id == id)
            .map(|(_, outcome)| Arc::clone(outcome))
            .ok_or_else(|| anyhow!("{} {id} is no longer available", P::KIND))?;
        drop(state);
        Ok(serde_json::to_value(&*finished)?)
    }

    pub fn cancel(&self, id: u64) -> Result<()> {
        let state = self.state.lock().unwrap();
        if let Some(job) = state
            .active
            .as_ref()
            .filter(|job| job.observable && job.id == id)
        {
            job.progress.cancel();
        } else if !state.finished.iter().any(|(job_id, _)| *job_id == id) {
            return Err(anyhow!("{} {id} is no longer available", P::KIND));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn wait_for_state(jobs: &ScanJobs, id: u64, expected: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = jobs.status(id).unwrap();
            if status["state"] == expected {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "Expected {expected}, got {status}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn cancellation_releases_the_shared_slot_only_after_the_worker_stops() {
        let jobs = Arc::new(ScanJobs::<ScanProgress>::default());
        let (started, working) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let id = jobs
            .start(move |progress| {
                started.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(5)).unwrap();
                assert!(progress.is_cancelled());
                Ok(json!({ "partial": true }))
            })
            .unwrap();

        working.recv_timeout(Duration::from_secs(5)).unwrap();
        let running = jobs.status(id).unwrap();
        assert_eq!(running["state"], "running");
        assert_eq!(running["progress"]["processed_files"], 0);
        assert!(jobs.run_inline(|| Ok(())).unwrap_err().is::<ScanBusy>());
        jobs.cancel(id).unwrap();
        assert_eq!(jobs.status(id).unwrap()["state"], "cancelling");
        assert!(jobs
            .start(|_| Ok(json!(null)))
            .unwrap_err()
            .is::<ScanBusy>());
        release.send(()).unwrap();
        assert_eq!(
            wait_for_state(&jobs, id, "cancelled"),
            json!({ "state": "cancelled" })
        );

        let next = jobs.start(|_| Ok(json!({ "complete": true }))).unwrap();
        jobs.cancel(id).unwrap();
        assert_eq!(
            wait_for_state(&jobs, next, "completed")["result"],
            json!({ "complete": true })
        );
    }
}
