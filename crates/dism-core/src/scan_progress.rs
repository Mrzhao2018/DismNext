use anyhow::Result;
use serde::Serialize;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Mutex,
};

#[derive(Clone, Copy, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanPhase {
    #[default]
    Walking,
    Sampling,
    Hashing,
    Finishing,
}

#[derive(Serialize)]
pub struct ScanProgressSnapshot {
    phase: ScanPhase,
    processed_files: u64,
    total_files: Option<u64>,
    bytes_read: u64,
}

#[derive(Default)]
pub struct ScanProgress {
    cancelled: AtomicBool,
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
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> ScanProgressSnapshot {
        let &(phase, total_files) = &*self.phase.lock().unwrap();
        ScanProgressSnapshot {
            phase,
            total_files,
            processed_files: self.processed_files.load(Ordering::Relaxed),
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            return Err(ScanCancelled.into());
        }
        Ok(())
    }

    pub(crate) fn set_phase(&self, phase: ScanPhase, total_files: Option<usize>) {
        let mut state = self.phase.lock().unwrap();
        if total_files.is_some() {
            self.processed_files.store(0, Ordering::Relaxed);
        }
        *state = (phase, total_files.map(|count| count as u64));
    }

    pub(crate) fn file_processed(&self) {
        self.processed_files.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn read_bytes(&self, bytes: usize) {
        self.bytes_read.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}
