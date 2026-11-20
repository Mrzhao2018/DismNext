use super::custom_procs::driver_target_label;
use super::{
    cleanup_report, diagnose_file, old_enough, plan_cleanup, CleanupAction, CleanupExecutionReport,
    CleanupPath, CleanupRule, FileStatus, PlannedCleanup, PlannedTarget, RuleCleanOutcome,
    SystemInfo, TargetKey,
};
use crate::drivers::compute_drivers_uncached;
use crate::file_stamp::{delete_open_target, open_for_verification, TargetStamp};
use crate::recycle_bin::{move_batch, RecycleRequest, Verified};
use crate::systasks::compute_orphaned_installer_files_uncached;
use crate::win_util::{has_reparse_point_in_ancestors, is_local_drive_path, pin_directory};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::{BuildHasher, RandomState};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    ERROR_DIR_NOT_EMPTY, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_READ_DATA, FILE_SHARE_DELETE, FILE_SHARE_READ,
};
use windows_sys::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};

use super::versions::VersionGuard;

pub const CLEANUP_REVIEW_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Default)]
pub struct CleanupReviews {
    ids: RandomState,
    sequence: u64,
    pending: Option<(String, Instant, ReviewedCleanup)>,
}

impl CleanupReviews {
    pub fn store(&mut self, plan: ReviewedCleanup, now: Instant) -> String {
        self.sequence = self.sequence.wrapping_add(1);
        let id = format!("{:016x}", self.ids.hash_one(self.sequence));
        self.pending = Some((id.clone(), now, plan));
        id
    }

    pub fn take(&mut self, id: &str, now: Instant) -> Option<ReviewedCleanup> {
        let (expected, created, _) = self.pending.as_ref()?;
        if now.saturating_duration_since(*created) >= CLEANUP_REVIEW_TTL {
            self.clear();
            return None;
        }
        if id != expected {
            return None;
        }
        self.pending.take().map(|(_, _, plan)| plan)
    }

    pub fn clear(&mut self) {
        self.pending = None;
    }
}

#[derive(Debug)]
pub struct CleanupReviewUnavailable;

impl std::fmt::Display for CleanupReviewUnavailable {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str("Cleanup review is no longer available. Review the selected rules again.")
    }
}

impl std::error::Error for CleanupReviewUnavailable {}

/// Desktop cleanup plan. Execution consumes the plan and only considers its captured targets.
pub struct ReviewedCleanup {
    outcomes: Vec<RuleCleanOutcome>,
    files: Vec<(PlannedTarget, TargetStamp)>,
    directories: Vec<(usize, CleanupPath, TargetStamp)>,
    assoc_changed: bool,
}

#[derive(Debug)]
struct RecycleIoError {
    kind: io::ErrorKind,
    raw_os_error: Option<i32>,
    message: String,
}

impl std::fmt::Display for RecycleIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RecycleIoError {}

#[derive(Debug)]
struct RecycleChangedError;

impl std::fmt::Display for RecycleChangedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Target file changed before recycling")
    }
}

impl std::error::Error for RecycleChangedError {}

fn acquire_version_lock(
    path: &CleanupPath,
    locks: &mut HashMap<usize, (Arc<VersionGuard>, Option<File>)>,
) -> bool {
    let Some(guard) = path.version_guard.as_ref() else {
        return true;
    };
    let key = Arc::as_ptr(guard) as usize;
    locks
        .entry(key)
        .or_insert_with(|| (Arc::clone(guard), guard.lock()))
        .1
        .is_some()
}

impl ReviewedCleanup {
    pub fn prepare(
        rules: &[CleanupRule],
        rule_ids: &[String],
        sys: &SystemInfo,
    ) -> (Self, CleanupExecutionReport) {
        let wanted = rule_ids.iter().map(String::as_str).collect();
        let PlannedCleanup {
            mut outcomes,
            targets,
            directories,
            assoc_changed,
        } = plan_cleanup(rules, &wanted, sys, true);
        let mut files = Vec::new();
        let mut reviewed_dirs = Vec::new();

        for mut target in targets {
            let outcome = &mut outcomes[target.owner];
            if target.status != FileStatus::Ready {
                outcome.record_target(&target, Ok((target.status, 0)));
                continue;
            }
            let is_driver = matches!(target.action, CleanupAction::RemoveDriver(_));
            let access = if is_driver { 0 } else { DELETE };
            match capture_stamp(&target.target.path, false, access) {
                Ok(stamp) => {
                    if !is_driver {
                        target.size = stamp.file_size().unwrap();
                    }
                    files.push((target, stamp));
                }
                Err(error) => record_check_error(outcome, &target.target.path, error),
            }
        }
        for (owner, directory) in directories {
            match capture_stamp(&directory.path, true, DELETE) {
                Ok(stamp) => reviewed_dirs.push((owner, directory, stamp)),
                Err(error) => record_check_error(&mut outcomes[owner], &directory.path, error),
            }
        }

        let mut preview = outcomes.clone();
        for (target, _) in &files {
            preview[target.owner].record_target(target, Ok((FileStatus::Ready, target.size)));
        }
        for (owner, _, _) in &reviewed_dirs {
            preview[*owner].review.get_or_insert_default().directories += 1;
        }
        let plan = Self {
            outcomes,
            files,
            directories: reviewed_dirs,
            assoc_changed,
        };
        (plan, cleanup_report(preview, true))
    }

