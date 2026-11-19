use super::is_broad_program_root;
use crate::diskmap::{invalidate_orphans_cache, is_protected_recycle_target};
use crate::duplicate_recycle::move_to_recycle_bin;
use crate::file_stamp::{open_for_verification, TargetStamp};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::hash::{BuildHasher, Hash, Hasher, RandomState};
use std::io;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use walkdir::WalkDir;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LeftoverReview {
    Ready { stamp: String },
    Blocked { reason: LeftoverSkipReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeftoverSkipReason {
    Changed,
    Missing,
    Protected,
    Unverifiable,
}

#[derive(Deserialize)]
pub struct LeftoverRecycleTarget {
    pub path: PathBuf,
    pub stamp: String,
}

#[derive(Debug, Serialize)]
pub struct LeftoverRecycleItem {
    pub path: PathBuf,
    #[serde(flatten)]
    pub outcome: LeftoverRecycleOutcome,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LeftoverRecycleOutcome {
    Recycled { size_bytes: u64 },
    Skipped { reason: LeftoverSkipReason },
    Failed { error: String },
}

pub(super) struct FolderSnapshot {
    path: PathBuf,
    pub stamp: String,
    pub size_bytes: u64,
    pub file_count: usize,
}

static STAMP_KEYS: OnceLock<RandomState> = OnceLock::new();
const SHARING: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

impl FolderSnapshot {
    pub(super) fn capture(path: &Path) -> Result<Self, LeftoverSkipReason> {
        let path = std::path::absolute(path).map_err(|_| LeftoverSkipReason::Unverifiable)?;
        if path.to_str().is_none() {
            return Err(LeftoverSkipReason::Unverifiable);
        }
        check_route(&path)?;
        let root = directory_revision(&path).map_err(root_error)?;
        let mut stamp = STAMP_KEYS.get_or_init(RandomState::new).build_hasher();
        path.hash(&mut stamp);
        root.hash(&mut stamp);
        let mut checked_dirs = vec![(path.clone(), root)];
        let mut size_bytes = 0u64;
        let mut file_count = 0;

        // Require a complete enumeration. An unreadable subtree can hide unreviewed files.
        for entry in WalkDir::new(&path)
            .follow_links(false)
            .follow_root_links(false)
            .sort_by_file_name()
        {
            let entry = entry.map_err(|_| LeftoverSkipReason::Unverifiable)?;
            if entry.depth() == 0 {
                continue;
            }
            let file_type = entry.file_type();
            if !file_type.is_dir() && !file_type.is_file() {
                return Err(LeftoverSkipReason::Protected);
            }
            let relative = entry
                .path()
                .strip_prefix(&path)
                .map_err(|_| LeftoverSkipReason::Unverifiable)?;
            relative.hash(&mut stamp);
            if file_type.is_dir() {
                let revision = directory_revision(entry.path()).map_err(entry_error)?;
                revision.hash(&mut stamp);
                checked_dirs.push((entry.into_path(), revision));
            } else {
                let handle =
                    open_for_verification(entry.path(), false, 0, SHARING).map_err(entry_error)?;
                let revision = TargetStamp::read(&handle, false).map_err(entry_error)?;
                revision.hash(&mut stamp);
                size_bytes = size_bytes.saturating_add(revision.file_size().unwrap());
                file_count += 1;
            }
        }

        check_route(&path)?;
        // Detect additions to directories whose entries were already enumerated.
        for (dir, previous) in checked_dirs.into_iter().rev() {
            if directory_revision(&dir).map_err(entry_error)? != previous {
                return Err(LeftoverSkipReason::Changed);
            }
        }
        Ok(Self {
            path,
            stamp: format!("{:016x}", stamp.finish()),
            size_bytes,
            file_count,
        })
    }
}

fn check_route(path: &Path) -> Result<(), LeftoverSkipReason> {
    if path.parent().is_none() || is_protected_recycle_target(path) || is_broad_program_root(path) {
        return Err(LeftoverSkipReason::Protected);
    }
    for part in path.ancestors() {
        let metadata = part.symlink_metadata().map_err(root_error)?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(LeftoverSkipReason::Protected);
        }
    }
    Ok(())
}

fn root_error(error: io::Error) -> LeftoverSkipReason {
    match error.kind() {
        io::ErrorKind::NotFound => LeftoverSkipReason::Missing,
        io::ErrorKind::InvalidData => LeftoverSkipReason::Protected,
        _ => LeftoverSkipReason::Unverifiable,
    }
}

fn entry_error(error: io::Error) -> LeftoverSkipReason {
    if error.kind() == io::ErrorKind::NotFound {
        LeftoverSkipReason::Changed
    } else {
        root_error(error)
    }
}

fn directory_revision(path: &Path) -> io::Result<(TargetStamp, u64)> {
    let handle = open_for_verification(path, true, 0, SHARING)?;
    Ok((
        TargetStamp::read(&handle, true)?,
        handle.metadata()?.last_write_time(),
    ))
}

pub fn recycle_app_leftovers(targets: &[LeftoverRecycleTarget]) -> Vec<LeftoverRecycleItem> {
    let results = recycle_with(targets, move_to_recycle_bin);
    if results
        .iter()
        .any(|item| matches!(item.outcome, LeftoverRecycleOutcome::Recycled { .. }))
    {
        invalidate_orphans_cache();
    }
    results
}

fn recycle_with(
    targets: &[LeftoverRecycleTarget],
    mut recycle: impl FnMut(&Path) -> anyhow::Result<()>,
) -> Vec<LeftoverRecycleItem> {
    let mut seen = HashSet::new();
    targets
        .iter()
        .filter(|target| seen.insert(&target.path))
        .map(|target| {
            let outcome = match FolderSnapshot::capture(&target.path) {
                Err(reason) => LeftoverRecycleOutcome::Skipped { reason },
                Ok(snapshot) if snapshot.stamp != target.stamp => LeftoverRecycleOutcome::Skipped {
                    reason: LeftoverSkipReason::Changed,
                },
                Ok(snapshot) => match recycle(&snapshot.path) {
                    Ok(()) => LeftoverRecycleOutcome::Recycled {
                        size_bytes: snapshot.size_bytes,
                    },
                    Err(error) => LeftoverRecycleOutcome::Failed {
                        error: error.to_string(),
                    },
                },
            };
            LeftoverRecycleItem {
                path: target.path.clone(),
                outcome,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
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
            let snapshot = FolderSnapshot::capture(&path).unwrap();
            assert_eq!(snapshot.file_count, 1);
            assert_eq!(snapshot.size_bytes, 3);
            targets.push(LeftoverRecycleTarget {
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

        let results = recycle_with(&targets, |path| {
            let name = path.file_name().unwrap();
            anyhow::ensure!(name != "fails", "simulated Shell error");
            fs::rename(path, bin.join(name))?;
            Ok(())
        });
        assert_eq!(results.len(), targets.len());
        assert!(matches!(
            results[0].outcome,
            LeftoverRecycleOutcome::Recycled { size_bytes: 3 }
        ));
        for result in [&results[1], &results[2]] {
            assert!(matches!(
                result.outcome,
                LeftoverRecycleOutcome::Skipped {
                    reason: LeftoverSkipReason::Changed
                }
            ));
        }
        assert!(
            matches!(&results[3].outcome, LeftoverRecycleOutcome::Failed { error } if error == "simulated Shell error")
        );
        assert!(matches!(
            results[4].outcome,
            LeftoverRecycleOutcome::Recycled { size_bytes: 3 }
        ));
        assert!(matches!(
            results[5].outcome,
            LeftoverRecycleOutcome::Skipped {
                reason: LeftoverSkipReason::Missing
            }
        ));
        assert!(candidates.join("grows/subdir/new.txt").exists());
        assert!(candidates.join("replaced/subdir/sample.txt").exists());
        assert!(renamed.join("subdir/sample.txt").exists());
        assert!(candidates.join("fails/subdir/sample.txt").exists());
        assert!(bin.join("first/subdir/sample.txt").exists());
        assert!(bin.join("last/subdir/sample.txt").exists());

        let retry = recycle_with(&targets[3..4], |path| {
            fs::rename(path, bin.join("fails"))?;
            Ok(())
        });
        assert!(matches!(
            retry[0].outcome,
            LeftoverRecycleOutcome::Recycled { size_bytes: 3 }
        ));
        fs::remove_dir_all(scratch).unwrap();
    }
}
