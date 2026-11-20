use anyhow::Result;
use serde::Serialize;
use std::sync::{
    atomic::{AtomicU64, AtomicU8, Ordering},
    Arc, Mutex,
};

const RUNNING: u8 = 0;
const CANCELLED: u8 = 1;
const COMMITTING: u8 = 2;

#[derive(Clone, Copy, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanPhase {
    #[default]
    Walking,
    Sampling,
    Hashing,
    Finishing,
    Verifying,
    Recycling,
}

#[derive(Serialize)]
pub struct ScanProgressSnapshot {
    phase: ScanPhase,
    processed_files: u64,
    total_files: Option<u64>,
    bytes_read: u64,
    can_cancel: bool,
}

#[derive(Clone, Default)]
pub struct ScanProgress {
    state: Arc<ProgressState>,
}

#[derive(Default)]
struct ProgressState {
    lifecycle: AtomicU8,
    phase: Mutex<(ScanPhase, Option<u64>)>,
    processed_files: AtomicU64,
    bytes_read: AtomicU64,
}

#[derive(Debug)]
pub struct ScanCancelled;

impl std::fmt::Display for ScanCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Scan cancelled")
    }
}

impl std::error::Error for ScanCancelled {}

impl ScanProgress {
    pub fn cancel(&self) {
        let _ = self.state.lifecycle.compare_exchange(
            RUNNING,
            CANCELLED,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.lifecycle.load(Ordering::Relaxed) == CANCELLED
    }

    pub(crate) fn begin_recycling(&self) -> Result<()> {
        self.state
            .lifecycle
            .compare_exchange(RUNNING, COMMITTING, Ordering::Relaxed, Ordering::Relaxed)
            .map_err(|_| ScanCancelled)?;
        self.set_phase(ScanPhase::Recycling, None);
        Ok(())
    }

    pub fn snapshot(&self) -> ScanProgressSnapshot {
        let &(phase, total_files) = &*self.state.phase.lock().unwrap();
        ScanProgressSnapshot {
            phase,
            total_files,
            processed_files: self.state.processed_files.load(Ordering::Relaxed),
            bytes_read: self.state.bytes_read.load(Ordering::Relaxed),
            can_cancel: self.state.lifecycle.load(Ordering::Relaxed) == RUNNING,
        }
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            return Err(ScanCancelled.into());
        }
        Ok(())
    }

    pub(crate) fn set_phase(&self, phase: ScanPhase, total_files: Option<usize>) {
        let mut state = self.state.phase.lock().unwrap();
        if total_files.is_some() {
            self.state.processed_files.store(0, Ordering::Relaxed);
        }
        *state = (phase, total_files.map(|count| count as u64));
    }

    pub(crate) fn file_processed(&self) {
        self.state.processed_files.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn read_bytes(&self, bytes: usize) {
        self.state
            .bytes_read
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_cannot_hide_an_operation_that_has_started_recycling() {
        let progress = ScanProgress::default();
        let cancel = progress.clone();
        progress.begin_recycling().unwrap();
        cancel.cancel();
        assert!(!progress.snapshot().can_cancel);
        assert!(!progress.is_cancelled());
        assert!(matches!(progress.snapshot().phase, ScanPhase::Recycling));

        let cancelled = ScanProgress::default();
        cancelled.cancel();
        assert!(cancelled
            .begin_recycling()
            .unwrap_err()
            .is::<ScanCancelled>());
    }
}
