use crate::evaluator::{expand_env_vars, reg_value_to_string};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use winreg::enums::{
    RegType, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_ALL_ACCESS, KEY_READ, KEY_WOW64_32KEY,
    KEY_WOW64_64KEY,
};
use winreg::{RegKey, RegValue, HKEY};

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
    sam: u32,
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
        sam: KEY_WOW64_64KEY,
    },
    StartupSourceDef {
        tag: "hklm_run",
        location: "HKLM Run",
        scope: StartupScope::System,
        kind: SourceKind::RegistryRun,
        hive: HKEY_LOCAL_MACHINE,
        run_subkey: r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run",
        approved_subkey: r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run",
        sam: KEY_WOW64_64KEY,
    },
    StartupSourceDef {
        tag: "hklm_run32",
        location: "HKLM Run (32-bit)",
        scope: StartupScope::System,
        kind: SourceKind::RegistryRun,
        hive: HKEY_LOCAL_MACHINE,
        run_subkey: r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run",
        approved_subkey: r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run32",
        sam: KEY_WOW64_32KEY,
    },
    StartupSourceDef {
        tag: "user_folder",
        location: "User Startup Folder",
        scope: StartupScope::User,
        kind: SourceKind::StartupFolder,
        hive: HKEY_CURRENT_USER,
        run_subkey: "",
        approved_subkey:
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder",
        sam: KEY_WOW64_64KEY,
    },
    StartupSourceDef {
        tag: "common_folder",
        location: "Common Startup Folder",
        scope: StartupScope::System,
        kind: SourceKind::StartupFolder,
        hive: HKEY_LOCAL_MACHINE,
        run_subkey: "",
        approved_subkey:
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder",
        sam: KEY_WOW64_64KEY,
    },
];

/// Lists all Windows startup items from Registry Run keys and Startup folders,
/// merged with `StartupApproved` enabled/disabled status.
pub fn list_startup_items() -> Vec<StartupItem> {
    let mut items = Vec::new();
    for src in SOURCES {
        let approved_map = read_approved_map(src.hive, src.approved_subkey, src.sam);
        match src.kind {
            SourceKind::RegistryRun => {
                let root = RegKey::predef(src.hive);
                if let Ok(k) = root.open_subkey_with_flags(src.run_subkey, KEY_READ | src.sam) {
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
        b.enabled
            .cmp(&a.enabled)
            .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
    });
    items
}

/// Non-destructively enables or disables a startup item by writing Windows `StartupApproved` 12-byte status.
pub fn set_startup_enabled(id: &str, enable: bool) -> Result<StartupItem> {
    let (tag, name) = id
        .split_once(':')
        .ok_or_else(|| anyhow!("Invalid startup item id '{}'", id))?;
    let src = SOURCES
        .iter()
        .find(|s| s.tag == tag)
        .ok_or_else(|| anyhow!("Unknown startup source '{}'", tag))?;

    let root = RegKey::predef(src.hive);
    let (approved_key, _) = root
        .create_subkey_with_flags(src.approved_subkey, KEY_ALL_ACCESS | src.sam)
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
        let filetime: u64 = secs.saturating_mul(10_000_000).saturating_add(116_444_736_000_000_000);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_startup_approved_encoding_and_target_extraction() {
        assert!(is_startup_approved_enabled(&[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(is_startup_approved_enabled(&[0x06, 0, 0, 0]));
        assert!(!is_startup_approved_enabled(&[0x03, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8]));

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
}
