use crate::file_stamp::{delete_open_target, open_for_verification};
use crate::maintenance::{run_system_command, MaintenanceProgress};
use crate::win_util::{
    has_reparse_point_in_ancestors, pin_directory, CREATE_NO_WINDOW, FILE_ATTRIBUTE_REPARSE_POINT,
};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_SHARE_READ};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT,
    SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS,
};
use windows_sys::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestorePointInfo {
    pub sequence_number: u32,
    pub description: String,
    pub creation_time: String,
    pub event_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RescueActionOutcome {
    pub action_id: String,
    pub success: bool,
    pub requires_restart: bool,
    pub output: String,
}

/// Lists existing Windows System Restore points (returns empty list if unavailable or non-elevated).
pub fn list_restore_points() -> Vec<RestorePointInfo> {
    let ps_cmd = "Get-ComputerRestorePoint -ErrorAction SilentlyContinue | Select-Object SequenceNumber,Description,CreationTime,RestorePointType,EventType | ConvertTo-Json -Compress";
    let Ok(output) = crate::system_tools::powershell_command().and_then(|mut command| {
        command
            .creation_flags(CREATE_NO_WINDOW)
            .args(["-NoProfile", "-NonInteractive", "-Command", ps_cmd])
            .output()
    }) else {
        return Vec::new();
    };

    if !output.status.success() {
        return Vec::new();
    }

    let raw = crate::systasks::decode_cmd_output(&output.stdout)
        .trim()
        .to_string();
    if raw.is_empty() || raw == "null" {
        return Vec::new();
    }

    let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let arr = match val {
        serde_json::Value::Array(a) => a,
        obj @ serde_json::Value::Object(_) => vec![obj],
        _ => return Vec::new(),
    };

    let mut points = Vec::new();
    for item in arr {
        let seq = item
            .get("SequenceNumber")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let desc = item
            .get("Description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let ctime = item
            .get("CreationTime")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let event_type = format_restore_point_type(
            item.get("RestorePointType").and_then(|v| v.as_u64()),
            item.get("EventType").and_then(|v| v.as_u64()),
        );
        points.push(RestorePointInfo {
            sequence_number: seq,
            description: desc,
            creation_time: format_wmi_time(&ctime),
            event_type,
        });
    }
    points.sort_by_key(|entry| std::cmp::Reverse(entry.sequence_number));
    points
}

fn format_restore_point_type(rp_type: Option<u64>, event_type: Option<u64>) -> String {
    match rp_type {
        Some(0) => "APPLICATION_INSTALL".to_string(),
        Some(1) => "APPLICATION_UNINSTALL".to_string(),
        Some(10) => "DEVICE_DRIVER_INSTALL".to_string(),
        Some(12) => "MODIFY_SETTINGS".to_string(),
        Some(13) => "CANCELLED_OPERATION".to_string(),
        Some(other) => format!("TYPE_{other}"),
        None => match event_type {
            Some(100) => "BEGIN_SYSTEM_CHANGE".to_string(),
            Some(101) => "END_SYSTEM_CHANGE".to_string(),
            Some(102) => "BEGIN_NESTED_SYSTEM_CHANGE".to_string(),
            Some(103) => "END_NESTED_SYSTEM_CHANGE".to_string(),
            _ => "MODIFY_SETTINGS".to_string(),
        },
    }
}

fn sanitize_restore_point_description(description: &str) -> String {
    let clean: String = description
        .trim()
        .chars()
        .filter(|c| !c.is_control() && *c != '\0')
        .take(96)
        .collect();
    if clean.is_empty() {
        format!(
            "DismNext Checkpoint {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M")
        )
    } else {
        clean
    }
}

/// Creates a new Windows System Restore point with the given description (Administrator required).
pub fn create_restore_point(description: &str) -> Result<String> {
    if !crate::sysinfo::SystemInfo::collect().is_admin {
        return Err(anyhow!(
            "Creating a Windows System Restore Point requires Administrator privileges."
        ));
    }

    let final_desc = sanitize_restore_point_description(description);
    let ps_script = "$ErrorActionPreference='Stop'; Checkpoint-Computer -Description $env:DISMNEXT_RP_DESC -RestorePointType 'MODIFY_SETTINGS'";
    let output = crate::system_tools::powershell_command()?
        .creation_flags(CREATE_NO_WINDOW)
        .env("DISMNEXT_RP_DESC", &final_desc)
        .args(["-NoProfile", "-NonInteractive", "-Command", ps_script])
        .output()
        .context("Failed to execute PowerShell Checkpoint-Computer")?;

    let stdout = crate::systasks::decode_cmd_output(&output.stdout);
    let stderr = crate::systasks::decode_cmd_output(&output.stderr);
    let combined = format!("{}\n{}", stdout.trim(), stderr.trim())
        .trim()
        .to_string();
    if !output.status.success() {
        return Err(anyhow!(
            "Checkpoint-Computer failed (ensure System Protection is enabled on C: or wait for Windows restore frequency cooldown): {}",
            combined
        ));
    }

    Ok(format!("Created System Restore Point: {}", final_desc))
}

/// Executes a targeted Windows rescue / repair operation cleanly without hidden global side effects.
pub fn run_rescue_action(action_id: &str) -> Result<RescueActionOutcome> {
    run_rescue_action_impl(action_id, None)
}

pub fn run_rescue_action_with_progress(
    action_id: &str,
    progress: &MaintenanceProgress,
) -> Result<RescueActionOutcome> {
    run_rescue_action_impl(action_id, Some(progress))
}

fn run_rescue_action_impl(
    action_id: &str,
    progress: Option<&MaintenanceProgress>,
) -> Result<RescueActionOutcome> {
    if let Some(progress) = progress {
        progress.check()?;
    }
    let sys = crate::sysinfo::SystemInfo::collect();

    match action_id.trim() {
        "rebuild_icon_cache" => {
            let local_app = std::env::var("LOCALAPPDATA").unwrap_or_default();
            let mut removed = 0usize;
            if !local_app.is_empty() {
                let single = PathBuf::from(&local_app).join("IconCache.db");
                if remove_unlocked_regular_file(&single) {
                    removed += 1;
                }
                let explorer_dir = PathBuf::from(&local_app).join(r"Microsoft\Windows\Explorer");
                if !has_reparse_point_in_ancestors(&explorer_dir, true) {
                    if let Ok(rd) = fs::read_dir(&explorer_dir) {
                        for entry in rd.flatten() {
                            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                            if (name.starts_with("iconcache") || name.starts_with("thumbcache"))
                                && name.ends_with(".db")
                                && remove_unlocked_regular_file(&entry.path())
                            {
                                removed += 1;
                            }
                        }
                    }
                }
            }
            unsafe {
                SHChangeNotify(
                    SHCNE_ASSOCCHANGED as i32,
                    SHCNF_IDLIST,
                    std::ptr::null(),
                    std::ptr::null(),
                );
            }
            Ok(RescueActionOutcome {
                action_id: action_id.to_string(),
                success: true,
                requires_restart: false,
                output: format!(
                    "已清理 {} 个未占用的图标/缩略图缓存文件并触发外壳图标刷新通知。",
                    removed
                ),
            })
        }
        "flush_dns_network" => {
            let mut logs = Vec::new();
            logs.push(exec_simple("ipconfig.exe", &["/flushdns"], progress)?);
            if sys.is_admin {
                logs.push(exec_simple("netsh.exe", &["winsock", "reset"], progress)?);
            } else {
                logs.push(
                    "Skipped `netsh winsock reset` (standard user mode; DNS cache flushed)."
                        .to_string(),
                );
            }
            Ok(RescueActionOutcome {
                action_id: action_id.to_string(),
                success: true,
                requires_restart: sys.is_admin,
                output: logs.join("\n"),
            })
        }
        "reset_windows_update" => {
            require_admin(&sys, "Resetting Windows Update cache")?;
            if let Some(progress) = progress {
                progress.check()?;
            }
            let dl_dir = crate::system_tools::windows_dir()?.join(r"SoftwareDistribution\Download");
            let wuauserv_running = is_service_running("wuauserv");
            let bits_running = is_service_running("bits");
            let mut notes = Vec::new();
            if wuauserv_running {
                if let Err(err) = exec_simple("net.exe", &["stop", "wuauserv"], progress) {
                    notes.push(format!("stop wuauserv warning: {err}"));
                }
            }
            if bits_running {
                if let Err(err) = exec_simple("net.exe", &["stop", "bits"], progress) {
                    notes.push(format!("stop bits warning: {err}"));
                }
            }

            let deleted = purge_unlocked_contents(&dl_dir);

            if bits_running {
                if let Err(err) = exec_simple("net.exe", &["start", "bits"], None) {
                    notes.push(format!("start bits warning: {err}"));
                }
            }
            if wuauserv_running {
                if let Err(err) = exec_simple("net.exe", &["start", "wuauserv"], None) {
                    notes.push(format!("start wuauserv warning: {err}"));
                }
            }
            let deleted = deleted?;
            let mut summary = format!(
                "Windows Update cache cleanup cleared {} cached download entries in {}.",
                deleted,
                dl_dir.display()
            );
            if !notes.is_empty() {
                summary.push('\n');
                summary.push_str(&notes.join("\n"));
            }

            Ok(RescueActionOutcome {
                action_id: action_id.to_string(),
                success: true,
                requires_restart: false,
                output: summary,
            })
        }
        "clear_print_spooler" => {
            require_admin(&sys, "Clearing Print Spooler queue")?;
            if let Some(progress) = progress {
                progress.check()?;
            }
            let spool_dir = crate::system_tools::system32_dir()?.join(r"spool\PRINTERS");
            let spooler_running = is_service_running("Spooler");
            let mut notes = Vec::new();
            if spooler_running {
                if let Err(err) = exec_simple("net.exe", &["stop", "Spooler"], progress) {
                    notes.push(format!("stop Spooler warning: {err}"));
                }
            }
            let deleted = purge_unlocked_contents(&spool_dir);
            if spooler_running {
                if let Err(err) = exec_simple("net.exe", &["start", "Spooler"], None) {
                    notes.push(format!("start Spooler warning: {err}"));
                }
            }
            let deleted = deleted?;
            let mut summary = format!("Print Spooler cleared {} stuck spool file(s).", deleted);
            if !notes.is_empty() {
                summary.push('\n');
                summary.push_str(&notes.join("\n"));
            }
            Ok(RescueActionOutcome {
                action_id: action_id.to_string(),
                success: true,
                requires_restart: false,
                output: summary,
            })
        }
        "dism_check_health" => {
            require_admin(&sys, "DISM /CheckHealth")?;
            let out = exec_simple(
                "dism.exe",
                &["/Online", "/Cleanup-Image", "/CheckHealth"],
                progress,
            )?;
            Ok(RescueActionOutcome {
                action_id: action_id.to_string(),
                success: true,
                requires_restart: false,
                output: out,
            })
        }
        "dism_restore_health" => {
            require_admin(&sys, "DISM /RestoreHealth")?;
            let out = exec_simple(
                "dism.exe",
                &["/Online", "/Cleanup-Image", "/RestoreHealth"],
                progress,
            )?;
            Ok(RescueActionOutcome {
                action_id: action_id.to_string(),
                success: true,
                requires_restart: true,
                output: out,
            })
        }
        "sfc_scannow" => {
            require_admin(&sys, "SFC /scannow")?;
            let out = exec_simple("sfc.exe", &["/scannow"], progress)?;
            Ok(RescueActionOutcome {
                action_id: action_id.to_string(),
                success: true,
                requires_restart: false,
                output: out,
            })
        }
        other => Err(anyhow!("Unknown rescue action '{}'", other)),
    }
}

fn require_admin(sys: &crate::sysinfo::SystemInfo, label: &str) -> Result<()> {
    if !sys.is_admin {
        return Err(anyhow!("{} requires Administrator privileges.", label));
    }
    Ok(())
}

fn is_service_running(name: &str) -> bool {
    let wide_name: Vec<u16> = std::ffi::OsStr::new(name)
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return false;
        }
        let svc = OpenServiceW(scm, wide_name.as_ptr(), SERVICE_QUERY_STATUS);
        if svc.is_null() {
            CloseServiceHandle(scm);
            return false;
        }
        let mut status: SERVICE_STATUS = std::mem::zeroed();
        let ok = QueryServiceStatus(svc, &mut status);
        CloseServiceHandle(svc);
        CloseServiceHandle(scm);
        ok != 0 && status.dwCurrentState == SERVICE_RUNNING
    }
}

