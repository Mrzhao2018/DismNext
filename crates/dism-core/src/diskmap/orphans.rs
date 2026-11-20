use crate::path_recycle::scan_timestamp;
use crate::ttl_cache::TtlCache;
use crate::win_util::FILE_ATTRIBUTE_REPARSE_POINT;
use chrono::DateTime;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};
use winreg::enums::{
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
};
use winreg::RegKey;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrphanDirEntry {
    pub name: String,
    pub path: String,
    pub scope_label: String,
    pub size_bytes: u64,
    pub file_count: usize,
    #[serde(default)]
    pub size_is_estimate: bool,
    pub modified_date: String,
    #[serde(default)]
    pub scan_started_at: String,
}

static ORPHANS_CACHE: TtlCache<Vec<OrphanDirEntry>> = TtlCache::new(Duration::from_secs(30));

pub fn invalidate_orphans_cache() {
    ORPHANS_CACHE.invalidate();
}

/// Scans top-level AppData / ProgramData directories and flags large folders not matched
/// against active Uninstall keys, installed AppX apps, or known Windows/vendor safelists.
pub fn find_orphaned_app_dirs() -> Vec<OrphanDirEntry> {
    ORPHANS_CACHE.get_or_load(compute_orphaned_app_dirs)
}

fn compute_orphaned_app_dirs() -> Vec<OrphanDirEntry> {
    let scan_started_at = scan_timestamp();
    let (installed_tokens, installed_dirs) = collect_installed_app_tokens();
    let safelist: HashSet<&'static str> = [
        "microsoft",
        "windows",
        "package cache",
        "packages",
        "temp",
        "d3dscache",
        "connecteddevicesplatform",
        "crashdumps",
        "comms",
        "publishers",
        "virtualstore",
        "cef",
        "squirreltemp",
        "fontcache",
        "usoshared",
        "ssh",
        "git",
        "npm",
        "npm-cache",
        "pip",
        "uv",
        "pnpm",
        "yarn",
        "cargo",
        "rustup",
        "go-build",
        "nuget",
        "jetbrains",
        "vscode",
        "code",
        "google",
        "mozilla",
        "nvidia",
        "nvidia corporation",
        "intel",
        "amd",
        "realtek",
        "dolby",
        "lenovo",
        "apple",
        "apple computer",
        "steam",
        "epicgameslauncher",
        "unity",
        "unrealengine",
        "solidworks",
        "ansys",
        "codesys",
        "codemeter",
        "bonjour",
        "coremail",
        "baidunetdisk",
        "iconcache",
        "elevateddiagnostics",
        "low",
        "programs",
        "peernetworking",
        "application data",
        "history",
        "local",
        "roaming",
    ]
    .into_iter()
    .collect();

    let mut candidates: Vec<(String, PathBuf, String)> = Vec::new();
    for (env_var, label) in [
        ("LOCALAPPDATA", "AppData\\Local"),
        ("APPDATA", "AppData\\Roaming"),
        ("ProgramData", "ProgramData"),
    ] {
        let Ok(base_str) = std::env::var(env_var) else {
            continue;
        };
        let Ok(rd) = std::fs::read_dir(&base_str) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            if !meta.is_dir() || (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name.starts_with('$') {
                continue;
            }
            let lower_name = name.trim().to_ascii_lowercase();
            let lower_path = p.to_string_lossy().to_ascii_lowercase();

            if safelist.contains(lower_name.as_str())
                || installed_dirs.iter().any(|id_path| {
                    lower_path.starts_with(id_path) || id_path.starts_with(&lower_path)
                })
                || matches_app_tokens(&lower_name, &installed_tokens, &safelist)
            {
                continue;
            }
            let mod_date = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .and_then(|d| DateTime::from_timestamp(d.as_secs() as i64, 0))
                .map(|dt| dt.format("%Y-%m-%d").to_string())
                .unwrap_or_default();
            candidates.push((name, p, format!("{}|{}", label, mod_date)));
        }
    }

    let mut results: Vec<OrphanDirEntry> = candidates
        .into_par_iter()
        .filter_map(|(name, path, meta_tag)| {
            let stats = measure_dir_stats(&path);
            if stats.size_bytes < 1_048_576 {
                return None;
            }
            let (scope_label, modified_date) = meta_tag.split_once('|').unwrap_or(("", ""));
            Some(OrphanDirEntry {
                name,
                path: path.to_string_lossy().into_owned(),
                scope_label: scope_label.to_string(),
                size_bytes: stats.size_bytes,
                file_count: stats.file_count,
                size_is_estimate: stats.incomplete,
                modified_date: modified_date.to_string(),
                scan_started_at: scan_started_at.clone(),
            })
        })
        .collect();

    results.sort_by_key(|entry| std::cmp::Reverse(entry.size_bytes));
    results
}

