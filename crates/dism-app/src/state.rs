use crate::{desktop_lifecycle::DesktopLifecycle, request_security::Session, scan_jobs::ScanJobs};
use dism_core::cleaner::{CleanupBatchScan, CleanupLease, CleanupReviews, CleanupScanResult};
use dism_core::maintenance::MaintenanceProgress;
use dism_core::systasks::invalidate_systasks_cache;
use dism_core::uninstaller::Win32Uninstall;
use dism_core::{Catalog, SnapshotManager};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, RwLock, TryLockError};
use std::time::{Duration, Instant};

pub(crate) struct AppState {
    pub(crate) port: u16,
    pub(crate) session: Session,
    pub(crate) dev_origin: bool,
    pub(crate) catalog_path: Option<PathBuf>,
    pub(crate) catalog: RwLock<Catalog>,
    pub(crate) snap_mgr: SnapshotManager,
    pub(crate) scan_cache: RwLock<CleanupScanCache>,
    pub(crate) cleanup_operation: Mutex<CleanupReviewSlot>,
    pub(crate) scan_jobs: Arc<ScanJobs>,
    pub(crate) maintenance_jobs: Arc<ScanJobs<MaintenanceProgress>>,
    pub(crate) uninstall: Mutex<Option<Win32Uninstall>>,
    pub(crate) ui_dir: Option<PathBuf>,
    pub(crate) desktop: Option<DesktopLifecycle>,
    pub(crate) requests: RwLock<()>,
    pub(crate) relaunching: AtomicBool,
}

const FULL_SCAN_TTL: Duration = Duration::from_secs(60);
#[derive(Default)]
pub(crate) struct CleanupReviewSlot {
    pub(crate) reviews: CleanupReviews,
    pub(crate) maintenance_active: bool,
}

#[derive(Default)]
pub(crate) struct CleanupScanCache {
    pub(crate) rules: HashMap<String, CleanupScanResult>,
    full_scan: Option<(Instant, Arc<CleanupBatchScan>)>,
    invalidated_at: Option<Instant>,
}

impl CleanupScanCache {
    pub(crate) fn invalidate(&mut self) {
        self.full_scan = None;
        self.rules.clear();
        self.invalidated_at = Some(Instant::now());
    }

    pub(crate) fn can_store(&self, started: Instant) -> bool {
        self.invalidated_at
            .is_none_or(|invalidated| started >= invalidated)
    }

    pub(crate) fn store_full_scan(
        &mut self,
        results: CleanupBatchScan,
        started: Instant,
    ) -> Arc<CleanupBatchScan> {
        let results = Arc::new(results);
        if self.can_store(started) {
            self.full_scan = Some((started, Arc::clone(&results)));
        }
        results
    }

    pub(crate) fn fresh_full_scan(&self, now: Instant) -> Option<Arc<CleanupBatchScan>> {
        let (started, results) = self.full_scan.as_ref()?;
        (now.saturating_duration_since(*started) < FULL_SCAN_TTL).then(|| Arc::clone(results))
    }
}

