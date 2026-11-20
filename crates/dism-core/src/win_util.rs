use crate::com::Apartment;
use anyhow::{anyhow, ensure, Result};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf, Prefix};
use windows::core::PCWSTR;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems};
use windows_sys::Win32::Storage::FileSystem::{
    GetDriveTypeW, FILE_LIST_DIRECTORY, FILE_SHARE_READ,
};
use windows_sys::Win32::System::WindowsProgramming::{DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOVABLE};

pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;
pub(crate) const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

pub(crate) fn windows_dir_or_default() -> PathBuf {
    crate::system_tools::windows_dir().unwrap_or_else(|_| PathBuf::from(r"C:\Windows"))
}

/// Normalizes a Windows path for consistent comparison (`\` separators, ASCII lowercase,
/// `\\?\` / `\\?\UNC\` stripped, `.` / `..` collapsed when non-verbatim, no trailing `\`
/// except on drive roots like `c:\`).
pub(crate) fn normalize_win_path(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy().replace('/', "\\");
    let stripped = if let Some(rest) = strip_prefix_ci(&raw, r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        if rest.as_bytes().get(1) == Some(&b':') {
            rest.to_string()
        } else {
            raw
        }
    } else {
        raw
    };

    let candidate = Path::new(&stripped);
    let resolved = if !candidate.is_absolute()
        || candidate
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        std::path::absolute(candidate).unwrap_or_else(|_| candidate.to_path_buf())
    } else {
        candidate.to_path_buf()
    };

    let normalized: PathBuf = resolved.components().collect();
    let mut lower = normalized
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix(r"\\?\unc\") {
        lower = format!(r"\\{rest}");
    } else if let Some(rest) = lower.strip_prefix(r"\\?\") {
        if rest.as_bytes().get(1) == Some(&b':') {
            lower = rest.to_string();
        }
    }
    while lower.ends_with('\\') && !is_drive_root_str(&lower) && lower != r"\" {
        lower.pop();
    }
    PathBuf::from(lower)
}

fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    {
        Some(&text[prefix.len()..])
    } else {
        None
    }
}

fn is_drive_root_str(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() == 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\'
}

/// Returns true if any non-root ancestor (or `path` itself when `include_self` is true)
/// is a Windows reparse point, or cannot be verified safely.
/// Non-existent components (`NotFound`) are tolerated so creation targets can verify existing ancestors.
pub(crate) fn has_reparse_point_in_ancestors(path: &Path, include_self: bool) -> bool {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let check = |candidate: &Path, is_ancestor: bool| -> bool {
        if candidate.parent().is_none() {
            return false;
        }
        match std::fs::symlink_metadata(candidate) {
            Ok(meta) => {
                (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
                    || (is_ancestor && !meta.is_dir())
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => false,
            Err(_) => true,
        }
    };

    let mut ancestors: Vec<_> = abs
        .ancestors()
        .skip(1)
        .take_while(|path| path.parent().is_some())
        .collect();
    ancestors.reverse();
    for ancestor in ancestors {
        if check(ancestor, true) {
            return true;
        }
    }
    include_self && check(&abs, false)
}

/// Returns true for UNC / network paths (`\\server\share`, `//server/share`, `\\?\UNC\...`).
pub(crate) fn is_unc_path(path: &Path) -> bool {
    if let Some(Component::Prefix(prefix)) = path.components().next() {
        if matches!(prefix.kind(), Prefix::UNC(..) | Prefix::VerbatimUNC(..)) {
            return true;
        }
    }
    let raw = path.to_string_lossy().replace('/', "\\");
    if strip_prefix_ci(&raw, r"\\?\UNC\").is_some() {
        return true;
    }
    raw.starts_with(r"\\") && !raw.starts_with(r"\\?\") && !raw.starts_with(r"\\.\")
}

pub(crate) fn is_local_drive_path(path: &Path) -> bool {
    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return false;
    };
    let drive = match prefix.kind() {
        Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) if path.is_absolute() => drive,
        _ => return false,
    };
    let root = [u16::from(drive), u16::from(b':'), u16::from(b'\\'), 0];
    matches!(
        unsafe { GetDriveTypeW(root.as_ptr()) },
        DRIVE_FIXED | DRIVE_REMOVABLE | DRIVE_RAMDISK
    )
}

/// Keep each directory open while a caller writes by name below it.
pub(crate) fn pin_directory(path: &Path, create: bool) -> io::Result<Vec<File>> {
    if !is_local_drive_path(path)
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Expected an absolute local directory",
        ));
    }
    let mut parents: Vec<_> = path
        .ancestors()
        .filter(|part| part.parent().is_some())
        .collect();
    parents.reverse();
    let mut handles = Vec::with_capacity(parents.len());
    for parent in parents {
        let open = || {
            crate::file_stamp::open_for_verification(
                parent,
                true,
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ,
            )
        };
        let handle = match open() {
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                match fs::create_dir(parent) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                open()?
            }
            result => result?,
        };
        let meta = handle.metadata()?;
        if !meta.is_dir() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Expected a directory without reparse points",
            ));
        }
        handles.push(handle);
    }
    Ok(handles)
}

