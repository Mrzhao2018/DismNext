use crate::duplicate_recycle::FileIdentity;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::mem::{size_of, MaybeUninit};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;
use windows_sys::Win32::Storage::FileSystem::{
    FileCompressionInfo, FileStandardInfo, GetFileInformationByHandleEx,
    GetVolumeInformationByHandleW, FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_SPARSE_FILE,
    FILE_COMPRESSION_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
/// Allocation of ordinary files' default data streams within the scan root.
pub enum DiskAllocation {
    Measured {
        allocated_bytes: u64,
        hard_link_aliases: usize,
        unmeasured_files: usize,
        unreadable_entries: usize,
    },
    UnsupportedFileSystem,
    Unavailable,
}

#[derive(Default)]
pub(crate) struct AllocationScan {
    seen_linked_files: Mutex<HashSet<FileIdentity>>,
    bytes: AtomicU64,
    aliases: AtomicUsize,
    unmeasured: AtomicUsize,
    unreadable: AtomicUsize,
}

impl AllocationScan {
    pub(crate) fn for_path(path: &Path) -> Result<Self, DiskAllocation> {
        let file = open_for_attributes(path, FILE_FLAG_BACKUP_SEMANTICS)
            .map_err(|_| DiskAllocation::Unavailable)?;
        let mut fs_name = [0u16; 32];
        let ok = unsafe {
            GetVolumeInformationByHandleW(
                file.as_raw_handle(),
                null_mut(),
                0,
                null_mut(),
                null_mut(),
                null_mut(),
                fs_name.as_mut_ptr(),
                fs_name.len() as u32,
            )
        };
        if ok == 0 {
            return Err(DiskAllocation::Unavailable);
        }
        if !String::from_utf16_lossy(&fs_name)
            .trim_end_matches('\0')
            .eq_ignore_ascii_case("NTFS")
        {
            return Err(DiskAllocation::UnsupportedFileSystem);
        }
        Ok(Self::default())
    }

    pub(crate) fn record_file(&self, path: &Path, attributes: u32) -> Option<u64> {
        let result = self.measure_file(path, attributes);
        if result.is_none() {
            self.unmeasured.fetch_add(1, Relaxed);
        }
        result
    }

    fn measure_file(&self, path: &Path, attributes: u32) -> Option<u64> {
        let file = open_for_attributes(path, 0).ok()?;
        let mut standard = MaybeUninit::<FILE_STANDARD_INFO>::uninit();
        let ok = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileStandardInfo,
                standard.as_mut_ptr().cast(),
                size_of::<FILE_STANDARD_INFO>() as u32,
            )
        };
        if ok == 0 {
            return None;
        }
        let standard = unsafe { standard.assume_init() };
        if standard.Directory != 0 || standard.DeletePending != 0 || standard.NumberOfLinks == 0 {
            return None;
        }

        let bytes = if attributes & (FILE_ATTRIBUTE_COMPRESSED | FILE_ATTRIBUTE_SPARSE_FILE) != 0 {
            compressed_allocation(&file)?
        } else {
            standard.AllocationSize.try_into().ok()?
        };

        let alias = if standard.NumberOfLinks > 1 {
            let identity = FileIdentity::read(&file).ok()?;
            !self.seen_linked_files.lock().unwrap().insert(identity)
        } else {
            false
        };
        if alias {
            self.aliases.fetch_add(1, Relaxed);
        } else {
            self.bytes.fetch_add(bytes, Relaxed);
        }
        Some(bytes)
    }

    pub(crate) fn record_unreadable(&self) {
        self.unreadable.fetch_add(1, Relaxed);
    }

    pub(crate) fn summary(&self) -> DiskAllocation {
        DiskAllocation::Measured {
            allocated_bytes: self.bytes.load(Relaxed),
            hard_link_aliases: self.aliases.load(Relaxed),
            unmeasured_files: self.unmeasured.load(Relaxed),
            unreadable_entries: self.unreadable.load(Relaxed),
        }
    }
}

fn open_for_attributes(path: &Path, extra_flags: u32) -> std::io::Result<File> {
    OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | extra_flags)
        .open(path)
}

fn compressed_allocation(file: &File) -> Option<u64> {
    let mut info = MaybeUninit::<FILE_COMPRESSION_INFO>::uninit();
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileCompressionInfo,
            info.as_mut_ptr().cast(),
            size_of::<FILE_COMPRESSION_INFO>() as u32,
        )
    };
    if ok == 0 {
        return None;
    }
    unsafe { info.assume_init() }
        .CompressedFileSize
        .try_into()
        .ok()
}
