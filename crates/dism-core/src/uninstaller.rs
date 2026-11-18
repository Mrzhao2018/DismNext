use crate::diskmap::{invalidate_orphans_cache, is_protected_recycle_target, measure_dir_stats};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};
use winreg::enums::{
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
};
use winreg::RegKey;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Win32AppInfo {
    pub id: String,
    pub key_name: String,
    pub display_name: String,
    pub publisher: String,
    pub display_version: String,
    pub install_date: String,
    pub install_location: String,
    pub uninstall_string: String,
    pub quiet_uninstall_string: String,
    pub estimated_size_bytes: u64,
    pub scope: String,
    pub is_msi: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppLeftoverItem {
    pub path: String,
    pub kind: String,
    pub size_bytes: u64,
    pub file_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UninstallLaunchOutcome {
    pub app_id: String,
    pub display_name: String,
    pub command_launched: String,
    pub leftovers: Vec<AppLeftoverItem>,
}

static WIN32_APPS_CACHE: OnceLock<RwLock<Option<(Instant, Vec<Win32AppInfo>)>>> = OnceLock::new();

pub fn invalidate_win32_apps_cache() {
    if let Some(lock) = WIN32_APPS_CACHE.get() {
        if let Ok(mut w) = lock.write() {
            *w = None;
        }
    }
    invalidate_orphans_cache();
}

/// Enumerates traditional Win32 / MSI applications registered under HKLM (x64 & x86) and HKCU Uninstall keys.
pub fn list_win32_apps() -> Vec<Win32AppInfo> {
    let cache = WIN32_APPS_CACHE.get_or_init(|| RwLock::new(None));
    if let Ok(guard) = cache.read() {
        if let Some((ts, ref items)) = *guard {
            if ts.elapsed() < Duration::from_secs(30) {
                return items.clone();
            }
        }
    }

    let computed = collect_win32_apps();
    if let Ok(mut w) = cache.write() {
        *w = Some((Instant::now(), computed.clone()));
    }
    computed
}

fn collect_win32_apps() -> Vec<Win32AppInfo> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    let sources: [(&RegKey, &str, u32, &str, &str); 3] = [
        (
            &hklm,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            KEY_READ | KEY_WOW64_64KEY,
            "hklm64",
            "system_x64",
        ),
        (
            &hklm,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            KEY_READ | KEY_WOW64_32KEY,
            "hklm32",
            "system_x86",
        ),
        (
            &hkcu,
            r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
            KEY_READ,
            "hkcu",
            "user",
        ),
    ];

    let mut items = Vec::new();
    let mut seen_dedup: HashSet<(String, String)> = HashSet::new();

    for (root, subkey_path, sam, prefix, scope) in sources {
        let Ok(uninst) = root.open_subkey_with_flags(subkey_path, sam) else {
            continue;
        };
        for key_name in uninst.enum_keys().flatten() {
            let Ok(sk) = uninst.open_subkey_with_flags(&key_name, sam) else {
                continue;
            };

            let display_name: String = sk
                .get_value::<String, _>("DisplayName")
                .unwrap_or_default()
                .trim()
                .to_string();
            if display_name.is_empty() {
                continue;
            }

            let sys_component: u32 = sk.get_value::<u32, _>("SystemComponent").unwrap_or(0);
            if sys_component == 1 {
                continue;
            }

            let parent_key: String = sk
                .get_value::<String, _>("ParentKeyName")
                .unwrap_or_default()
                .trim()
                .to_string();
            if !parent_key.is_empty() {
                continue;
            }

            let release_type: String = sk
                .get_value::<String, _>("ReleaseType")
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            if matches!(
                release_type.as_str(),
                "security update" | "update" | "hotfix" | "service pack"
            ) {
                continue;
            }

            let uninstall_string: String = sk
                .get_value::<String, _>("UninstallString")
                .unwrap_or_default()
                .trim()
                .to_string();
            let quiet_uninstall_string: String = sk
                .get_value::<String, _>("QuietUninstallString")
                .unwrap_or_default()
                .trim()
                .to_string();
            if uninstall_string.is_empty() && quiet_uninstall_string.is_empty() {
                continue;
            }

            let display_version: String = sk
                .get_value::<String, _>("DisplayVersion")
                .unwrap_or_default()
                .trim()
                .to_string();

            let dedup_key = (
                display_name.to_ascii_lowercase(),
                display_version.to_ascii_lowercase(),
            );
            if !seen_dedup.insert(dedup_key) {
                continue;
            }

            let publisher: String = sk
                .get_value::<String, _>("Publisher")
                .unwrap_or_default()
                .trim()
                .to_string();
            let raw_date: String = sk
                .get_value::<String, _>("InstallDate")
                .unwrap_or_default()
                .trim()
                .to_string();
            let install_date = format_install_date(&raw_date);
            let install_location: String = sk
                .get_value::<String, _>("InstallLocation")
                .unwrap_or_default()
                .trim()
                .trim_matches('"')
                .to_string();
            let estimated_kib: u32 = sk.get_value::<u32, _>("EstimatedSize").unwrap_or(0);
            let windows_installer: u32 = sk.get_value::<u32, _>("WindowsInstaller").unwrap_or(0);
            let is_msi = windows_installer == 1
                || uninstall_string.to_ascii_lowercase().contains("msiexec");

            items.push(Win32AppInfo {
                id: format!("{}::{}", prefix, key_name),
                key_name,
                display_name,
                publisher,
                display_version,
                install_date,
                install_location,
                uninstall_string,
                quiet_uninstall_string,
                estimated_size_bytes: (estimated_kib as u64).saturating_mul(1024),
                scope: scope.to_string(),
                is_msi,
            });
        }
    }

    items.sort_by(|a, b| {
        b.estimated_size_bytes
            .cmp(&a.estimated_size_bytes)
            .then_with(|| a.display_name.to_ascii_lowercase().cmp(&b.display_name.to_ascii_lowercase()))
    });
    items
}

fn format_install_date(raw: &str) -> String {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() == 8 {
        format!("{}-{}-{}", &digits[0..4], &digits[4..6], &digits[6..8])
    } else {
        raw.to_string()
    }
}

/// Scans candidate leftover installation and AppData/ProgramData folders for a given desktop application.
pub fn scan_app_leftovers(
    display_name: &str,
    publisher: &str,
    install_location: &str,
) -> Vec<AppLeftoverItem> {
    let mut results = Vec::new();
    let mut seen_paths: HashSet<String> = HashSet::new();

    let clean_install = install_location.trim().trim_matches('"');
    if !clean_install.is_empty() {
        let p = PathBuf::from(clean_install);
        if p.is_dir() && !is_protected_recycle_target(&p) && !is_broad_program_root(&p) {
            let norm = normalize_lower_path(&p);
            if seen_paths.insert(norm) {
                let (size_bytes, file_count) = measure_dir_stats(&p);
                results.push(AppLeftoverItem {
                    path: p.to_string_lossy().into_owned(),
                    kind: "install_dir".to_string(),
                    size_bytes,
                    file_count,
                });
            }
        }
    }

    let app_tokens = extract_safe_app_tokens(display_name);
    let pub_tokens = extract_safe_app_tokens(publisher);
    if app_tokens.is_empty() {
        return results;
    }

    for (env_var, kind) in [
        ("LOCALAPPDATA", "appdata_local"),
        ("APPDATA", "appdata_roaming"),
        ("ProgramData", "program_data"),
    ] {
        let Ok(base) = std::env::var(env_var) else {
            continue;
        };
        let Ok(rd) = std::fs::read_dir(&base) else {
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
            if is_protected_recycle_target(&p) {
                continue;
            }
            let dir_name = entry.file_name().to_string_lossy().into_owned();
            let dir_lower = dir_name.to_ascii_lowercase();
            if is_reserved_app_folder(&dir_lower) {
                continue;
            }

            if folder_matches_tokens(&dir_lower, &app_tokens) {
                let norm = normalize_lower_path(&p);
                if seen_paths.insert(norm) {
                    let (size_bytes, file_count) = measure_dir_stats(&p);
                    if size_bytes > 0 || file_count > 0 {
                        results.push(AppLeftoverItem {
                            path: p.to_string_lossy().into_owned(),
                            kind: kind.to_string(),
                            size_bytes,
                            file_count,
                        });
                    }
                }
                continue;
            }

            // Check `<Publisher>\<App>` one level deeper when folder matches publisher tokens
            if !pub_tokens.is_empty() && folder_matches_tokens(&dir_lower, &pub_tokens) {
                let Ok(sub_rd) = std::fs::read_dir(&p) else {
                    continue;
                };
                for sub_entry in sub_rd.flatten() {
                    let sp = sub_entry.path();
                    let Ok(smeta) = std::fs::symlink_metadata(&sp) else {
                        continue;
                    };
                    if !smeta.is_dir()
                        || (smeta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0
                    {
                        continue;
                    }
                    let sname = sub_entry.file_name().to_string_lossy().to_ascii_lowercase();
                    if is_reserved_app_folder(&sname) {
                        continue;
                    }
                    if folder_matches_tokens(&sname, &app_tokens) {
                        let norm = normalize_lower_path(&sp);
                        if seen_paths.insert(norm) {
                            let (size_bytes, file_count) = measure_dir_stats(&sp);
                            if size_bytes > 0 || file_count > 0 {
                                results.push(AppLeftoverItem {
                                    path: sp.to_string_lossy().into_owned(),
                                    kind: kind.to_string(),
                                    size_bytes,
                                    file_count,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    results.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    results
}

fn is_broad_program_root(p: &Path) -> bool {
    let norm = normalize_lower_path(p);
    let sys_root = std::env::var("SystemRoot")
        .unwrap_or_else(|_| r"C:\Windows".to_string())
        .to_ascii_lowercase();
    let users = format!(
        r"{}\users",
        std::env::var("SystemDrive")
            .unwrap_or_else(|_| "C:".to_string())
            .to_ascii_lowercase()
    );
    let blocked = [
        sys_root,
        users,
        std::env::var("ProgramFiles").unwrap_or_default().to_ascii_lowercase(),
        std::env::var("ProgramFiles(x86)").unwrap_or_default().to_ascii_lowercase(),
        std::env::var("ProgramW6432").unwrap_or_default().to_ascii_lowercase(),
        format!(
            r"{}\Common Files",
            std::env::var("ProgramFiles").unwrap_or_default()
        )
        .to_ascii_lowercase(),
    ];
    blocked
        .iter()
        .filter(|b| !b.is_empty())
        .any(|b| b.trim_end_matches('\\') == norm)
}

fn is_reserved_app_folder(dir_lower: &str) -> bool {
    matches!(
        dir_lower,
        "microsoft"
            | "windows"
            | "packages"
            | "package cache"
            | "temp"
            | "programs"
            | "common"
            | "application data"
            | "history"
            | "local"
            | "roaming"
            | "low"
            | "google"
            | "mozilla"
            | "apple"
            | "nvidia"
            | "intel"
            | "amd"
    )
}

pub(crate) fn extract_safe_app_tokens(input: &str) -> Vec<String> {
    const GENERIC_WORDS: &[&str] = &[
        "microsoft",
        "windows",
        "google",
        "common",
        "packages",
        "temp",
        "programs",
        "corporation",
        "corp",
        "inc",
        "ltd",
        "llc",
        "gmbh",
        "software",
        "technology",
        "technologies",
        "studio",
        "studios",
        "client",
        "desktop",
        "update",
        "updater",
        "installer",
        "setup",
        "runtime",
        "redistributable",
        "edition",
        "version",
        "for",
        "and",
        "the",
        "app",
        "apps",
        "tool",
        "tools",
        "helper",
        "service",
        "x64",
        "x86",
        "arm64",
        "bit",
        "win64",
        "win32",
    ];
    let generic: HashSet<&str> = GENERIC_WORDS.iter().copied().collect();

    let mut out = Vec::new();
    let trimmed = input.trim().to_ascii_lowercase();
    if trimmed.len() >= 3 && !generic.contains(trimmed.as_str()) {
        out.push(trimmed.clone());
    }

    for part in trimmed.split(|c: char| !c.is_ascii_alphanumeric() && !is_cjk(c)) {
        if part.chars().count() < 3 {
            continue;
        }
        if part.chars().all(|c| c.is_ascii_digit() || c == 'v') {
            continue;
        }
        if generic.contains(part) {
            continue;
        }
        if !out.iter().any(|x| x == part) {
            out.push(part.to_string());
        }
    }
    out
}

fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

fn folder_matches_tokens(dir_lower: &str, tokens: &[String]) -> bool {
    if tokens.is_empty() {
        return false;
    }
    if tokens[0].len() >= 4 && (dir_lower == tokens[0] || dir_lower.contains(&tokens[0])) {
        return true;
    }
    let dir_parts: Vec<&str> = dir_lower
        .split(|c: char| !c.is_ascii_alphanumeric() && !is_cjk(c))
        .filter(|s| s.chars().count() >= 3)
        .collect();
    tokens.iter().any(|tok| {
        dir_parts
            .iter()
            .any(|dp| *dp == tok || (tok.len() >= 5 && dp.contains(tok)))
    })
}

fn normalize_lower_path(p: &Path) -> String {
    let mut s = p.to_string_lossy().replace('/', "\\").to_ascii_lowercase();
    while s.ends_with('\\') && s.len() > 3 {
        s.pop();
    }
    s
}

/// Launches the registered Win32 or MSI uninstaller process for `app_id`, returning candidate
/// leftover folders so the user can review and recycle them once uninstallation finishes.
pub fn launch_win32_uninstaller(app_id: &str, use_quiet: bool) -> Result<UninstallLaunchOutcome> {
    let apps = list_win32_apps();
    let app = apps
        .into_iter()
        .find(|a| a.id == app_id)
        .ok_or_else(|| anyhow!("Installed application '{}' not found in registry", app_id))?;

    let leftovers = scan_app_leftovers(&app.display_name, &app.publisher, &app.install_location);
    let raw_cmd = if use_quiet && !app.quiet_uninstall_string.is_empty() {
        app.quiet_uninstall_string.clone()
    } else if !app.uninstall_string.is_empty() {
        app.uninstall_string.clone()
    } else {
        app.quiet_uninstall_string.clone()
    };

    if raw_cmd.trim().is_empty() {
        return Err(anyhow!(
            "No uninstall command registered for '{}'",
            app.display_name
        ));
    }

    let _wide_hint: Vec<u16> = std::ffi::OsStr::new(&raw_cmd)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    Command::new("cmd.exe")
        .args(["/C", &raw_cmd])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .with_context(|| format!("Failed to launch uninstaller command: {}", raw_cmd))?;

    invalidate_win32_apps_cache();

    Ok(UninstallLaunchOutcome {
        app_id: app.id,
        display_name: app.display_name,
        command_launched: raw_cmd,
        leftovers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_win32_apps_enumeration_and_leftover_token_safety() {
        let apps = list_win32_apps();
        assert!(
            !apps.is_empty(),
            "Expected at least one registered Win32/MSI app on host"
        );
        for app in &apps {
            assert!(!app.display_name.is_empty());
            assert!(!app.uninstall_string.is_empty() || !app.quiet_uninstall_string.is_empty());
        }

        // Generic vendor/framework words must be excluded from leftover folder matching
        let toks = extract_safe_app_tokens("Microsoft Visual C++ 2022 Redistributable (x64)");
        assert!(!toks.iter().any(|t| t == "microsoft" || t == "redistributable" || t == "x64"));

        // Distinct product name tokens must survive
        let custom_toks = extract_safe_app_tokens("Acme SuperTool 2026");
        assert!(custom_toks.iter().any(|t| t == "acme" || t == "supertool"));

        // Protected roots must never appear as leftovers even if passed as install_location
        let sys_leftovers = scan_app_leftovers("Test App", "Test Corp", r"C:\Windows");
        assert!(sys_leftovers.is_empty());
    }
}
