use crate::diskmap::{invalidate_orphans_cache, is_protected_recycle_target, measure_dir_stats};
use crate::path_recycle::{recycle_paths, PathSnapshot};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::os::windows::fs::MetadataExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus};
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};
use winreg::enums::{
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
};
use winreg::RegKey;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";

pub use crate::path_recycle::{
    RecycleItem as LeftoverRecycleItem, RecycleOutcome as LeftoverRecycleOutcome,
    RecycleSkipReason as LeftoverSkipReason, RecycleTarget as LeftoverRecycleTarget,
};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LeftoverReview {
    Ready { stamp: String },
    Blocked { reason: LeftoverSkipReason },
}

pub fn recycle_app_leftovers(targets: &[LeftoverRecycleTarget]) -> Vec<LeftoverRecycleItem> {
    recycle_paths(targets, Some(true))
}

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
    pub match_basis: LeftoverMatch,
    pub size_bytes: u64,
    pub file_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<LeftoverReview>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeftoverMatch {
    RegisteredInstallLocation,
    ApplicationName,
    PublisherAndApplicationName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UninstallState {
    Running,
    AwaitingVerification,
    Completed,
    Cancelled,
    RestartRequired,
    Failed,
}

#[derive(Debug, Serialize)]
pub struct UninstallStatus<'a> {
    pub app_id: &'a str,
    pub display_name: &'a str,
    pub state: UninstallState,
    pub exit_code: Option<i32>,
}

pub struct Win32Uninstall {
    app: Win32AppInfo,
    process: UninstallProcess,
    last_state: UninstallState,
}

enum UninstallProcess {
    Running(Child),
    Exited(ExitStatus),
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

fn uninstall_sources() -> [(RegKey, u32, &'static str, &'static str); 3] {
    [
        (
            RegKey::predef(HKEY_LOCAL_MACHINE),
            KEY_READ | KEY_WOW64_64KEY,
            "hklm64",
            "system_x64",
        ),
        (
            RegKey::predef(HKEY_LOCAL_MACHINE),
            KEY_READ | KEY_WOW64_32KEY,
            "hklm32",
            "system_x86",
        ),
        (
            RegKey::predef(HKEY_CURRENT_USER),
            KEY_READ,
            "hkcu",
            "user",
        ),
    ]
}

fn collect_win32_apps() -> Vec<Win32AppInfo> {
    let mut items = Vec::new();
    let mut seen_dedup: HashSet<(String, String)> = HashSet::new();

    for (root, sam, prefix, scope) in uninstall_sources() {
        let Ok(uninst) = root.open_subkey_with_flags(UNINSTALL_KEY, sam) else {
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
    scan_leftovers(display_name, publisher, install_location, false)
}

pub fn scan_reviewable_app_leftovers(
    display_name: &str,
    publisher: &str,
    install_location: &str,
) -> Vec<AppLeftoverItem> {
    scan_leftovers(display_name, publisher, install_location, true)
}

fn scan_leftovers(
    display_name: &str,
    publisher: &str,
    install_location: &str,
    reviewable: bool,
) -> Vec<AppLeftoverItem> {
    let mut results = Vec::new();
    let mut seen_paths: HashSet<String> = HashSet::new();
    let mut record = |path: &Path, kind: &str, match_basis| {
        if !seen_paths.insert(normalize_lower_path(path)) {
            return;
        }
        let (size_bytes, file_count, review) = if reviewable {
            match PathSnapshot::capture(path, Some(true)) {
                Ok(snapshot) => (
                    snapshot.size_bytes,
                    snapshot.file_count,
                    Some(LeftoverReview::Ready { stamp: snapshot.stamp }),
                ),
                Err(reason) => (0, 0, Some(LeftoverReview::Blocked { reason })),
            }
        } else {
            let (size, count) = measure_dir_stats(path);
            (size, count, None)
        };
        if match_basis == LeftoverMatch::RegisteredInstallLocation
            || file_count > 0
            || matches!(review, Some(LeftoverReview::Blocked { .. }))
        {
            results.push(AppLeftoverItem {
                path: path.to_string_lossy().into_owned(),
                kind: kind.to_string(),
                match_basis,
                size_bytes,
                file_count,
                review,
            });
        }
    };

    let clean_install = install_location.trim().trim_matches('"');
    if !clean_install.is_empty() {
        let p = PathBuf::from(clean_install);
        if p.is_dir() && !is_protected_recycle_target(&p) {
            record(&p, "install_dir", LeftoverMatch::RegisteredInstallLocation);
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
                record(&p, kind, LeftoverMatch::ApplicationName);
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
                        record(&sp, kind, LeftoverMatch::PublisherAndApplicationName);
                    }
                }
            }
        }
    }

