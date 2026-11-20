use crate::file_stamp::{delete_open_target, open_for_verification};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::os::windows::fs::MetadataExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_SHARE_READ,
};
use windows_sys::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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
    let ps_cmd = "Get-ComputerRestorePoint -ErrorAction SilentlyContinue | Select-Object SequenceNumber,Description,CreationTime | ConvertTo-Json -Compress";
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
        points.push(RestorePointInfo {
            sequence_number: seq,
            description: desc,
            creation_time: format_wmi_time(&ctime),
            event_type: "MODIFY_SETTINGS".to_string(),
        });
    }
    points.sort_by(|a, b| b.sequence_number.cmp(&a.sequence_number));
    points
}

/// Creates a new Windows System Restore point with the given description (Administrator required).
pub fn create_restore_point(description: &str) -> Result<String> {
    if !crate::sysinfo::SystemInfo::collect().is_admin {
        return Err(anyhow!(
            "Creating a Windows System Restore Point requires Administrator privileges. Please click '一键提权为管理员' first."
        ));
    }

    let clean_desc: String = description
        .trim()
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '`' | '$' | ';' | '\r' | '\n'))
        .take(96)
        .collect();
    let final_desc = if clean_desc.is_empty() {
        format!("DismNext Checkpoint {}", chrono::Local::now().format("%Y-%m-%d %H:%M"))
    } else {
        clean_desc
    };

    let ps_script = format!(
        "$ErrorActionPreference='Stop'; Checkpoint-Computer -Description '{}' -RestorePointType 'MODIFY_SETTINGS'",
        final_desc
    );
    let output = crate::system_tools::powershell_command()?
        .creation_flags(CREATE_NO_WINDOW)
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps_script])
        .output()
        .context("Failed to execute PowerShell Checkpoint-Computer")?;

    let stdout = crate::systasks::decode_cmd_output(&output.stdout);
    let stderr = crate::systasks::decode_cmd_output(&output.stderr);
    let combined = format!("{}\n{}", stdout.trim(), stderr.trim()).trim().to_string();
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
    let sys = crate::sysinfo::SystemInfo::collect();

    match action_id.trim() {
        "rebuild_icon_cache" => {
            let local_app = std::env::var("LOCALAPPDATA").unwrap_or_default();
            let mut removed = 0usize;
            if !local_app.is_empty() {
                let single = PathBuf::from(&local_app).join("IconCache.db");
                if single.is_file() && fs::remove_file(&single).is_ok() {
                    removed += 1;
                }
                let explorer_dir = PathBuf::from(&local_app).join(r"Microsoft\Windows\Explorer");
                if let Ok(rd) = fs::read_dir(&explorer_dir) {
                    for entry in rd.flatten() {
                        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                        if (name.starts_with("iconcache") || name.starts_with("thumbcache"))
                            && name.ends_with(".db")
                            && fs::remove_file(entry.path()).is_ok()
                        {
                            removed += 1;
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
            logs.push(exec_simple("ipconfig.exe", &["/flushdns"])?);
            if sys.is_admin {
                if let Ok(ws) = exec_simple("netsh.exe", &["winsock", "reset"]) {
                    logs.push(ws);
                }
            } else {
                logs.push("Skipped `netsh winsock reset` (standard user mode; DNS cache flushed).".to_string());
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
            let dl_dir = crate::system_tools::windows_dir()?.join(r"SoftwareDistribution\Download");
            let _ = exec_simple("net.exe", &["stop", "wuauserv"]);
            let _ = exec_simple("net.exe", &["stop", "bits"]);

            let deleted = purge_unlocked_contents(&dl_dir);

            let _ = exec_simple("net.exe", &["start", "bits"]);
            let _ = exec_simple("net.exe", &["start", "wuauserv"]);
            let deleted = deleted?;

            Ok(RescueActionOutcome {
                action_id: action_id.to_string(),
                success: true,
                requires_restart: false,
                output: format!(
                    "Windows Update services restarted and cleared {} cached download entries in {}.",
                    deleted,
                    dl_dir.display()
                ),
            })
        }
        "clear_print_spooler" => {
            require_admin(&sys, "Clearing Print Spooler queue")?;
            let spool_dir = crate::system_tools::system32_dir()?.join(r"spool\PRINTERS");
            let _ = exec_simple("net.exe", &["stop", "Spooler"]);
            let deleted = purge_unlocked_contents(&spool_dir);
            let _ = exec_simple("net.exe", &["start", "Spooler"]);
            let deleted = deleted?;
            Ok(RescueActionOutcome {
                action_id: action_id.to_string(),
                success: true,
                requires_restart: false,
                output: format!(
                    "Print Spooler restarted and cleared {} stuck spool file(s).",
                    deleted
                ),
            })
        }
        "dism_check_health" => {
            require_admin(&sys, "DISM /CheckHealth")?;
            let out = exec_simple("dism.exe", &["/Online", "/Cleanup-Image", "/CheckHealth"])?;
            Ok(RescueActionOutcome {
                action_id: action_id.to_string(),
                success: true,
                requires_restart: false,
                output: out,
            })
        }
        "dism_restore_health" => {
            require_admin(&sys, "DISM /RestoreHealth")?;
            let out = exec_simple("dism.exe", &["/Online", "/Cleanup-Image", "/RestoreHealth"])?;
            Ok(RescueActionOutcome {
                action_id: action_id.to_string(),
                success: true,
                requires_restart: true,
                output: out,
            })
        }
        "sfc_scannow" => {
            require_admin(&sys, "SFC /scannow")?;
            let out = exec_simple("sfc.exe", &["/scannow"])?;
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
        return Err(anyhow!(
            "{} requires Administrator privileges. Please click '一键提权为管理员' first.",
            label
        ));
    }
    Ok(())
}

fn exec_simple(program: &str, args: &[&str]) -> Result<String> {
    let output = crate::system_tools::system32_command(program)?
        .creation_flags(CREATE_NO_WINDOW)
        .args(args)
        .output()
        .with_context(|| format!("Failed to launch {}", program))?;

    let stdout = crate::systasks::decode_cmd_output(&output.stdout);
    let stderr = crate::systasks::decode_cmd_output(&output.stderr);
    let combined = format!("{}\n{}", stdout.trim(), stderr.trim()).trim().to_string();
    let code = output.status.code().unwrap_or(-1);
    if !output.status.success() && code != 3010 {
        return Err(anyhow!("{} exited with code {}: {}", program, code, combined));
    }
    Ok(if combined.is_empty() {
        format!("{} completed cleanly (code {}).", program, code)
    } else {
        combined
    })
}

fn purge_unlocked_contents(dir: &Path) -> Result<usize> {
    let ancestors: Vec<_> = dir.ancestors().collect();
    let mut pinned = Vec::with_capacity(ancestors.len());
    // Hold each verified directory without delete sharing while resolving its children.
    for path in ancestors.into_iter().rev() {
        let file = match open_for_verification(path, true, 0, FILE_SHARE_READ) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => {
                return Err(error).with_context(|| format!("Could not open {}", path.display()))
            }
        };
        let metadata = file.metadata()?;
        anyhow::ensure!(
            metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "Refusing to clean through a reparse point or non-directory at {}",
            path.display()
        );
        pinned.push(file);
    }
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
        if directory && purge_children(&path).is_err() {
            continue;
        }
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
        assert_eq!(format_wmi_time("20261119051022.000000+480"), "2026-11-19 05:10");
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
        assert_eq!(result.unwrap(), 2);
        assert!(kept_lock);
        assert!(!kept_folder);
    }
}
