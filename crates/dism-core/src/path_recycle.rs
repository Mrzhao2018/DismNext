use crate::diskmap::{invalidate_orphans_cache, is_protected_recycle_target};
use crate::recycle_bin::{move_batch, move_to_recycle_bin, RecycleRequest, Verified};
use crate::file_stamp::{open_for_verification, read_basic_info, TargetStamp};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::hash::{BuildHasher, Hash, Hasher, RandomState};
use std::io;
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use walkdir::WalkDir;
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use windows_sys::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RecyclePreview {
    Ready {
        stamp: String,
        size_bytes: u64,
        file_count: usize,
        dir_count: usize,
    },
    Blocked {
        reason: RecycleSkipReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecycleSkipReason {
    Changed,
    Missing,
    Protected,
    Unverifiable,
}

#[derive(Clone, Deserialize)]
pub struct RecycleTarget {
    pub path: PathBuf,
    pub stamp: String,
}

#[derive(Debug, Serialize)]
pub struct RecycleItem {
    pub path: PathBuf,
    #[serde(flatten)]
    pub outcome: RecycleOutcome,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RecycleOutcome {
    Recycled {
        size_bytes: u64,
        file_count: usize,
        dir_count: usize,
    },
    Skipped {
        reason: RecycleSkipReason,
    },
    Failed {
        error: String,
    },
}

pub(crate) struct PathSnapshot {
    path: PathBuf,
    pub stamp: String,
    pub size_bytes: u64,
    pub file_count: usize,
    pub dir_count: usize,
    latest_change: i64,
}

static STAMP_KEYS: OnceLock<RandomState> = OnceLock::new();
const SHARING: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

// FILETIME's 100 ns ticks exceed the range of exact JavaScript integers.
pub(crate) fn scan_timestamp() -> String {
    let mut time = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    unsafe { GetSystemTimePreciseAsFileTime(&mut time) };
    ((u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)).to_string()
}

pub fn review_recycle_path(path: &Path, is_dir: bool, scan_started_at: i64) -> RecyclePreview {
    match PathSnapshot::capture(path, Some(is_dir)) {
        Ok(snapshot) if snapshot.latest_change <= scan_started_at => RecyclePreview::Ready {
            stamp: snapshot.stamp,
            size_bytes: snapshot.size_bytes,
            file_count: snapshot.file_count,
            dir_count: snapshot.dir_count,
        },
        Ok(_) => {
            invalidate_orphans_cache();
            RecyclePreview::Blocked {
                reason: RecycleSkipReason::Changed,
            }
        }
        Err(reason) => {
            if matches!(
                reason,
                RecycleSkipReason::Changed | RecycleSkipReason::Missing
            ) {
                invalidate_orphans_cache();
            }
            RecyclePreview::Blocked { reason }
        }
    }
}

pub fn recycle_reviewed_path(target: &RecycleTarget) -> RecycleItem {
    recycle_paths(std::slice::from_ref(target), None).pop().unwrap()
}

pub(crate) fn recycle_path_immediately(path: &Path) -> anyhow::Result<u64> {
    let snapshot = PathSnapshot::capture(path, None).map_err(|reason| {
        anyhow::anyhow!(
            "Cannot verify recycle target '{}': {reason:?}",
            path.display()
        )
    })?;
    move_to_recycle_bin(&snapshot.path)?;
    invalidate_orphans_cache();
    Ok(snapshot.size_bytes)
}

impl PathSnapshot {
    pub(crate) fn capture(
        path: &Path,
        expected_dir: Option<bool>,
    ) -> Result<Self, RecycleSkipReason> {
        let path = std::path::absolute(path).map_err(|_| RecycleSkipReason::Unverifiable)?;
        if path.to_str().is_none() {
            return Err(RecycleSkipReason::Unverifiable);
        }
        let is_dir = check_route(&path)?;
        if expected_dir.is_some_and(|expected| expected != is_dir) {
            return Err(RecycleSkipReason::Changed);
        }
        let mut stamp = STAMP_KEYS.get_or_init(RandomState::new).build_hasher();
        path.hash(&mut stamp);
        if !is_dir {
            let revision = file_revision(&path).map_err(root_error)?;
            check_route(&path)?;
            if file_revision(&path).map_err(root_error)? != revision {
                return Err(RecycleSkipReason::Changed);
            }
            revision.hash(&mut stamp);
            return Ok(Self {
                path,
                stamp: format!("{:016x}", stamp.finish()),
                size_bytes: revision.file_size().unwrap(),
                file_count: 1,
                dir_count: 0,
                latest_change: revision.latest_file_change(),
            });
        }

        let root = directory_revision(&path).map_err(root_error)?;
        root.hash(&mut stamp);
        let mut latest_change = root.1;
        let mut checked_dirs = vec![(path.clone(), root)];
        let mut size_bytes = 0u64;
        let mut file_count = 0;

        // Require a complete enumeration. An unreadable subtree can hide unreviewed files.
        for entry in WalkDir::new(&path)
            .follow_links(false)
            .follow_root_links(false)
            .sort_by_file_name()
        {
            let entry = entry.map_err(|_| RecycleSkipReason::Unverifiable)?;
            if entry.depth() == 0 {
                continue;
            }
            let file_type = entry.file_type();
            if !file_type.is_dir() && !file_type.is_file() {
                return Err(RecycleSkipReason::Protected);
            }
            let relative = entry
                .path()
                .strip_prefix(&path)
                .map_err(|_| RecycleSkipReason::Unverifiable)?;
            relative.hash(&mut stamp);
            if file_type.is_dir() {
                let revision = directory_revision(entry.path()).map_err(entry_error)?;
                latest_change = latest_change.max(revision.1);
                revision.hash(&mut stamp);
                checked_dirs.push((entry.into_path(), revision));
            } else {
                let revision = file_revision(entry.path()).map_err(entry_error)?;
                latest_change = latest_change.max(revision.latest_file_change());
                revision.hash(&mut stamp);
                size_bytes = size_bytes.saturating_add(revision.file_size().unwrap());
                file_count += 1;
            }
        }

        check_route(&path)?;
        // Detect additions to directories whose entries were already enumerated.
        let dir_count = checked_dirs.len();
        for (dir, previous) in checked_dirs.into_iter().rev() {
            if directory_revision(&dir).map_err(entry_error)? != previous {
                return Err(RecycleSkipReason::Changed);
            }
        }
        Ok(Self {
            path,
            stamp: format!("{:016x}", stamp.finish()),
            size_bytes,
            file_count,
            dir_count,
            latest_change,
        })
    }
}

fn check_route(path: &Path) -> Result<bool, RecycleSkipReason> {
    if path.parent().is_none()
        || is_protected_recycle_target(path)
        || path.components().any(|component| {
            matches!(component,
            Component::Normal(name) if name.eq_ignore_ascii_case("$Recycle.Bin"))
        })
    {
        return Err(RecycleSkipReason::Protected);
    }
    let target = path.symlink_metadata().map_err(root_error)?;
    if (!target.is_dir() && !target.is_file())
        || target.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(RecycleSkipReason::Protected);
    }
    for part in path.ancestors().skip(1) {
        let metadata = part.symlink_metadata().map_err(root_error)?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(RecycleSkipReason::Protected);
        }
    }
    Ok(target.is_dir())
}

fn root_error(error: io::Error) -> RecycleSkipReason {
    match error.kind() {
        io::ErrorKind::NotFound => RecycleSkipReason::Missing,
        io::ErrorKind::InvalidData => RecycleSkipReason::Protected,
        _ => RecycleSkipReason::Unverifiable,
    }
}

fn entry_error(error: io::Error) -> RecycleSkipReason {
    if error.kind() == io::ErrorKind::NotFound {
        RecycleSkipReason::Changed
    } else {
        root_error(error)
    }
}

fn directory_revision(path: &Path) -> io::Result<(TargetStamp, i64)> {
    let handle = open_for_verification(path, true, 0, SHARING)?;
    let info = read_basic_info(&handle)?;
    Ok((
        TargetStamp::read(&handle, true)?,
        if info.ChangeTime > 0 {
            info.ChangeTime
        } else {
            info.LastWriteTime.max(info.CreationTime)
        },
    ))
}

fn file_revision(path: &Path) -> io::Result<TargetStamp> {
    let handle = open_for_verification(path, false, 0, SHARING)?;
    TargetStamp::read(&handle, false)
}

pub(crate) fn recycle_paths(
    targets: &[RecycleTarget],
    expected_dir: Option<bool>,
) -> Vec<RecycleItem> {
    let results = recycle_with(targets, expected_dir, move_batch);
    if results
        .iter()
        .any(|item| matches!(item.outcome, RecycleOutcome::Recycled { .. }))
    {
        invalidate_orphans_cache();
    }
    results
}

#[derive(Debug)]
struct VerificationError(RecycleSkipReason);

impl std::fmt::Display for VerificationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Could not verify recycle target: {:?}", self.0)
    }
}

