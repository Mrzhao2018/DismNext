use anyhow::{anyhow, Result};
use dism_core::scan_progress::{ScanCancelled, ScanProgress};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

const MAX_FINISHED_JOBS: usize = 4;

#[derive(Debug)]
pub struct ScanBusy;

impl std::fmt::Display for ScanBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Another scan is already running")
    }
}

impl std::error::Error for ScanBusy {}

#[derive(Default)]
pub struct ScanJobs {
    state: Mutex<JobState>,
}

#[derive(Default)]
struct JobState {
    sequence: u64,
    active: Option<Arc<Job>>,
    finished: VecDeque<(u64, Arc<Finished>)>,
}

struct Job {
    id: u64,
    observable: bool,
    progress: ScanProgress,
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Finished {
    Completed { result: Value },
    Cancelled,
    Failed { error: String },
}

struct Reservation {
    jobs: Arc<ScanJobs>,
    job: Arc<Job>,
}

impl Reservation {
    fn finish(&self, result: Result<Value>) {
        let mut state = self.jobs.state.lock().unwrap();
        let outcome = if self.job.progress.is_cancelled() {
            Finished::Cancelled
        } else {
            match result {
                Ok(result) => Finished::Completed { result },
                Err(err) if err.is::<ScanCancelled>() => Finished::Cancelled,
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

impl Drop for Reservation {
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

impl ScanJobs {
    fn reserve(self: &Arc<Self>, observable: bool) -> Result<Reservation> {
        let mut state = self.state.lock().unwrap();
        if state.active.is_some() {
            return Err(ScanBusy.into());
        }
        state.sequence += 1;
        let job = Arc::new(Job {
            id: state.sequence,
            observable,
            progress: ScanProgress::default(),
        });
        state.active = Some(Arc::clone(&job));
        Ok(Reservation {
            jobs: Arc::clone(self),
            job,
        })
    }

    pub fn run_inline<T>(self: &Arc<Self>, task: impl FnOnce() -> Result<T>) -> Result<T> {
        let _reservation = self.reserve(false)?;
        task()
    }

    pub fn start(
        self: &Arc<Self>,
        task: impl FnOnce(&ScanProgress) -> Result<Value> + Send + 'static,
    ) -> Result<u64> {
        let reservation = self.reserve(true)?;
        let id = reservation.job.id;
        std::thread::Builder::new()
            .name(format!("scan-{id}"))
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| task(&reservation.job.progress)))
                    .unwrap_or_else(|_| Err(anyhow!("Scan worker stopped unexpectedly")));
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
            return Ok(json!({ "state": phase, "progress": job.progress.snapshot() }));
        }
        let finished = state
            .finished
            .iter()
            .find(|(job_id, _)| *job_id == id)
            .map(|(_, outcome)| Arc::clone(outcome))
            .ok_or_else(|| anyhow!("Scan {id} is no longer available"))?;
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
            return Err(anyhow!("Scan {id} is no longer available"));
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
        let jobs = Arc::new(ScanJobs::default());
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
