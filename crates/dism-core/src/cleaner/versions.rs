use super::review::TargetStamp;
use super::{
    expand_expression, resolve_wildcard_roots_until, walk_dir_filtered, CleanupPath,
    CollectedFiles, SystemInfo, TargetKey,
};
use crate::evaluator::read_file_version_uncached;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
};

pub(super) struct VersionGuard {
    executable: PathBuf,
    active_directory: PathBuf,
    state: TargetStamp,
}

impl VersionGuard {
    fn read(root: &Path, version_file: &str) -> Option<(Self, [u16; 4])> {
        let executable = root.join(version_file);
        let file = open_version_file(&executable)?;
        let state = TargetStamp::read(&file, false).ok()?;
        let version_text = read_file_version_uncached(&executable)?;
        let version = parse_version(&version_text)?;
        let active_directory = root.join(version_text);
        is_real_directory(&active_directory).then_some((
            Self {
                executable,
                active_directory,
                state,
            },
            version,
        ))
    }

    pub(super) fn lock(&self) -> Option<File> {
        let file = open_version_file(&self.executable)?;
        (TargetStamp::read(&file, false).ok()? == self.state
            && is_real_directory(&self.active_directory))
        .then_some(file)
    }
}

fn is_real_directory(path: &Path) -> bool {
    path.symlink_metadata().is_ok_and(|metadata| {
        metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
    })
}

fn open_version_file(path: &Path) -> Option<File> {
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()
}

fn parse_version(text: &str) -> Option<[u16; 4]> {
    let mut parts = text.split('.');
    let mut version = [0; 4];
    for value in &mut version {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *value = part.parse().ok()?;
    }
    (parts.next().is_none()).then_some(version)
}

pub(super) fn collect_versioned_directories(
    root_path: &str,
    version_file: &str,
    sys: &SystemInfo,
    files: &mut CollectedFiles<'_>,
    seen_paths: &mut HashSet<TargetKey>,
    removable_dirs: &mut Vec<CleanupPath>,
    probe_budget: &mut usize,
) {
    let mut components = Path::new(version_file).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return;
    }
    let Some(expanded) = expand_expression(root_path, None) else {
        return;
    };
    let roots = resolve_wildcard_roots_until(&expanded, &sys.fixed_drives, &|| files.cancelled());
    for root in roots {
        if files.cancelled() {
            return;
        }
        if !is_real_directory(&root) {
            continue;
        }
        let Some((guard, version)) = VersionGuard::read(&root, version_file) else {
            continue;
        };
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        files.version_guard = Some(Arc::new(guard));
        for entry in entries.flatten() {
            if files.cancelled() {
                break;
            }
            let name = entry.file_name();
            if !name
                .to_str()
                .and_then(parse_version)
                .is_some_and(|candidate| candidate < version)
            {
                continue;
            }
            let path = entry.path();
            if is_real_directory(&path) {
                walk_dir_filtered(
                    &path,
                    &root,
                    &[],
                    true,
                    true,
                    sys.is_admin,
                    files,
                    seen_paths,
                    removable_dirs,
                    probe_budget,
                );
            }
        }
        files.version_guard = None;
    }
}