    pub fn confirm(self, _lease: &super::CleanupLease) -> CleanupExecutionReport {
        self.execute()
    }

    pub(super) fn blocked(mut self, error: anyhow::Error) -> CleanupExecutionReport {
        for outcome in &mut self.outcomes {
            outcome.record_error("Cleanup", &error);
        }
        cleanup_report(self.outcomes, false)
    }

    pub(super) fn execute(mut self) -> CleanupExecutionReport {
        // Recheck eligibility for dynamic system rules; new matches are never added to the plan.
        let installer_targets: HashSet<_> = if self
            .files
            .iter()
            .any(|(target, _)| matches!(target.action, CleanupAction::Recycle))
        {
            compute_orphaned_installer_files_uncached()
                .into_iter()
                .map(|(path, _)| TargetKey::file(&path))
                .collect()
        } else {
            HashSet::new()
        };
        let driver_targets: HashMap<_, _> = if self
            .files
            .iter()
            .any(|(target, _)| matches!(target.action, CleanupAction::RemoveDriver(_)))
        {
            compute_drivers_uncached()
                .into_iter()
                .filter(|driver| driver.is_outdated_duplicate)
                .map(|driver| {
                    (
                        driver.published_name.to_ascii_lowercase(),
                        driver_target_label(&driver),
                    )
                })
                .collect()
        } else {
            HashMap::new()
        };

        let mut version_locks: HashMap<usize, (Arc<VersionGuard>, Option<File>)> = HashMap::new();
        let mut recycle_targets: Vec<PlannedTarget> = Vec::new();
        let mut recycle_requests: Vec<RecycleRequest<()>> = Vec::new();

        for (target, stamp) in self.files {
            if !acquire_version_lock(&target.target, &mut version_locks) {
                self.outcomes[target.owner]
                    .review
                    .get_or_insert_default()
                    .skipped_changed += 1;
                continue;
            }
            if matches!(target.action, CleanupAction::Recycle) {
                let outcome = &mut self.outcomes[target.owner];
                let path = &target.target.path;
                if !installer_targets.contains(&TargetKey::file(path)) {
                    outcome.review.get_or_insert_default().skipped_changed += 1;
                    continue;
                }
                let (status, _) = diagnose_file(path, &target.path);
                if status == FileStatus::Locked {
                    outcome.skipped_locked_files += 1;
                    continue;
                }
                let path_clone = path.clone();
                let modified_before = target.target.modified_before;
                recycle_requests.push(RecycleRequest {
                    path: path_clone.clone(),
                    verify: Box::new(move || {
                        let to_err = |err: io::Error| {
                            anyhow::Error::new(RecycleIoError {
                                kind: err.kind(),
                                raw_os_error: err.raw_os_error(),
                                message: err.to_string(),
                            })
                        };
                        let file = open_for_verification(
                            &path_clone,
                            false,
                            FILE_READ_DATA,
                            FILE_SHARE_READ | FILE_SHARE_DELETE,
                        )
                        .map_err(to_err)?;
                        let actual = TargetStamp::read(&file, false).map_err(to_err)?;
                        if actual != stamp {
                            return Err(anyhow::Error::new(RecycleChangedError));
                        }
                        if modified_before.is_some() {
                            let meta = file.metadata().map_err(to_err)?;
                            if !old_enough(&meta, modified_before) {
                                return Err(anyhow::Error::new(RecycleChangedError));
                            }
                        }
                        Ok(Verified {
                            value: (),
                            hold: Some(file),
                        })
                    }),
                });
                recycle_targets.push(target);
                continue;
            }

            let outcome = &mut self.outcomes[target.owner];
            let path = &target.target.path;
            let (access, sharing) = match &target.action {
                CleanupAction::Delete => (DELETE, 0),
                // PnPUtil acts on the driver package outside our handle.
                _ => (FILE_READ_DATA, FILE_SHARE_READ | FILE_SHARE_DELETE),
            };
            let Some(file) = verify_target(outcome, path, &stamp, false, access, sharing) else {
                continue;
            };
            if target.target.modified_before.is_some() {
                match file.metadata() {
                    Ok(meta) if old_enough(&meta, target.target.modified_before) => (),
                    Ok(_) => {
                        outcome.review.get_or_insert_default().skipped_changed += 1;
                        continue;
                    }
                    Err(error) => {
                        record_check_error(outcome, path, error);
                        continue;
                    }
                }
            }
            match &target.action {
                CleanupAction::Delete => {
                    let result = delete_open_target(&file, false).map_err(Into::into);
                    drop(file);
                    outcome
                        .record_target(&target, result.map(|()| (FileStatus::Ready, target.size)));
                }
                CleanupAction::RemoveDriver(name)
                    if driver_targets.get(name) == Some(&target.path) =>
                {
                    drop(file);
                    let result = crate::drivers::delete_driver(name).map(|_| ());
                    outcome
                        .record_target(&target, result.map(|()| (FileStatus::Ready, target.size)));
                }
                _ => {
                    drop(file);
                    outcome.review.get_or_insert_default().skipped_changed += 1;
                }
            }
        }

        if !recycle_requests.is_empty() {
            let results = move_batch(recycle_requests);
            for (target, result) in recycle_targets.into_iter().zip(results) {
                let outcome = &mut self.outcomes[target.owner];
                match result {
                    Ok(()) => outcome.record_target(&target, Ok((FileStatus::Ready, target.size))),
                    Err(error) => {
                        if error.downcast_ref::<RecycleChangedError>().is_some() {
                            outcome.review.get_or_insert_default().skipped_changed += 1;
                        } else if let Some(io_err) = error.downcast_ref::<RecycleIoError>() {
                            record_check_error_parts(
                                outcome,
                                &target.target.path,
                                io_err.kind,
                                io_err.raw_os_error,
                                &io_err.message,
                            );
                        } else {
                            outcome.record_target(&target, Err(error));
                        }
                    }
                }
            }
        }

        self.directories
            .sort_by_key(|(_, dir, _)| Reverse(dir.path.components().count()));
        for (owner, directory, stamp) in self.directories {
            let outcome = &mut self.outcomes[owner];
            if !acquire_version_lock(&directory, &mut version_locks) {
                outcome.review.get_or_insert_default().skipped_changed += 1;
                continue;
            }
            let path = &directory.path;
            let Some(dir) = verify_target(outcome, path, &stamp, true, DELETE, 0) else {
                continue;
            };
            match delete_open_target(&dir, true) {
                Ok(()) => outcome.review.get_or_insert_default().directories += 1,
                // Directory rules only remove empty directories.
                Err(error) if error.raw_os_error() == Some(ERROR_DIR_NOT_EMPTY as i32) => (),
                Err(error) => record_check_error(outcome, path, error),
            }
        }

        drop(version_locks);
        let report = cleanup_report(self.outcomes, false);
        if self.assoc_changed && report.total_deleted_files > 0 {
            unsafe {
                SHChangeNotify(
                    SHCNE_ASSOCCHANGED as i32,
                    SHCNF_IDLIST,
                    std::ptr::null(),
                    std::ptr::null(),
                );
            }
        }
        report
    }
}

