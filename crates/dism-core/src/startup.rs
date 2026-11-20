use crate::evaluator::{expand_env_vars, reg_value_to_string};
use crate::ttl_cache::TtlCache;
use crate::win_util::{windows_dir_or_default, CREATE_NO_WINDOW};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SERVICES_KEY: &str = r"SYSTEM\CurrentControlSet\Services";
use winreg::enums::{
    RegType, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_ALL_ACCESS, KEY_READ, KEY_WOW64_32KEY,
    KEY_WOW64_64KEY,
};
use winreg::{RegKey, RegValue, HKEY};

mod native;
use native::{compute_scheduled_tasks_uncached, inspect_launch_file};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StartupScope {
    User,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartupItem {
    pub id: String,
    pub name: String,
    pub command: String,
    pub target_path: Option<String>,
    pub location: String,
    pub scope: StartupScope,
    pub enabled: bool,
    pub target_exists: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StartupOrigin {
    Registry {
        key_path: String,
        value_name: String,
    },
    StartupFolder {
        path: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupSignature {
    Valid,
    Unsigned,
    Untrusted,
    HashMismatch,
    Unsupported,
    MissingTarget,
    UnresolvedTarget,
    Unavailable,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StartupTargetInspection {
    pub path: Option<String>,
    pub signature: StartupSignature,
    pub signer: Option<String>,
    pub company: Option<String>,
}

impl StartupTargetInspection {
    fn without_signature(path: Option<String>, signature: StartupSignature) -> Self {
        Self {
            path,
            signature,
            signer: None,
            company: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct StartupInspection {
    pub origin: StartupOrigin,
    pub target: StartupTargetInspection,
}

#[derive(Clone, Copy)]
enum SourceKind {
    RegistryRun,
    StartupFolder,
}

struct StartupSourceDef {
    tag: &'static str,
    location: &'static str,
    scope: StartupScope,
    kind: SourceKind,
    hive: HKEY,
    run_subkey: &'static str,
    approved_subkey: &'static str,
    run_sam: u32,
    approved_sam: u32,
}

impl StartupSourceDef {
    fn registry_path(&self) -> String {
        match self.scope {
            StartupScope::User => format!(r"HKEY_CURRENT_USER\{}", self.run_subkey),
            StartupScope::System if self.run_sam == KEY_WOW64_32KEY => {
                let software_subkey = self.run_subkey.trim_start_matches(r"SOFTWARE\");
                format!(r"HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\{software_subkey}")
            }
            StartupScope::System => format!(r"HKEY_LOCAL_MACHINE\{}", self.run_subkey),
        }
    }
}

const SOURCES: &[StartupSourceDef] = &[
    StartupSourceDef {
        tag: "hkcu_run",
        location: "HKCU Run",
        scope: StartupScope::User,
        kind: SourceKind::RegistryRun,
        hive: HKEY_CURRENT_USER,
        run_subkey: r"Software\Microsoft\Windows\CurrentVersion\Run",
        approved_subkey: r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run",
        run_sam: KEY_WOW64_64KEY,
        approved_sam: KEY_WOW64_64KEY,
    },
    StartupSourceDef {
        tag: "hklm_run",
        location: "HKLM Run",
        scope: StartupScope::System,
        kind: SourceKind::RegistryRun,
        hive: HKEY_LOCAL_MACHINE,
        run_subkey: r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run",
        approved_subkey: r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run",
        run_sam: KEY_WOW64_64KEY,
        approved_sam: KEY_WOW64_64KEY,
    },
    StartupSourceDef {
        tag: "hklm_run32",
        location: "HKLM Run (32-bit)",
        scope: StartupScope::System,
        kind: SourceKind::RegistryRun,
        hive: HKEY_LOCAL_MACHINE,
        run_subkey: r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run",
        approved_subkey: r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run32",
        run_sam: KEY_WOW64_32KEY,
        // StartupApproved\Run32 lives in the 64-bit view on 64-bit Windows.
        approved_sam: KEY_WOW64_64KEY,
    },
    StartupSourceDef {
        tag: "user_folder",
        location: "User Startup Folder",
        scope: StartupScope::User,
        kind: SourceKind::StartupFolder,
        hive: HKEY_CURRENT_USER,
        run_subkey: "",
        approved_subkey: r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder",
        run_sam: KEY_WOW64_64KEY,
        approved_sam: KEY_WOW64_64KEY,
    },
    StartupSourceDef {
        tag: "common_folder",
        location: "Common Startup Folder",
        scope: StartupScope::System,
        kind: SourceKind::StartupFolder,
        hive: HKEY_LOCAL_MACHINE,
        run_subkey: "",
        approved_subkey: r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder",
        run_sam: KEY_WOW64_64KEY,
        approved_sam: KEY_WOW64_64KEY,
    },
];

/// Lists all Windows startup items from Registry Run keys and Startup folders,
/// merged with `StartupApproved` enabled/disabled status.
pub fn list_startup_items() -> Vec<StartupItem> {
    let mut items = Vec::new();
    for src in SOURCES {
        let approved_map = read_approved_map(src.hive, src.approved_subkey, src.approved_sam);
        match src.kind {
            SourceKind::RegistryRun => {
                let root = RegKey::predef(src.hive);
                if let Ok(k) = root.open_subkey_with_flags(src.run_subkey, KEY_READ | src.run_sam) {
                    for (name, val) in k.enum_values().flatten() {
                        if name.trim().is_empty() {
                            continue;
                        }
                        let cmd = reg_value_to_string(&val).trim().to_string();
                        if cmd.is_empty() {
                            continue;
                        }
                        let enabled = approved_map
                            .get(&name.to_ascii_lowercase())
                            .copied()
                            .unwrap_or(true);
                        let target_path = extract_target_path(&cmd);
                        let target_exists = target_path
                            .as_deref()
                            .map(|p| Path::new(p).exists())
                            .unwrap_or(false);
                        items.push(StartupItem {
                            id: format!("{}:{}", src.tag, name),
                            name,
                            command: cmd,
                            target_path,
                            location: src.location.to_string(),
                            scope: src.scope,
                            enabled,
                            target_exists,
                        });
                    }
                }
            }
            SourceKind::StartupFolder => {
                if let Some(dir) = resolve_startup_folder(src.tag) {
                    if let Ok(rd) = std::fs::read_dir(&dir) {
                        for entry in rd.flatten() {
                            let path = entry.path();
                            if !path.is_file() {
                                continue;
                            }
                            let name = entry.file_name().to_string_lossy().into_owned();
                            if name.eq_ignore_ascii_case("desktop.ini") {
                                continue;
                            }
                            let enabled = approved_map
                                .get(&name.to_ascii_lowercase())
                                .copied()
                                .unwrap_or(true);
                            let full_str = path.to_string_lossy().into_owned();
                            items.push(StartupItem {
                                id: format!("{}:{}", src.tag, name),
                                name,
                                command: full_str.clone(),
                                target_path: Some(full_str),
                                location: src.location.to_string(),
                                scope: src.scope,
                                enabled,
                                target_exists: true,
                            });
                        }
                    }
                }
            }
        }
    }

    items.sort_by(|a, b| {
        b.enabled.cmp(&a.enabled).then_with(|| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        })
    });
    items
}

/// Reads a listed startup entry and inspects the launch file without running its command.
pub fn inspect_startup_item(id: &str) -> Result<StartupInspection> {
    let (source, _) = startup_source(id)?;
    let item = list_startup_items()
        .into_iter()
        .find(|item| item.id == id)
        .with_context(|| format!("Startup item '{id}' no longer exists"))?;
    let (origin, target) = match source.kind {
        SourceKind::RegistryRun => (
            StartupOrigin::Registry {
                key_path: source.registry_path(),
                value_name: item.name,
            },
            inspection_command_path(&item.command),
        ),
        SourceKind::StartupFolder => (
            StartupOrigin::StartupFolder { path: item.command },
            item.target_path,
        ),
    };
    let target = target.map_or_else(
        || StartupTargetInspection::without_signature(None, StartupSignature::UnresolvedTarget),
        inspect_launch_file,
    );
    Ok(StartupInspection { origin, target })
}

fn startup_source(id: &str) -> Result<(&'static StartupSourceDef, &str)> {
    let (tag, name) = id
        .split_once(':')
        .ok_or_else(|| anyhow!("Invalid startup item id '{}'", id))?;
    let source = SOURCES
        .iter()
        .find(|source| source.tag == tag)
        .ok_or_else(|| anyhow!("Unknown startup source '{}'", tag))?;
    Ok((source, name))
}

fn inspection_command_path(command: &str) -> Option<String> {
    let expanded = expand_env_vars(command, None);
    let command = expanded.trim();
    let path = if let Some(quoted) = command.strip_prefix('"') {
        quoted.split_once('"')?.0
    } else {
        // A bare path containing spaces or a PATH lookup cannot identify a single file reliably.
        let first = command.split_whitespace().next()?;
        Path::new(first).extension()?;
        first
    };
    Path::new(path).is_absolute().then(|| path.to_string())
}

/// Non-destructively enables or disables a startup item by writing Windows `StartupApproved` 12-byte status.
pub fn set_startup_enabled(id: &str, enable: bool) -> Result<StartupItem> {
    let (src, name) = startup_source(id)?;
    if !list_startup_items().iter().any(|it| it.id == id) {
        return Err(anyhow!("Startup item '{}' does not exist", id));
    }

    let root = RegKey::predef(src.hive);
    let (approved_key, _) = root
        .create_subkey_with_flags(src.approved_subkey, KEY_ALL_ACCESS | src.approved_sam)
        .with_context(|| {
            format!(
                "Failed to open StartupApproved key ({}); administrator rights may be required",
                src.location
            )
        })?;

    let bytes = build_startup_approved_bytes(enable);
    let rv = RegValue {
        vtype: RegType::REG_BINARY,
        bytes,
    };
    approved_key
        .set_raw_value(name, &rv)
        .with_context(|| format!("Failed to update startup state for '{}'", name))?;

    list_startup_items()
        .into_iter()
        .find(|it| it.id == id)
        .ok_or_else(|| anyhow!("Updated startup item '{}' but could not re-read it", id))
}

fn read_approved_map(hive: HKEY, subkey: &str, sam: u32) -> HashMap<String, bool> {
    let mut map = HashMap::new();
    let root = RegKey::predef(hive);
    if let Ok(k) = root.open_subkey_with_flags(subkey, KEY_READ | sam) {
        for (name, val) in k.enum_values().flatten() {
            map.insert(
                name.to_ascii_lowercase(),
                is_startup_approved_enabled(&val.bytes),
            );
        }
    }
    map
}

pub fn is_startup_approved_enabled(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }
    // Windows StartupApproved: even first byte (e.g. 0x02, 0x06) = enabled; odd (0x03, 0x01) = disabled.
    (bytes[0] & 1) == 0
}

pub fn build_startup_approved_bytes(enable: bool) -> Vec<u8> {
    if enable {
        vec![0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    } else {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let filetime: u64 = secs
            .saturating_mul(10_000_000)
            .saturating_add(116_444_736_000_000_000);
        let mut buf = vec![0x03, 0, 0, 0];
        buf.extend_from_slice(&filetime.to_le_bytes());
        buf
    }
}

fn resolve_startup_folder(tag: &str) -> Option<PathBuf> {
    match tag {
        "user_folder" => {
            let appdata = std::env::var("APPDATA").ok()?;
            Some(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs\Startup"))
        }
        "common_folder" => {
            let progdata = std::env::var("ProgramData").ok()?;
            Some(PathBuf::from(progdata).join(r"Microsoft\Windows\Start Menu\Programs\StartUp"))
        }
        _ => None,
    }
}

pub fn extract_target_path(command: &str) -> Option<String> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }
    let raw = if let Some(rest) = trimmed.strip_prefix('"') {
        rest.split('"').next().unwrap_or("").trim().to_string()
    } else {
        let lower = trimmed.to_ascii_lowercase();
        let mut found_end = None;
        for ext in [".exe", ".bat", ".cmd", ".vbs", ".lnk", ".msc"] {
            if let Some(idx) = lower.find(ext) {
                let end = idx + ext.len();
                let next = trimmed.as_bytes().get(end).copied();
                if next.is_none() || next == Some(b' ') || next == Some(b'\t') {
                    found_end = Some(end);
                    break;
                }
            }
        }
        match found_end {
            Some(end) => trimmed[..end].trim().to_string(),
            None => trimmed.split_whitespace().next().unwrap_or("").to_string(),
        }
    };

    let expanded = expand_env_vars(&raw, None);
    if expanded.is_empty() {
        None
    } else {
        Some(expanded)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThirdPartyServiceItem {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub image_path: String,
    pub target_path: Option<String>,
    pub start_mode: String,
    pub delayed_auto: bool,
    pub target_exists: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTaskItem {
    pub path: String,
    pub name: String,
    pub command: String,
    pub enabled: bool,
    pub state: String,
}

/// Lists non-system Win32 background services from `HKLM\SYSTEM\CurrentControlSet\Services`.
pub fn list_third_party_services() -> Vec<ThirdPartyServiceItem> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let Ok(root) = hklm.open_subkey_with_flags(SERVICES_KEY, KEY_READ) else {
        return Vec::new();
    };

    let windows_dir_prefix = format!(
        r"{}\",
        windows_dir_or_default()
            .to_string_lossy()
            .to_ascii_lowercase()
    );
    let mut out = Vec::new();
    for svc_name in root.enum_keys().flatten() {
        let Ok(sk) = root.open_subkey_with_flags(&svc_name, KEY_READ) else {
            continue;
        };
        let Some(image_path) = third_party_service_image(&svc_name, &sk, &windows_dir_prefix)
        else {
            continue;
        };
        let start_val: u32 = sk.get_value("Start").unwrap_or(3);
        let start_mode = match start_val {
            2 => "auto",
            3 => "manual",
            4 => "disabled",
            _ => continue,
        }
        .to_string();

        let target_path = extract_target_path(&image_path);
        let target_exists = target_path
            .as_deref()
            .map(|p| Path::new(p).exists())
            .unwrap_or(false);

        let raw_disp = sk
            .get_raw_value("DisplayName")
            .ok()
            .map(|rv| reg_value_to_string(&rv).trim().to_string())
            .unwrap_or_default();
        let display_name = if raw_disp.is_empty() || raw_disp.starts_with('@') {
            svc_name.clone()
        } else {
            raw_disp
        };

        let raw_desc = sk
            .get_raw_value("Description")
            .ok()
            .map(|rv| reg_value_to_string(&rv).trim().to_string())
            .unwrap_or_default();
        let description = if raw_desc.starts_with('@') {
            String::new()
        } else {
            raw_desc
        };

        let delayed_auto = sk.get_value::<u32, _>("DelayedAutostart").unwrap_or(0) != 0;

        out.push(ThirdPartyServiceItem {
            name: svc_name,
            display_name,
            description,
            image_path,
            target_path,
            start_mode,
            delayed_auto,
            target_exists,
        });
    }

    out.sort_by(|a, b| {
        mode_rank(&a.start_mode)
            .cmp(&mode_rank(&b.start_mode))
            .then_with(|| {
                a.display_name
                    .to_ascii_lowercase()
                    .cmp(&b.display_name.to_ascii_lowercase())
            })
    });
    out
}

/// Changes a third-party service startup mode (`auto`, `manual`, `disabled`) via `sc.exe config`.
pub fn set_service_start_mode(
    service_name: &str,
    start_mode: &str,
) -> Result<ThirdPartyServiceItem> {
    let trimmed = service_name.trim();
    if trimmed.is_empty() || trimmed.contains(['\\', '/', '"', '\0']) {
        return Err(anyhow!("Invalid service name '{}'", trimmed));
    }

    if !crate::sysinfo::SystemInfo::collect().is_admin {
        return Err(anyhow!(
            "Changing Windows service startup mode requires Administrator privileges."
        ));
    }

    let sc_mode = match start_mode.trim().to_ascii_lowercase().as_str() {
        "auto" => "auto",
        "manual" | "demand" => "demand",
        "disabled" => "disabled",
        other => return Err(anyhow!("Unsupported service start mode '{}'", other)),
    };

    let windows_dir_prefix = format!(
        r"{}\",
        windows_dir_or_default()
            .to_string_lossy()
            .to_ascii_lowercase()
    );
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(format!(r"{SERVICES_KEY}\{trimmed}"), KEY_READ)
        .with_context(|| format!("Could not verify service '{trimmed}'"))?;
    if third_party_service_image(trimmed, &key, &windows_dir_prefix).is_none()
        || !matches!(key.get_value::<u32, _>("Start"), Ok(2..=4))
    {
        return Err(anyhow!(
            "Service '{trimmed}' is protected or is not a third-party Win32 service"
        ));
    }

    let output = crate::system_tools::system32_command("sc.exe")?
        .creation_flags(CREATE_NO_WINDOW)
        .args(["config", trimmed, "start=", sc_mode])
        .output()
        .context("Failed to execute sc.exe")?;

    let stdout = crate::systasks::decode_cmd_output(&output.stdout);
    let stderr = crate::systasks::decode_cmd_output(&output.stderr);
    let combined = format!("{}\n{}", stdout.trim(), stderr.trim())
        .trim()
        .to_string();
    if !output.status.success() {
        return Err(anyhow!("sc.exe config failed: {}", combined));
    }

    list_third_party_services()
        .into_iter()
        .find(|s| s.name.eq_ignore_ascii_case(trimmed))
        .ok_or_else(|| anyhow!("Service '{}' not found after update", trimmed))
}

static TASKS_CACHE: TtlCache<Vec<ScheduledTaskItem>> = TtlCache::new(Duration::from_secs(60));

pub fn invalidate_tasks_cache() {
    TASKS_CACHE.invalidate();
}

/// Enumerates tasks outside `\Microsoft\Windows` through the Task Scheduler COM API.
pub fn list_scheduled_tasks() -> Vec<ScheduledTaskItem> {
    TASKS_CACHE.get_or_load(compute_scheduled_tasks_uncached)
}

/// Enables or disables a third-party scheduled task via `schtasks.exe /Change`.
pub fn set_scheduled_task_enabled(task_path: &str, enabled: bool) -> Result<()> {
    let trimmed = task_path.trim();
    if !trimmed.starts_with(['\\', '/']) {
        return Err(anyhow!(
            "Refusing to modify invalid or core Windows scheduled task: '{}'",
            trimmed
        ));
    }
    let mut normalized = String::from("\\");
    for part in trimmed.replace('/', "\\").split('\\') {
        if part.is_empty() {
            continue;
        }
        if part == "." || part == ".." {
            return Err(anyhow!(
                "Refusing to modify invalid or core Windows scheduled task: '{}'",
                trimmed
            ));
        }
        if normalized.len() > 1 {
            normalized.push('\\');
        }
        normalized.push_str(part);
    }
    let lower = normalized.to_ascii_lowercase();
    if normalized == "\\"
        || lower == r"\microsoft\windows"
        || lower.starts_with(r"\microsoft\windows\")
    {
        return Err(anyhow!(
            "Refusing to modify invalid or core Windows scheduled task: '{}'",
            trimmed
        ));
    }

    let flag = if enabled { "/ENABLE" } else { "/DISABLE" };
    let output = crate::system_tools::system32_command("schtasks.exe")?
        .creation_flags(CREATE_NO_WINDOW)
        .args(["/Change", "/TN", &normalized, flag])
        .output()
        .context("Failed to execute schtasks.exe")?;

    let stdout = crate::systasks::decode_cmd_output(&output.stdout);
    let stderr = crate::systasks::decode_cmd_output(&output.stderr);
    let combined = format!("{}\n{}", stdout.trim(), stderr.trim())
        .trim()
        .to_string();
    if !output.status.success() {
        return Err(anyhow!("schtasks /Change failed: {}", combined));
    }
    invalidate_tasks_cache();
    Ok(())
}

fn third_party_service_image(name: &str, key: &RegKey, windows_dir_prefix: &str) -> Option<String> {
    if !matches!(
        key.get_value::<u32, _>("Type"),
        Ok(0x10 | 0x20 | 0x110 | 0x120)
    ) {
        return None;
    }
    let value = key.get_raw_value("ImagePath").ok()?;
    let raw = reg_value_to_string(&value);
    let image = raw.trim();
    (!image.is_empty() && !is_core_windows_service(name, image, windows_dir_prefix))
        .then(|| image.to_string())
}

fn is_core_windows_service(name: &str, image_path: &str, windows_dir_prefix: &str) -> bool {
    let lower_path = image_path
        .trim()
        .trim_start_matches('"')
        .to_ascii_lowercase()
        .replace('/', "\\");
    if lower_path.contains(r"\windows\system32\")
        || lower_path.contains(r"\windows\syswow64\")
        || lower_path.contains(r"\windows\servicing\")
        || lower_path.contains(r"\windows\microsoft.net\")
        || lower_path.starts_with(r"%systemroot%\")
        || lower_path.starts_with(r"%windir%\")
        || lower_path.starts_with(r"\systemroot\")
        || lower_path.starts_with(windows_dir_prefix)
        || lower_path.contains("svchost.exe")
        || lower_path.contains("msmpeng.exe")
        || lower_path.contains("nissrv.exe")
        || lower_path.contains("securityhealthservice.exe")
    {
        return true;
    }
    matches!(
        name.to_ascii_lowercase().as_str(),
        "wscsvc"
            | "trustedinstaller"
            | "wuauserv"
            | "bits"
            | "spooler"
            | "defender"
            | "rpcss"
            | "rpceptmapper"
            | "dcomlaunch"
            | "windefend"
            | "bfe"
            | "mpssvc"
    )
}

fn mode_rank(mode: &str) -> u8 {
    match mode {
        "auto" => 0,
        "manual" => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_startup_approved_encoding_and_target_extraction() {
        assert!(is_startup_approved_enabled(&[
            0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
        ]));
        assert!(is_startup_approved_enabled(&[0x06, 0, 0, 0]));
        assert!(!is_startup_approved_enabled(&[
            0x03, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8
        ]));

        let en = build_startup_approved_bytes(true);
        assert_eq!(en.len(), 12);
        assert!(is_startup_approved_enabled(&en));

        let dis = build_startup_approved_bytes(false);
        assert_eq!(dis.len(), 12);
        assert!(!is_startup_approved_enabled(&dis));

        assert_eq!(
            extract_target_path(r#""C:\Program Files\My App\app.exe" --minimized"#).as_deref(),
            Some(r"C:\Program Files\My App\app.exe")
        );
        assert_eq!(
            extract_target_path(r"C:\Windows\System32\OneDriveSetup.exe /thfirstsetup").as_deref(),
            Some(r"C:\Windows\System32\OneDriveSetup.exe")
        );
    }

    #[test]
    fn inspection_reports_exact_run_views_and_unambiguous_launch_files() {
        let (source, name) = startup_source("hklm_run32:Example:Updater").unwrap();
        assert_eq!(name, "Example:Updater");
        assert_eq!(
            source.registry_path(),
            r"HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Run"
        );
        assert_eq!(
            startup_source("hkcu_run:Example")
                .unwrap()
                .0
                .registry_path(),
            r"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run"
        );
        for path in [
            r#""C:\Program Files\Example\update.exe" C:\Other\tool.exe"#,
            r"C:\Apps\update.exe C:\Other\tool.exe",
        ] {
            assert!(inspection_command_path(path)
                .unwrap()
                .ends_with(r"\update.exe"));
        }
        assert!(inspection_command_path(r"C:\Program Files\Example\update.exe").is_none());
        assert!(inspection_command_path("cmd.exe /c update.cmd").is_none());
        assert!(inspection_command_path(r#""C:\Apps\update.exe"#).is_none());
    }
}