/// Quotes a single Windows command-line argument according to Microsoft CRT / `CommandLineToArgvW` rules.
pub(crate) fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.bytes().any(|b| matches!(b, b' ' | b'\t' | b'"')) {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                for _ in 0..(backslashes * 2 + 1) {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            }
            _ => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                out.push(ch);
                backslashes = 0;
            }
        }
    }
    for _ in 0..(backslashes * 2) {
        out.push('\\');
    }
    out.push('"');
    out
}

/// Opens Windows File Explorer and selects `target` via Shell API without invoking explorer.exe CLI string parsing.
pub fn reveal_in_explorer(target: &Path) -> Result<()> {
    ensure!(
        !target.as_os_str().is_empty(),
        "Reveal path must not be empty"
    );
    ensure!(
        !is_unc_path(target),
        "Refusing to reveal network/UNC path in Explorer: {}",
        target.display()
    );
    let abs = std::path::absolute(target)?;
    ensure!(
        is_local_drive_path(&abs),
        "Refusing to reveal a path outside local drives: {}",
        abs.display()
    );
    ensure!(
        !has_reparse_point_in_ancestors(&abs, true),
        "Refusing to traverse a reparse point to reveal {}",
        abs.display()
    );
    ensure!(abs.exists(), "Path does not exist: {}", target.display());

    run_on_sta(move || reveal_in_explorer_sta(&abs))
}

fn run_on_sta<T: Send + 'static>(work: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    match Apartment::initialize() {
        Ok(_apt) => return work(),
        Err(error) if Apartment::is_changed_mode(&error) => {}
        Err(error) => return Err(error.into()),
    }
    std::thread::Builder::new()
        .name("dism-sta-shell".to_string())
        .spawn(move || {
            let _apt = Apartment::initialize()
                .map_err(|err| anyhow!("Failed to initialize COM STA apartment: {err}"))?;
            work()
        })?
        .join()
        .map_err(|_| anyhow!("COM STA helper thread stopped unexpectedly"))?
}

fn reveal_in_explorer_sta(target: &Path) -> Result<()> {
    struct PidlGuard(*mut ITEMIDLIST);
    impl Drop for PidlGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { ILFree(Some(self.0.cast_const())) };
            }
        }
    }

    let wide: Vec<u16> = OsStr::new(target).encode_wide().chain(Some(0)).collect();
    let pidl = unsafe { ILCreateFromPathW(PCWSTR(wide.as_ptr())) };
    if pidl.is_null() {
        return Err(anyhow!(
            "Could not resolve shell item for {}",
            target.display()
        ));
    }
    let _guard = PidlGuard(pidl);
    unsafe { SHOpenFolderAndSelectItems(pidl.cast_const(), None, 0) }
        .map_err(|err| anyhow!("Failed to open Explorer for {}: {err}", target.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_windows_path_variants() {
        assert_eq!(
            normalize_win_path(Path::new(r"C:\Temp\Cache\")),
            PathBuf::from(r"c:\temp\cache")
        );
        assert_eq!(
            normalize_win_path(Path::new(r"c:/TEMP/./cache/nested\..\")),
            PathBuf::from(r"c:\temp\cache")
        );
        assert_eq!(
            normalize_win_path(Path::new(r"\\?\C:\Temp\Cache\")),
            PathBuf::from(r"c:\temp\cache")
        );
        assert_eq!(normalize_win_path(Path::new(r"C:\")), PathBuf::from(r"c:\"));
        assert_eq!(
            normalize_win_path(Path::new(r"\\?\UNC\Server\Share\Folder\")),
            PathBuf::from(r"\\server\share\folder")
        );
    }

    #[test]
    fn detects_unc_paths() {
        assert!(is_unc_path(Path::new(r"\\server\share\file.txt")));
        assert!(is_unc_path(Path::new(r"//server/share/file.txt")));
        assert!(is_unc_path(Path::new(r"\\?\UNC\server\share\file.txt")));
        assert!(!is_unc_path(Path::new(r"C:\Windows\System32")));
        assert!(!is_unc_path(Path::new(r"\\?\C:\Windows\System32")));
    }

    #[test]
    fn quotes_windows_args_per_crt_rules() {
        assert_eq!(quote_windows_arg("--port"), "--port");
        assert_eq!(quote_windows_arg(""), "\"\"");
        assert_eq!(quote_windows_arg(r"C:\My App\"), r#""C:\My App\\""#);
        assert_eq!(
            quote_windows_arg(r#"say "hello\" world"#),
            r#""say \"hello\\\" world""#
        );
        assert_eq!(quote_windows_arg("a\tb"), "\"a\tb\"");
    }

    #[test]
    fn reveal_rejects_unc_and_missing_paths() {
        assert!(reveal_in_explorer(Path::new(r"\\evil\share\file.txt")).is_err());
        assert!(reveal_in_explorer(Path::new(r"C:\DismNext_NonExistent_Path_12345")).is_err());
    }
}
