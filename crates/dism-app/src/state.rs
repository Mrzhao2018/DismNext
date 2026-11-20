use crate::{desktop_lifecycle::DesktopLifecycle, request_security::Session, scan_jobs::ScanJobs};
use dism_core::cleaner::{CleanupBatchScan, CleanupScanResult, ReviewedCleanup};
use dism_core::maintenance::MaintenanceProgress;
use dism_core::systasks::invalidate_systasks_cache;
use dism_core::uninstaller::Win32Uninstall;
use dism_core::{Catalog, SnapshotManager};
use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
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
}

const FULL_SCAN_TTL: Duration = Duration::from_secs(60);
const CLEANUP_REVIEW_TTL: Duration = Duration::from_secs(5 * 60);
#[derive(Default)]
pub(crate) struct CleanupReviewSlot {
    ids: RandomState,
    sequence: u64,
    pub(crate) pending: Option<(String, Instant, ReviewedCleanup)>,
    pub(crate) maintenance_active: bool,
}

impl CleanupReviewSlot {
    pub(crate) fn store(&mut self, plan: ReviewedCleanup, now: Instant) -> String {
        self.sequence += 1;
        let id = format!("{:016x}", self.ids.hash_one(self.sequence));
        self.pending = Some((id.clone(), now, plan));
        id
    }

    pub(crate) fn take(&mut self, id: &str, now: Instant) -> Option<ReviewedCleanup> {
        let (pending_id, created, _) = self.pending.as_ref()?;
        if now.saturating_duration_since(*created) >= CLEANUP_REVIEW_TTL {
            self.pending = None;
            return None;
        }
        if id != pending_id {
            return None;
        }
        self.pending.take().map(|(_, _, plan)| plan)
    }
}

#[derive(Debug)]
pub(crate) struct CleanupReviewUnavailable;

impl std::fmt::Display for CleanupReviewUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cleanup review is no longer available. Review the selected rules again.")
    }
}

impl std::error::Error for CleanupReviewUnavailable {}

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
    pub(crate) fn begin_cleanup_mutation(&self) -> MutexGuard<'_, CleanupReviewSlot> {
        let mut operation = self.cleanup_operation.lock().unwrap();
        operation.pending = None;
        self.scan_cache.write().unwrap().invalidate();
        operation
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
        let mut operation = self.0.cleanup_operation.lock().unwrap();
        operation.maintenance_active = false;
        operation.pending = None;
        self.0.scan_cache.write().unwrap().invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::cleanup::cleanup_rules_stub_list;
    use dism_core::SystemInfo;

    #[test]
    fn cleanup_reviews_are_single_use_and_expire() {
        let sys = SystemInfo::collect();
        let new_plan = || ReviewedCleanup::prepare(&[], &[], &sys).0;
        let mut slot = CleanupReviewSlot::default();
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
        assert!(slot.pending.is_none());
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
