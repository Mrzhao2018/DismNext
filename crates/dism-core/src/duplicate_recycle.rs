use crate::diskmap::{invalidate_orphans_cache, is_protected_recycle_target};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use windows::core::PCWSTR;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{
    FileOperation, IFileOperation, IShellItem, SHCreateItemFromParsingName, FOFX_EARLYFAILURE,
    FOFX_RECYCLEONDELETE, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_NO_CONNECTED_ELEMENTS, FOF_SILENT,
};
use windows_sys::Win32::Storage::FileSystem::{
    FileIdInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ,
};

#[derive(Deserialize)]
pub struct DuplicateRecycleTarget {
    pub path: PathBuf,
    pub stamp: String,
}

#[derive(Debug, Serialize)]
pub struct DuplicateRecycleItem {
    pub path: PathBuf,
    #[serde(flatten)]
    pub outcome: DuplicateRecycleOutcome,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DuplicateRecycleOutcome {
    Recycled { size_bytes: u64 },
    Skipped { reason: DuplicateSkipReason },
    Failed { error: String },
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateSkipReason {
    KeeperUnavailable,
    Changed,
    Missing,
    SameFile,
    Protected,
    DifferentContent,
}

#[derive(PartialEq, Eq, Hash)]
struct FileIdentity {
    volume: u64,
    index: u128,
}

#[derive(PartialEq, Eq)]
pub(crate) struct FileState {
    identity: FileIdentity,
    pub(crate) modified: u64,
    pub(crate) size: u64,
}

impl FileState {
    pub(crate) fn read(file: &File) -> io::Result<Self> {
        let meta = file.metadata()?;
        if !meta.is_file() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Expected a regular file",
            ));
        }
        let mut info: FILE_ID_INFO = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                (&mut info as *mut FILE_ID_INFO).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            identity: FileIdentity {
                volume: info.VolumeSerialNumber,
                index: u128::from_le_bytes(info.FileId.Identifier),
            },
            modified: meta.last_write_time(),
            size: meta.len(),
        })
    }

    pub(crate) fn stamp(&self) -> String {
        format!(
            "{:016x}:{:032x}:{:016x}:{:016x}",
            self.identity.volume, self.identity.index, self.modified, self.size
        )
    }
}

fn open_verified(
    target: &DuplicateRecycleTarget,
    share_mode: u32,
) -> std::result::Result<(File, FileState), DuplicateRecycleOutcome> {
    if is_protected_recycle_target(&target.path) {
        return Err(skipped(DuplicateSkipReason::Protected));
    }
    let file = OpenOptions::new()
        .read(true)
        .share_mode(share_mode)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&target.path)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                skipped(DuplicateSkipReason::Missing)
            } else {
                failed(error)
            }
        })?;
    let state = FileState::read(&file).map_err(|error| {
        if error.kind() == io::ErrorKind::InvalidInput {
            skipped(DuplicateSkipReason::Protected)
        } else {
            failed(error)
        }
    })?;
    if state.stamp() != target.stamp {
        return Err(skipped(DuplicateSkipReason::Changed));
    }
    Ok((file, state))
}

fn skipped(reason: DuplicateSkipReason) -> DuplicateRecycleOutcome {
    DuplicateRecycleOutcome::Skipped { reason }
}

fn failed(error: impl ToString) -> DuplicateRecycleOutcome {
    DuplicateRecycleOutcome::Failed {
        error: error.to_string(),
    }
}

fn same_content(keeper: &mut File, candidate: &mut File, mut bytes: u64) -> io::Result<bool> {
    keeper.rewind()?;
    let mut left = [0u8; 64 * 1024];
    let mut right = [0u8; 64 * 1024];
    while bytes > 0 {
        let count = bytes.min(left.len() as u64) as usize;
        keeper.read_exact(&mut left[..count])?;
        candidate.read_exact(&mut right[..count])?;
        if left[..count] != right[..count] {
            return Ok(false);
        }
        bytes -= count as u64;
    }
    Ok(true)
}

pub fn recycle_duplicates(
    keeper: &DuplicateRecycleTarget,
    candidates: &[DuplicateRecycleTarget],
) -> Vec<DuplicateRecycleItem> {
    let results = recycle_duplicates_with(keeper, candidates, move_to_recycle_bin);
    if results
        .iter()
        .any(|item| matches!(item.outcome, DuplicateRecycleOutcome::Recycled { .. }))
    {
        invalidate_orphans_cache();
    }
    results
}

fn recycle_duplicates_with(
    keeper: &DuplicateRecycleTarget,
    candidates: &[DuplicateRecycleTarget],
    mut recycle: impl FnMut(&Path) -> Result<()>,
) -> Vec<DuplicateRecycleItem> {
    // Denying write and delete sharing keeps the reference available throughout the batch.
    let mut reference = open_verified(keeper, FILE_SHARE_READ).ok();
    let mut seen = HashSet::new();
    candidates
        .iter()
        .map(|target| {
            let outcome = (|| {
                let Some((reference_file, reference_state)) = reference.as_mut() else {
                    return Err(skipped(DuplicateSkipReason::KeeperUnavailable));
                };
                let (mut file, state) = open_verified(target, FILE_SHARE_READ | FILE_SHARE_DELETE)?;
                if state.identity == reference_state.identity || seen.contains(&state.identity) {
                    return Err(skipped(DuplicateSkipReason::SameFile));
                }
                if state.size != reference_state.size
                    || !same_content(reference_file, &mut file, state.size).map_err(failed)?
                {
                    return Err(skipped(DuplicateSkipReason::DifferentContent));
                }

                // The Shell API takes a path. Reopen it immediately before handing it off.
                if open_verified(keeper, FILE_SHARE_READ).is_err() {
                    reference = None;
                    return Err(skipped(DuplicateSkipReason::KeeperUnavailable));
                }
                let _target = open_verified(target, FILE_SHARE_READ | FILE_SHARE_DELETE)?;
                recycle(&target.path).map_err(failed)?;
                seen.insert(state.identity);
                Ok(DuplicateRecycleOutcome::Recycled {
                    size_bytes: state.size,
                })
            })()
            .unwrap_or_else(|outcome| outcome);
            DuplicateRecycleItem {
                path: target.path.clone(),
                outcome,
            }
        })
        .collect()
}