fn remove_unlocked_regular_file(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let Ok(_parents) = pin_directory(parent, false) else {
        return false;
    };
    let Ok(file) = open_for_verification(path, false, DELETE, FILE_SHARE_READ) else {
        return false;
    };
    let Ok(meta) = file.metadata() else {
        return false;
    };
    meta.is_file()
        && (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) == 0
        && delete_open_target(&file, false).is_ok()
}

fn exec_simple(
    program: &str,
    args: &[&str],
    progress: Option<&MaintenanceProgress>,
) -> Result<String> {
    let result = run_system_command(program, args, progress)?;
    if result.output.is_empty() {
        Ok(format!(
            "{program} completed cleanly (code {}).",
            result.code
        ))
    } else {
        Ok(result.output)
    }
}

fn purge_unlocked_contents(dir: &Path) -> Result<usize> {
    let _parents = match pin_directory(dir, false) {
        Ok(handles) => handles,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(error).with_context(|| format!("Could not open {}", dir.display()))
        }
    };
    Ok(purge_children(dir)?)
}

fn purge_children(dir: &Path) -> io::Result<usize> {
    let mut removed = 0;
    for entry in fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        let Ok(file) = open_for_verification(&path, true, DELETE, FILE_SHARE_READ) else {
            continue;
        };
        let Ok(metadata) = file.metadata() else {
            continue;
        };
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            continue;
        }
        let directory = metadata.is_dir();
        let child_removed = if directory {
            match purge_children(&path) {
                Ok(count) => count,
                Err(_) => continue,
            }
        } else {
            0
        };
        removed += child_removed;
        if (directory || metadata.is_file()) && delete_open_target(&file, directory).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

