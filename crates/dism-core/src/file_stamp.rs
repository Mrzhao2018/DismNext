use crate::duplicate_recycle::FileIdentity;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use windows_sys::Win32::Storage::FileSystem::{
    FileBasicInfo, FileDispositionInfoEx, GetFileInformationByHandleEx, SetFileInformationByHandle,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FILE_DISPOSITION_FLAG_DELETE,
    FILE_DISPOSITION_FLAG_FORCE_IMAGE_SECTION_CHECK,
    FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    FILE_DISPOSITION_INFO_EX, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_READ_ATTRIBUTES,
};

pub(crate) fn open_for_verification(
    path: &Path,
    directory: bool,
    access: u32,
    sharing: u32,
) -> io::Result<File> {
    let flags = FILE_FLAG_OPEN_REPARSE_POINT
        | if directory {
            FILE_FLAG_BACKUP_SEMANTICS
        } else {
            0
        };
    OpenOptions::new()
        .access_mode(access | FILE_READ_ATTRIBUTES)
        .share_mode(sharing)
        .custom_flags(flags)
        .open(path)
}

#[derive(PartialEq, Eq, Hash)]
pub(crate) struct TargetStamp {
    identity: FileIdentity,
    created: u64,
    contents: Option<FileRevision>,
}

#[derive(PartialEq, Eq, Hash)]
struct FileRevision {
    size: u64,
    written: i64,
    changed: i64,
}

impl TargetStamp {
    pub(crate) fn read(file: &File, directory: bool) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || (directory && !metadata.is_dir())
            || (!directory && !metadata.is_file())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Target type changed",
            ));
        }
        let contents = if directory {
            None
        } else {
            let info = read_basic_info(file)?;
            Some(FileRevision {
                size: metadata.len(),
                written: info.LastWriteTime,
                changed: info.ChangeTime,
            })
        };
        Ok(Self {
            identity: FileIdentity::read(file)?,
            created: metadata.creation_time(),
            contents,
        })
    }

    pub(crate) fn file_size(&self) -> Option<u64> {
        self.contents.as_ref().map(|revision| revision.size)
    }

    pub(crate) fn latest_file_change(&self) -> i64 {
        self.contents.as_ref().map_or(0, |revision| {
            if revision.changed > 0 {
                revision.changed
            } else {
                revision.written.max(self.created as i64)
            }
        })
    }
}

pub(crate) fn read_basic_info(file: &File) -> io::Result<FILE_BASIC_INFO> {
    let mut info: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            (&mut info as *mut FILE_BASIC_INFO).cast(),
            std::mem::size_of_val(&info) as u32,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(info)
    }
}

pub(crate) fn delete_open_target(file: &File, directory: bool) -> io::Result<()> {
    let mut flags = FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS;
    if !directory {
        flags |= FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE
            | FILE_DISPOSITION_FLAG_FORCE_IMAGE_SECTION_CHECK;
    }
    let disposition = FILE_DISPOSITION_INFO_EX { Flags: flags };
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfoEx,
            (&disposition as *const FILE_DISPOSITION_INFO_EX).cast(),
            std::mem::size_of_val(&disposition) as u32,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