impl std::error::Error for VerificationError {}

fn recycle_with(
    targets: &[RecycleTarget],
    expected_dir: Option<bool>,
    recycle: impl FnOnce(Vec<RecycleRequest<PathSnapshot>>) -> Vec<anyhow::Result<PathSnapshot>>,
) -> Vec<RecycleItem> {
    let mut seen = HashSet::new();
    let selected: Vec<_> = targets.iter().filter(|target| seen.insert(&target.path)).collect();
    let requests = selected.iter().map(|target| {
        let target = (*target).clone();
        RecycleRequest {
            path: target.path.clone(),
            verify: Box::new(move || {
                let snapshot = PathSnapshot::capture(&target.path, expected_dir).map_err(VerificationError)?;
                if snapshot.stamp != target.stamp {
                    return Err(VerificationError(RecycleSkipReason::Changed).into());
                }
                Ok(Verified { value: snapshot, hold: None })
            }),
        }
    }).collect();
    selected.into_iter().zip(recycle(requests)).map(|(target, result)| {
        let outcome = match result {
            Ok(snapshot) => RecycleOutcome::Recycled {
                size_bytes: snapshot.size_bytes,
                file_count: snapshot.file_count,
                dir_count: snapshot.dir_count,
            },
            Err(error) => match error.downcast::<VerificationError>() {
                Ok(verification) => RecycleOutcome::Skipped { reason: verification.0 },
                Err(error) => RecycleOutcome::Failed { error: error.to_string() },
            },
        };
        RecycleItem { path: target.path.clone(), outcome }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recycle_bin::test_move_batch;

    fn recycle_test(targets: &[RecycleTarget], expected_dir: Option<bool>, recycle: impl FnMut(&Path) -> anyhow::Result<()>) -> Vec<RecycleItem> {
        recycle_with(targets, expected_dir, |requests| test_move_batch(requests, recycle))
    }
    use std::fs;

    #[test]
    fn leftover_batch_rechecks_contents_and_continues_after_failure() {
        let scratch =
            std::env::temp_dir().join(format!("dismnext_leftover_review_{}", std::process::id()));
        let candidates = scratch.join("candidates");
        let bin = scratch.join("mock-bin");
        fs::create_dir_all(&bin).unwrap();
        let mut targets = Vec::new();
        for name in ["first", "grows", "replaced", "fails", "last", "moves"] {
            let path = candidates.join(name);
            fs::create_dir_all(path.join("subdir")).unwrap();
            fs::write(path.join("subdir/sample.txt"), b"old").unwrap();
            let snapshot = PathSnapshot::capture(&path, Some(true)).unwrap();
            assert_eq!(snapshot.file_count, 1);
            assert_eq!(snapshot.size_bytes, 3);
            targets.push(RecycleTarget {
                path,
                stamp: snapshot.stamp,
            });
        }

        fs::write(candidates.join("grows/subdir/new.txt"), b"new").unwrap();
        let renamed = scratch.join("old-directory");
        fs::rename(candidates.join("replaced"), &renamed).unwrap();
        fs::create_dir_all(candidates.join("replaced/subdir")).unwrap();
        fs::write(candidates.join("replaced/subdir/sample.txt"), b"old").unwrap();
        fs::rename(candidates.join("moves"), scratch.join("moved-directory")).unwrap();

        let results = recycle_test(&targets, Some(true), |path| {
            let name = path.file_name().unwrap();
            anyhow::ensure!(name != "fails", "simulated Shell error");
            fs::rename(path, bin.join(name))?;
            Ok(())
        });
        assert_eq!(results.len(), targets.len());
        assert!(matches!(
            results[0].outcome,
            RecycleOutcome::Recycled {
                size_bytes: 3,
                file_count: 1,
                dir_count: 2
            }
        ));
        for result in [&results[1], &results[2]] {
            assert!(matches!(
                result.outcome,
                RecycleOutcome::Skipped {
                    reason: RecycleSkipReason::Changed
                }
            ));
        }
        assert!(
            matches!(&results[3].outcome, RecycleOutcome::Failed { error } if error == "simulated Shell error")
        );
        assert!(matches!(
            results[4].outcome,
            RecycleOutcome::Recycled { size_bytes: 3, .. }
        ));
        assert!(matches!(
            results[5].outcome,
            RecycleOutcome::Skipped {
                reason: RecycleSkipReason::Missing
            }
        ));
        assert!(candidates.join("grows/subdir/new.txt").exists());
        assert!(candidates.join("replaced/subdir/sample.txt").exists());
        assert!(renamed.join("subdir/sample.txt").exists());
        assert!(candidates.join("fails/subdir/sample.txt").exists());
        assert!(bin.join("first/subdir/sample.txt").exists());
        assert!(bin.join("last/subdir/sample.txt").exists());

        let retry = recycle_test(&targets[3..4], Some(true), |path| {
            fs::rename(path, bin.join("fails"))?;
            Ok(())
        });
        assert!(matches!(
            retry[0].outcome,
            RecycleOutcome::Recycled { size_bytes: 3, .. }
        ));
        fs::remove_dir_all(scratch).unwrap();
    }

    #[test]
    fn disk_recycle_rejects_replaced_files_and_changed_directories() {
        let scratch =
            std::env::temp_dir().join(format!("dismnext_disk_recycle_{}", std::process::id()));
        let folder = scratch.join("nested");
        let file = scratch.join("sample.tmp");
        let changed = scratch.join("changed.tmp");
        fs::create_dir_all(&folder).unwrap();
        fs::write(&file, b"old").unwrap();
        fs::write(&changed, b"old").unwrap();
        fs::write(folder.join("before.tmp"), b"old").unwrap();
        let cutoff = scan_timestamp().parse().unwrap();

        let RecyclePreview::Ready {
            stamp: file_stamp,
            size_bytes: 3,
            ..
        } = review_recycle_path(&file, false, cutoff)
        else {
            panic!("unchanged file should be ready for confirmation");
        };
        let RecyclePreview::Ready {
            stamp: folder_stamp,
            file_count: 1,
            ..
        } = review_recycle_path(&folder, true, cutoff)
        else {
            panic!("unchanged directory should be ready for confirmation");
        };

        fs::rename(&file, scratch.join("original.tmp")).unwrap();
        fs::write(&file, b"new").unwrap();
        fs::write(folder.join("after.tmp"), b"new").unwrap();
        let old_write_time = fs::metadata(&changed).unwrap().modified().unwrap();
        fs::write(&changed, b"new").unwrap();
        fs::File::options()
            .write(true)
            .open(&changed)
            .unwrap()
            .set_modified(old_write_time)
            .unwrap();
        for (path, directory) in [(&file, false), (&folder, true), (&changed, false)] {
            assert!(matches!(
                review_recycle_path(path, directory, cutoff),
                RecyclePreview::Blocked {
                    reason: RecycleSkipReason::Changed
                }
            ));
        }

        for target in [
            RecycleTarget {
                path: file.clone(),
                stamp: file_stamp,
            },
            RecycleTarget {
                path: folder.clone(),
                stamp: folder_stamp,
            },
        ] {
            let item = recycle_test(&[target], None, |_| {
                panic!("changed targets must not reach Shell")
            }).pop().unwrap();
            assert!(matches!(
                item.outcome,
                RecycleOutcome::Skipped {
                    reason: RecycleSkipReason::Changed
                }
            ));
        }
        assert_eq!(fs::read(&file).unwrap(), b"new");
        assert_eq!(fs::read(folder.join("after.tmp")).unwrap(), b"new");
        assert_eq!(fs::read(scratch.join("original.tmp")).unwrap(), b"old");
        fs::remove_dir_all(scratch).unwrap();
    }
}
