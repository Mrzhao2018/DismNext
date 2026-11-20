use super::{
    cleanup_report, diagnose_file, driver_target_label, old_enough, plan_cleanup, CleanupAction,
    CleanupExecutionReport, CleanupPath, CleanupRule, FileStatus, PlannedTarget, RuleCleanOutcome,
    SystemInfo, TargetKey,
};
use crate::drivers::compute_drivers_uncached;
use crate::duplicate_recycle::move_to_recycle_bin;
use crate::file_stamp::{delete_open_target, open_for_verification, TargetStamp};
use crate::systasks::compute_orphaned_installer_files_uncached;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io;
use std::path::Path;
use windows_sys::Win32::Foundation::{
    ERROR_DIR_NOT_EMPTY, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_READ_DATA, FILE_SHARE_DELETE, FILE_SHARE_READ,
};

/// Desktop cleanup plan. Execution consumes the plan and only considers its captured targets.
pub struct ReviewedCleanup {
    outcomes: Vec<RuleCleanOutcome>,
    files: Vec<(PlannedTarget, TargetStamp)>,
    directories: Vec<(usize, CleanupPath, TargetStamp)>,
}

impl ReviewedCleanup {
    pub fn prepare(
        rules: &[CleanupRule],
        rule_ids: &[String],
        sys: &SystemInfo,
    ) -> (Self, CleanupExecutionReport) {
        let wanted = rule_ids.iter().map(String::as_str).collect();
        let (mut outcomes, targets, directories) = plan_cleanup(rules, &wanted, sys, true);
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
        };
        (plan, cleanup_report(preview, true))
    }

    pub fn execute(mut self) -> CleanupExecutionReport {
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

        for (target, stamp) in self.files {
            let outcome = &mut self.outcomes[target.owner];
            let path = &target.target.path;
            let Ok(_version_lock) = target.target.lock_version() else {
                outcome.review.get_or_insert_default().skipped_changed += 1;
                continue;
            };
            let (access, sharing) = match &target.action {
                CleanupAction::Delete => (DELETE, 0),
                // The Shell and PnPUtil need to be able to act on the file by name.
                _ => (FILE_READ_DATA, FILE_SHARE_READ | FILE_SHARE_DELETE),
            };
            if matches!(target.action, CleanupAction::Recycle) {
                let (status, _) = diagnose_file(path, &target.path);
                if status == FileStatus::Locked {
                    outcome.skipped_locked_files += 1;
                    continue;
                }
            }
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
            let result = match &target.action {
                CleanupAction::Delete => delete_open_target(&file, false).map_err(Into::into),
                CleanupAction::Recycle if installer_targets.contains(&TargetKey::file(path)) => {
                    move_to_recycle_bin(path)
                }
                CleanupAction::RemoveDriver(name)
                    if driver_targets.get(name) == Some(&target.path) =>
                {
                    crate::drivers::delete_driver(name).map(|_| ())
                }
                _ => {
                    outcome.review.get_or_insert_default().skipped_changed += 1;
                    continue;
                }
            };
            drop(file);
            outcome.record_target(&target, result.map(|()| (FileStatus::Ready, target.size)));
        }

        self.directories
            .sort_by_key(|(_, dir, _)| Reverse(dir.path.components().count()));
        for (owner, directory, stamp) in self.directories {
            let outcome = &mut self.outcomes[owner];
            let Ok(_version_lock) = directory.lock_version() else {
                outcome.review.get_or_insert_default().skipped_changed += 1;
                continue;
            };
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
        cleanup_report(self.outcomes, false)
    }
}

fn capture_stamp(path: &Path, directory: bool, access: u32) -> io::Result<TargetStamp> {
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
    let result = open_for_verification(path, directory, access, sharing).and_then(|file| {
        let actual = TargetStamp::read(&file, directory)?;
        Ok((file, &actual == expected))
    });
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
    match error.kind() {
        io::ErrorKind::NotFound => outcome.review.get_or_insert_default().skipped_missing += 1,
        io::ErrorKind::InvalidData => outcome.review.get_or_insert_default().skipped_changed += 1,
        io::ErrorKind::PermissionDenied => outcome.skipped_protected_files += 1,
        _ if matches!(
            error.raw_os_error().map(|code| code as u32),
            Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
        ) =>
        {
            outcome.skipped_locked_files += 1;
        }
        _ => outcome.record_error(path.display(), error),
    }
}