#[derive(Default)]
pub(crate) struct DirStats {
    pub size_bytes: u64,
    pub file_count: usize,
    pub incomplete: bool,
}

pub(crate) fn measure_dir_stats(dir: &Path) -> DirStats {
    measure_dir_stats_up_to(dir, 20_000)
}

pub(crate) fn measure_dir_stats_up_to(dir: &Path, file_limit: usize) -> DirStats {
    let mut stats = DirStats::default();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&cur) else {
            stats.incomplete = true;
            continue;
        };
        for entry in rd {
            let Ok(entry) = entry else {
                stats.incomplete = true;
                continue;
            };
            let Ok(meta) = entry.metadata() else {
                stats.incomplete = true;
                continue;
            };
            if (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                continue;
            }
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                stats.size_bytes = stats.size_bytes.saturating_add(meta.len());
                stats.file_count += 1;
                if stats.file_count >= file_limit {
                    stats.incomplete = true;
                    return stats;
                }
            }
        }
    }
    stats
}

fn collect_installed_app_tokens() -> (HashSet<String>, Vec<String>) {
    let mut tokens = HashSet::new();
    let mut dirs = Vec::new();

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let targets = [
        (
            &hklm,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            KEY_READ | KEY_WOW64_64KEY,
        ),
        (
            &hklm,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            KEY_READ | KEY_WOW64_32KEY,
        ),
        (
            &hkcu,
            r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
            KEY_READ,
        ),
    ];

    for (root, sub, sam) in targets {
        if let Ok(uninst) = root.open_subkey_with_flags(sub, sam) {
            for k in uninst.enum_keys().flatten() {
                let Ok(sk) = uninst.open_subkey_with_flags(&k, sam) else {
                    continue;
                };
                for val_name in ["DisplayName", "Publisher"] {
                    if let Ok(s) = sk.get_value::<String, _>(val_name) {
                        add_tokens(&s, &mut tokens);
                    }
                }
                if let Ok(loc) = sk.get_value::<String, _>("InstallLocation") {
                    let norm = loc.trim().trim_end_matches('\\').to_ascii_lowercase();
                    if norm.len() >= 4 {
                        dirs.push(norm);
                    }
                }
            }
        }
    }

    for pkg in crate::appx::list_appx_packages() {
        add_tokens(&pkg.name, &mut tokens);
        add_tokens(&pkg.display_name.zh, &mut tokens);
        add_tokens(&pkg.display_name.en, &mut tokens);
    }

    (tokens, dirs)
}

fn add_tokens(s: &str, tokens: &mut HashSet<String>) {
    let full = s.trim().to_ascii_lowercase();
    if full.len() >= 3 {
        tokens.insert(full);
    }
    for part in s.split(|c: char| !c.is_ascii_alphanumeric()) {
        let lp = part.to_ascii_lowercase();
        if lp.len() >= 3 {
            tokens.insert(lp);
        }
    }
}

fn matches_app_tokens(
    dir_lower: &str,
    tokens: &HashSet<String>,
    safelist: &HashSet<&'static str>,
) -> bool {
    if tokens.contains(dir_lower) {
        return true;
    }
    for part in dir_lower.split(|c: char| !c.is_ascii_alphanumeric()) {
        if part.len() >= 4 && (tokens.contains(part) || safelist.contains(part)) {
            return true;
        }
    }
    false
}