fn capture_stamp(path: &Path, directory: bool, access: u32) -> io::Result<TargetStamp> {
    if !is_local_drive_path(path) || has_reparse_point_in_ancestors(path, false) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unsafe cleanup target path",
        ));
    }
    TargetStamp::read(
        &open_for_verification(path, directory, access, 0)?,
        directory,
    )
}

fn verify_target(
    outcome: &mut RuleCleanOutcome,
    path: &Path,
    expected: &TargetStamp,
    directory: bool,
    access: u32,
    sharing: u32,
) -> Option<File> {
    let result = (|| {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
        let _parents = pin_directory(parent, false)?;
        let file = open_for_verification(path, directory, access, sharing)?;
        let actual = TargetStamp::read(&file, directory)?;
        Ok((file, &actual == expected))
    })();
    match result {
        Ok((file, true)) => Some(file),
        Ok((_, false)) => {
            outcome.review.get_or_insert_default().skipped_changed += 1;
            None
        }
        Err(error) => {
            record_check_error(outcome, path, error);
            None
        }
    }
}

fn record_check_error(outcome: &mut RuleCleanOutcome, path: &Path, error: io::Error) {
    record_check_error_parts(
        outcome,
        path,
        error.kind(),
        error.raw_os_error(),
        &error.to_string(),
    );
}

fn record_check_error_parts(
    outcome: &mut RuleCleanOutcome,
    path: &Path,
    kind: io::ErrorKind,
    raw_os_error: Option<i32>,
    message: &str,
) {
    match kind {
        io::ErrorKind::NotFound => outcome.review.get_or_insert_default().skipped_missing += 1,
        io::ErrorKind::InvalidData => outcome.review.get_or_insert_default().skipped_changed += 1,
        io::ErrorKind::PermissionDenied => outcome.skipped_protected_files += 1,
        _ if matches!(
            raw_os_error.map(|code| code as u32),
            Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
        ) =>
        {
            outcome.skipped_locked_files += 1;
        }
        _ => outcome.record_error(path.display(), message),
    }
}