fn format_wmi_time(raw: &str) -> String {
    // Format: 20261118123045.000000+480 or ISO string
    let digits: String = raw.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 14 {
        format!(
            "{}-{}-{} {}:{}",
            &digits[0..4],
            &digits[4..6],
            &digits[6..8],
            &digits[8..10],
            &digits[10..12]
        )
    } else {
        raw.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    #[test]
    fn test_rescue_validation_and_wmi_time_format() {
        assert_eq!(
            format_wmi_time("20261119051022.000000+480"),
            "2026-11-19 05:10"
        );
        assert!(run_rescue_action("unknown_action").is_err());
    }

    #[test]
    fn purge_removes_unlocked_entries_and_keeps_locked_files() {
        let root = std::env::temp_dir().join(format!("dismnext-rescue-{}", std::process::id()));
        let folder = root.join("nested");
        let locked_path = root.join("locked.txt");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("cache.txt"), "nested").unwrap();
        fs::write(root.join("cache.txt"), "top-level").unwrap();
        let lock = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(&locked_path)
            .unwrap();

        let result = purge_unlocked_contents(&root);
        let kept_lock = locked_path.is_file();
        let kept_folder = folder.exists();
        drop(lock);
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(result.unwrap(), 3);
        assert!(kept_lock);
        assert!(!kept_folder);
    }
}