    results.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    results
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

/// Starts the registered uninstaller and retains its process and application identity.
pub fn launch_win32_uninstaller(app_id: &str, use_quiet: bool) -> Result<Win32Uninstall> {
    let app = collect_win32_apps()
        .into_iter()
        .find(|a| a.id == app_id)
        .ok_or_else(|| anyhow!("Installed application '{}' not found in registry", app_id))?;

    let raw_cmd = if use_quiet && !app.quiet_uninstall_string.is_empty() {
        &app.quiet_uninstall_string
    } else if !app.uninstall_string.is_empty() {
        &app.uninstall_string
    } else {
        &app.quiet_uninstall_string
    };

    if raw_cmd.trim().is_empty() {
        return Err(anyhow!(
            "No uninstall command registered for '{}'",
            app.display_name
        ));
    }

    let child = crate::system_tools::system32_command("cmd.exe")?
        .args(["/D", "/S", "/C"])
        .raw_arg(format!("\"{raw_cmd}\""))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .with_context(|| format!("Failed to launch uninstaller command: {}", raw_cmd))?;

    Ok(Win32Uninstall {
        app,
        process: UninstallProcess::Running(child),
        last_state: UninstallState::Running,
    })
}

impl Win32Uninstall {
    pub fn status(&mut self) -> Result<UninstallStatus<'_>> {
        if let UninstallProcess::Running(child) = &mut self.process {
            if let Some(exit) = child.try_wait().context("Checking the uninstaller process")? {
                self.process = UninstallProcess::Exited(exit);
            }
        }

        let (state, exit_code) = match self.process {
            UninstallProcess::Running(_) => (UninstallState::Running, None),
            UninstallProcess::Exited(exit) => {
                let code = exit.code();
                let state = classify_uninstall_exit(self.app.is_msi, code, || {
                    is_app_registered(&self.app)
                })?;
                (state, code)
            }
        };

        if state != self.last_state {
            invalidate_win32_apps_cache();
            self.last_state = state;
        }

        Ok(UninstallStatus {
            app_id: &self.app.id,
            display_name: &self.app.display_name,
            state,
            exit_code,
        })
    }

    /// Uses current status to gate the scan and retains metadata after the registry entry is gone.
    pub fn completed_app(&mut self, app_id: &str) -> Result<Win32AppInfo> {
        let status = self.status()?;
        anyhow::ensure!(
            status.app_id == app_id && status.state == UninstallState::Completed,
            "The application's uninstallation has not been verified"
        );
        Ok(self.app.clone())
    }
}

fn classify_uninstall_exit(
    is_msi: bool,
    code: Option<i32>,
    is_registered: impl FnOnce() -> Result<bool>,
) -> Result<UninstallState> {
    Ok(match code {
        Some(0) if is_registered()? => UninstallState::AwaitingVerification,
        Some(0) => UninstallState::Completed,
        Some(1602) if is_msi => UninstallState::Cancelled,
        Some(1641 | 3010) if is_msi => UninstallState::RestartRequired,
        _ => UninstallState::Failed,
    })
}

fn is_app_registered(app: &Win32AppInfo) -> Result<bool> {
    let (prefix, _) = app
        .id
        .split_once("::")
        .context("Invalid application registry source")?;
    let (root, access, _, _) = uninstall_sources()
        .into_iter()
        .find(|(_, _, source, _)| *source == prefix)
        .context("Unknown application registry source")?;
    match root.open_subkey_with_flags(format!("{UNINSTALL_KEY}\\{}", app.key_name), access) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).context("Checking the application's uninstall registration"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uninstall_scan_requires_success_and_removal_of_registration() {
        assert_eq!(
            classify_uninstall_exit(false, Some(0), || Ok(false)).unwrap(),
            UninstallState::Completed
        );
        assert_eq!(
            classify_uninstall_exit(true, Some(0), || Ok(true)).unwrap(),
            UninstallState::AwaitingVerification
        );
        assert!(classify_uninstall_exit(true, Some(0), || {
            anyhow::bail!("registry unavailable")
        })
        .is_err());

        for (is_msi, code, expected) in [
            (true, Some(1602), UninstallState::Cancelled),
            (true, Some(1603), UninstallState::Failed),
            (true, Some(1641), UninstallState::RestartRequired),
            (true, Some(3010), UninstallState::RestartRequired),
            (false, Some(3010), UninstallState::Failed),
            (false, None, UninstallState::Failed),
        ] {
            assert_eq!(
                classify_uninstall_exit(is_msi, code, || panic!("unexpected registry lookup"))
                    .unwrap(),
                expected
            );
        }
    }

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
