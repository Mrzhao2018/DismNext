use crate::win_util::{normalize_win_path, windows_dir_or_default};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Protects volume roots, shared top-level folders, and core Windows subtrees.
pub fn is_protected_recycle_target(target: &Path) -> bool {
    static PROTECTED: OnceLock<(Vec<PathBuf>, Vec<PathBuf>)> = OnceLock::new();
    let (roots, trees) = PROTECTED.get_or_init(|| {
        let windows = windows_dir_or_default();
        let drive = windows.ancestors().last().unwrap_or(&windows);
        let mut trees = vec![
            windows.join("System32"),
            windows.join("SysWOW64"),
            windows.join("WinSxS"),
            windows.join("Servicing"),
            windows.join("INF"),
            windows.join("Fonts"),
            windows.join("SystemApps"),
            windows.join("Microsoft.NET"),
            windows.join("Boot"),
            windows.join("Recovery"),
            drive.join("Boot"),
            drive.join("Recovery"),
            drive.join("Program Files").join("WindowsApps"),
        ];
        for var in ["ProgramW6432", "ProgramFiles"] {
            if let Some(pf) = std::env::var_os(var).filter(|value| !value.is_empty()) {
                trees.push(PathBuf::from(pf).join("WindowsApps"));
            }
        }
        let mut roots = vec![
            windows.clone(),
            drive.join("Users"),
            drive.join("Program Files"),
            drive.join("Program Files (x86)"),
            drive.join("ProgramData"),
        ];
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            roots.push(PathBuf::from(program_files).join("Common Files"));
        }
        roots.extend(
            [
                "ProgramFiles",
                "ProgramFiles(x86)",
                "ProgramW6432",
                "ProgramData",
                "USERPROFILE",
                "LOCALAPPDATA",
                "APPDATA",
            ]
            .into_iter()
            .filter_map(std::env::var_os)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
        );
        (
            roots.iter().map(|path| normalize_win_path(path)).collect(),
            trees.iter().map(|path| normalize_win_path(path)).collect(),
        )
    });
    let protected = |path: &Path| {
        let path = normalize_win_path(path);
        (path.has_root() && path.parent().is_none())
            || roots.contains(&path)
            || trees.iter().any(|tree| path.starts_with(tree))
    };
    let raw = target.as_os_str().to_string_lossy();
    (raw.len() == 2 && raw.ends_with(':'))
        || protected(target)
        || target.canonicalize().is_ok_and(|path| protected(&path))
}
