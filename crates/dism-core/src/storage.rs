use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_DELETE_ON_CLOSE;

pub(crate) struct DataDirectories {
    pub primary: PathBuf,
    pub legacy: Vec<PathBuf>,
}

pub(crate) fn data_directories() -> &'static DataDirectories {
    static DATA: OnceLock<DataDirectories> = OnceLock::new();
    DATA.get_or_init(|| {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let exe_folder = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| cwd.clone());
        let portable = [cwd.join("data"), exe_folder.join("data")];
        let profile = std::env::var_os("LOCALAPPDATA")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.join("data"))
            .join("DismNext");
        choose_directories(portable, profile, is_writable)
    })
}

fn choose_directories(
    portable: [PathBuf; 2],
    profile: PathBuf,
    is_writable: impl Fn(&Path) -> bool,
) -> DataDirectories {
    // Keep existing portable data in place. New portable installs write beside the executable.
    let primary = portable
        .iter()
        .filter(|path| path.is_dir())
        .chain(std::iter::once(&portable[1]))
        .find(|path| is_writable(path))
        .cloned()
        .unwrap_or_else(|| profile.clone());
    let mut legacy = Vec::new();
    for path in portable.into_iter().chain(std::iter::once(profile)) {
        if path != primary && !legacy.contains(&path) {
            legacy.push(path);
        }
    }
    DataDirectories { primary, legacy }
}

fn is_writable(path: &Path) -> bool {
    let directory = if path.is_dir() {
        path
    } else if !path.exists() {
        let Some(parent) = path.parent() else {
            return false;
        };
        parent
    } else {
        return false;
    };
    let probe = directory.join(format!(".dismnext-probe-{}", unique_suffix()));
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(probe)
        .is_ok()
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", unique_suffix()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let result = written.and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn unique_suffix() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}_{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwritable_portable_locations_fall_back_to_the_profile() {
        let candidates = [PathBuf::from("cwd/data"), PathBuf::from("portable/data")];
        let profile = PathBuf::from("profile/DismNext");
        let paths = choose_directories(candidates.clone(), profile.clone(), |_| false);
        assert_eq!(paths.primary, profile);
        assert_eq!(paths.legacy, candidates);
        let paths = choose_directories(candidates.clone(), profile, |_| true);
        assert_eq!(paths.primary, candidates[1]);
        assert!(paths.legacy.contains(&PathBuf::from("profile/DismNext")));
    }
}