impl AppState {
    pub(crate) fn lock_cleanup(&self) -> MutexGuard<'_, CleanupReviewSlot> {
        self.cleanup_operation.lock().unwrap_or_else(|poisoned| {
            let mut operation = poisoned.into_inner();
            operation.reviews.clear();
            self.cleanup_operation.clear_poison();
            operation
        })
    }

    pub(crate) fn begin_cleanup_mutation(&self) -> anyhow::Result<CleanupLease> {
        let lease = CleanupLease::try_acquire()?;
        let mut operation = self.try_cleanup()?;
        self.invalidate_cleanup(&mut operation);
        Ok(lease)
    }

    pub(crate) fn try_cleanup(
        &self,
    ) -> Result<MutexGuard<'_, CleanupReviewSlot>, crate::scan_jobs::ScanBusy> {
        let operation = match self.cleanup_operation.try_lock() {
            Ok(operation) => operation,
            Err(TryLockError::WouldBlock) => {
                return Err(crate::scan_jobs::ScanBusy("cleanup operation"))
            }
            Err(TryLockError::Poisoned(poisoned)) => {
                let mut operation = poisoned.into_inner();
                operation.reviews.clear();
                self.cleanup_operation.clear_poison();
                operation
            }
        };
        if operation.maintenance_active {
            return Err(crate::scan_jobs::ScanBusy("maintenance task"));
        }
        Ok(operation)
    }

    pub(crate) fn invalidate_cleanup(&self, operation: &mut CleanupReviewSlot) {
        operation.reviews.clear();
        self.scan_cache.write().unwrap().invalidate();
    }

    pub(crate) fn publish_cleanup_scan(
        &self,
        results: CleanupBatchScan,
        started: Instant,
    ) -> serde_json::Value {
        let results = {
            let mut cache = self.scan_cache.write().unwrap();
            if cache.can_store(started) {
                cache.rules.clear();
            }
            cache.store_full_scan(results, started)
        };
        serde_json::json!({ "items": results.items, "summary": results.summary })
    }
}

pub(crate) struct MaintenanceGuard<'a>(pub(crate) &'a AppState);

impl Drop for MaintenanceGuard<'_> {
    fn drop(&mut self) {
        invalidate_systasks_cache();
        let mut operation = self.0.lock_cleanup();
        operation.maintenance_active = false;
        self.0.invalidate_cleanup(&mut operation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::cleanup::cleanup_rules_stub_list;
    use dism_core::cleaner::{ReviewedCleanup, CLEANUP_REVIEW_TTL};
    use dism_core::SystemInfo;

    #[test]
    fn cleanup_reviews_are_single_use_and_expire() {
        let sys = SystemInfo::collect();
        let new_plan = || ReviewedCleanup::prepare(&[], &[], &sys).0;
        let mut slot = CleanupReviews::default();
        let now = Instant::now();
        let first = slot.store(new_plan(), now);
        let current = slot.store(new_plan(), now);
        assert_ne!(first, current);
        assert!(slot.take(&first, now).is_none());
        assert!(slot.take(&current, now).is_some());
        assert!(slot.take(&current, now).is_none());

        let last_valid = slot.store(new_plan(), now);
        assert!(slot
            .take(
                &last_valid,
                now + CLEANUP_REVIEW_TTL - Duration::from_millis(1)
            )
            .is_some());
        let expiring = slot.store(new_plan(), now);
        assert!(slot.take(&expiring, now + CLEANUP_REVIEW_TTL).is_none());
    }

    #[test]
    fn report_cache_requires_a_fresh_full_scan() {
        let catalog = Catalog::load(None).unwrap();
        let sys = SystemInfo::collect();
        let mut cache = CleanupScanCache::default();
        let entries = cleanup_rules_stub_list(&catalog, &cache.rules, &sys);
        let preview: CleanupScanResult =
            serde_json::from_value(entries[0]["rule"].clone()).unwrap();
        let started = Instant::now();

        cache.rules.insert(preview.id.clone(), preview.clone());
        assert!(cache.fresh_full_scan(started).is_none());

        cache.store_full_scan(
            CleanupBatchScan {
                items: vec![preview.clone()],
                ..CleanupBatchScan::default()
            },
            started,
        );
        cache.rules.get_mut(&preview.id).unwrap().cleanable_bytes = 50;
        let cached = cache.fresh_full_scan(started).unwrap();
        assert_eq!(cached.items[0].cleanable_bytes, preview.cleanable_bytes);
        assert!(cache.fresh_full_scan(started + FULL_SCAN_TTL).is_none());

        cache.store_full_scan(CleanupBatchScan::default(), started);
        assert!(cache.fresh_full_scan(started).unwrap().items.is_empty());

        cache.invalidate();
        cache.store_full_scan(CleanupBatchScan::default(), started);
        assert!(cache.fresh_full_scan(Instant::now()).is_none());
    }
}