pub(crate) fn move_to_recycle_bin(path: &Path) -> Result<()> {
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() }
        }
    }

    let path = std::path::absolute(path)?;
    if is_protected_recycle_target(&path) {
        return Err(anyhow!("Refusing to recycle protected path '{}'", path.display()));
    }
    let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let _apartment = Apartment;
        let operation: IFileOperation =
            CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
        operation.SetOperationFlags(
            FOFX_RECYCLEONDELETE
                | FOFX_EARLYFAILURE
                | FOF_SILENT
                | FOF_NOERRORUI
                | FOF_NOCONFIRMATION
                | FOF_NO_CONNECTED_ELEMENTS,
        )?;
        let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None)?;
        operation.DeleteItem(&item, None)?;
        operation.PerformOperations()?;
        let moved = path
            .symlink_metadata()
            .is_err_and(|error| error.kind() == io::ErrorKind::NotFound);
        if operation.GetAnyOperationsAborted()?.as_bool() || !moved {
            return Err(anyhow!(
                "Windows did not complete the move to the Recycle Bin"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sandbox(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dismnext_recycle_{label}_{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        dir
    }

    fn target(path: PathBuf) -> DuplicateRecycleTarget {
        let stamp = FileState::read(&File::open(&path).unwrap()).unwrap().stamp();
        DuplicateRecycleTarget { path, stamp }
    }

    #[test]
    fn native_batch_recycles_the_unchanged_copy_and_keeps_the_modified_file() {
        let dir = sandbox("changed");
        for name in ["keep.bin", "changed.bin", "old.bin"] {
            fs::write(dir.join(name), b"identical contents").unwrap();
        }
        let keeper = target(dir.join("keep.bin"));
        let copies = [target(dir.join("changed.bin")), target(dir.join("old.bin"))];
        fs::write(&copies[0].path, b"edited after the scan").unwrap();

        let results = recycle_duplicates(&keeper, &copies);
        assert!(matches!(
            results[0].outcome,
            DuplicateRecycleOutcome::Skipped {
                reason: DuplicateSkipReason::Changed
            }
        ));
        assert!(
            matches!(
                results[1].outcome,
                DuplicateRecycleOutcome::Recycled { size_bytes: 18 }
            ),
            "{results:?}"
        );
        assert_eq!(fs::read(&copies[0].path).unwrap(), b"edited after the scan");
        assert_eq!(fs::read(&keeper.path).unwrap(), b"identical contents");
        assert!(!copies[1].path.exists());
        fs::remove_file(&copies[0].path).unwrap();
        fs::remove_file(&keeper.path).unwrap();
        fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn batch_reports_failures_without_moving_unverified_candidates() {
        let dir = sandbox("failure");
        for name in ["keep.bin", "first.bin", "error.bin"] {
            fs::write(dir.join(name), b"test").unwrap();
        }
        fs::write(dir.join("different.bin"), b"diff").unwrap();
        fs::hard_link(dir.join("keep.bin"), dir.join("alias.bin")).unwrap();
        let keeper = target(dir.join("keep.bin"));
        let candidates = ["first.bin", "error.bin", "different.bin", "alias.bin"]
            .map(|name| target(dir.join(name)));
        let mut attempted = Vec::new();
        let results = recycle_duplicates_with(&keeper, &candidates, |path| {
            attempted.push(path.to_path_buf());
            if path == candidates[1].path {
                return Err(anyhow!("simulated shell failure"));
            }
            Ok(())
        });
        assert_eq!(
            attempted,
            [candidates[0].path.clone(), candidates[1].path.clone()]
        );
        assert!(matches!(
            results[0].outcome,
            DuplicateRecycleOutcome::Recycled { .. }
        ));
        assert!(matches!(
            results[1].outcome,
            DuplicateRecycleOutcome::Failed { .. }
        ));
        assert!(matches!(
            results[2].outcome,
            DuplicateRecycleOutcome::Skipped {
                reason: DuplicateSkipReason::DifferentContent
            }
        ));
        assert!(matches!(
            results[3].outcome,
            DuplicateRecycleOutcome::Skipped {
                reason: DuplicateSkipReason::SameFile
            }
        ));

        fs::remove_file(&keeper.path).unwrap();
        let skipped =
            recycle_duplicates_with(&keeper, &candidates, |_| panic!("keeper is missing"));
        assert!(skipped.iter().all(|item| matches!(
            item.outcome,
            DuplicateRecycleOutcome::Skipped {
                reason: DuplicateSkipReason::KeeperUnavailable
            }
        )));
        for candidate in candidates {
            fs::remove_file(candidate.path).unwrap();
        }
        fs::remove_dir(&dir).unwrap();
    }
}
