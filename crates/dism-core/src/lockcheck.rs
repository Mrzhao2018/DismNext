use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS, FILETIME};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CCH_RM_SESSION_KEY: usize = 32;
const CCH_RM_MAX_APP_NAME: usize = 255;
const CCH_RM_MAX_SVC_NAME: usize = 63;

#[repr(C)]
#[derive(Clone, Copy)]
struct RmUniqueProcess {
    dw_process_id: u32,
    process_start_time: FILETIME,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RmProcessInfo {
    process: RmUniqueProcess,
    str_app_name: [u16; CCH_RM_MAX_APP_NAME + 1],
    str_service_short_name: [u16; CCH_RM_MAX_SVC_NAME + 1],
    application_type: u32,
    app_status: u32,
    ts_session_id: u32,
    b_restartable: i32,
}

impl Default for RmProcessInfo {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

#[link(name = "rstrtmgr")]
extern "system" {
    fn RmStartSession(
        p_session_handle: *mut u32,
        dw_session_flags: u32,
        str_session_key: *mut u16,
    ) -> u32;

    fn RmRegisterResources(
        dw_session_handle: u32,
        n_files: u32,
        rgs_file_names: *const *const u16,
        n_applications: u32,
        rg_applications: *const RmUniqueProcess,
        n_services: u32,
        rgs_service_names: *const *const u16,
    ) -> u32;

    fn RmGetList(
        dw_session_handle: u32,
        pn_proc_info_needed: *mut u32,
        pn_proc_info: *mut u32,
        rg_affected_apps: *mut RmProcessInfo,
        lpdw_reboot_reasons: *mut u32,
    ) -> u32;

    fn RmEndSession(dw_session_handle: u32) -> u32;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockingProcessInfo {
    pub pid: u32,
    pub app_name: String,
    pub service_name: Option<String>,
    pub app_type: String,
    pub is_critical: bool,
}

/// Uses Windows Restart Manager (`rstrtmgr.dll`) to identify processes holding an open lock on `path`.
pub fn find_locking_processes(path: &Path) -> Vec<LockingProcessInfo> {
    if !path.is_file() {
        return Vec::new();
    }

    let mut session_handle: u32 = 0;
    let mut session_key: [u16; CCH_RM_SESSION_KEY + 1] = [0; CCH_RM_SESSION_KEY + 1];
    let rc = unsafe { RmStartSession(&mut session_handle, 0, session_key.as_mut_ptr()) };
    if rc != ERROR_SUCCESS {
        return Vec::new();
    }

    struct SessionGuard(u32);
    impl Drop for SessionGuard {
        fn drop(&mut self) {
            unsafe {
                RmEndSession(self.0);
            }
        }
    }
    let _guard = SessionGuard(session_handle);

    let wide_path: Vec<u16> = OsStr::new(path).encode_wide().chain(Some(0)).collect();
    let ptr_list = [wide_path.as_ptr()];
    let reg_rc = unsafe {
        RmRegisterResources(
            session_handle,
            1,
            ptr_list.as_ptr(),
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
        )
    };
    if reg_rc != ERROR_SUCCESS {
        return Vec::new();
    }

    let mut needed: u32 = 0;
    let mut buf = vec![RmProcessInfo::default(); 16];
    let mut count: u32 = buf.len() as u32;
    let mut reboot_reasons: u32 = 0;

    let mut list_rc = unsafe {
        RmGetList(
            session_handle,
            &mut needed,
            &mut count,
            buf.as_mut_ptr(),
            &mut reboot_reasons,
        )
    };

    if list_rc == ERROR_MORE_DATA && needed > 0 {
        let cap = (needed as usize).min(64);
        buf = vec![RmProcessInfo::default(); cap];
        count = cap as u32;
        list_rc = unsafe {
            RmGetList(
                session_handle,
                &mut needed,
                &mut count,
                buf.as_mut_ptr(),
                &mut reboot_reasons,
            )
        };
    }

    if list_rc != ERROR_SUCCESS {
        return Vec::new();
    }

    let self_pid = std::process::id();
    let mut seen_pids = HashSet::new();
    let mut results = Vec::new();

    for info in buf.into_iter().take(count as usize) {
        let pid = info.process.dw_process_id;
        if pid == 0 || !seen_pids.insert(pid) {
            continue;
        }
        let app_name = utf16_z_to_string(&info.str_app_name);
        let svc_raw = utf16_z_to_string(&info.str_service_short_name);
        let service_name = if svc_raw.is_empty() {
            None
        } else {
            Some(svc_raw)
        };
        let app_type = match info.application_type {
            1 => "MainWindow",
            2 => "OtherWindow",
            3 => "Service",
            4 => "Explorer",
            5 => "Console",
            1000 => "Critical",
            _ => "Unknown",
        }
        .to_string();

        let is_critical = is_critical_process(pid, self_pid, info.application_type, &app_name);
        let display_app = if app_name.is_empty() {
            service_name
                .clone()
                .unwrap_or_else(|| format!("PID {}", pid))
        } else {
            app_name
        };

        results.push(LockingProcessInfo {
            pid,
            app_name: display_app,
            service_name,
            app_type,
            is_critical,
        });
    }

    results
}

/// Safely terminates a non-critical user-space process holding a lock on a cleanup target.
pub fn terminate_locking_process(pid: u32) -> Result<String> {
    let self_pid = std::process::id();
    if pid <= 4 || pid == self_pid {
        return Err(anyhow!(
            "Refusing to terminate system core or host process (PID {})",
            pid
        ));
    }

    let pid_str = pid.to_string();
    let output = Command::new("taskkill.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["/PID", &pid_str, "/F"])
        .output()
        .context("Failed to execute taskkill.exe")?;

    let stdout = crate::systasks::decode_cmd_output(&output.stdout);
    let stderr = crate::systasks::decode_cmd_output(&output.stderr);
    let combined = format!("{}\n{}", stdout.trim(), stderr.trim()).trim().to_string();
    if !output.status.success() {
        return Err(anyhow!("taskkill PID {} failed: {}", pid, combined));
    }
    Ok(if combined.is_empty() {
        format!("Terminated PID {}", pid)
    } else {
        combined
    })
}

fn is_critical_process(pid: u32, self_pid: u32, app_type: u32, app_name: &str) -> bool {
    if pid <= 4 || pid == self_pid {
        return true;
    }
    if matches!(app_type, 3 | 4 | 1000) {
        return true;
    }
    let lower = app_name.trim().to_ascii_lowercase();
    const PROTECTED_PREFIXES: &[&str] = &[
        "system",
        "smss",
        "csrss",
        "wininit",
        "winlogon",
        "services",
        "lsass",
        "svchost",
        "dwm",
        "explorer",
        "windows explorer",
        "dism-app",
        "dism-cli",
        "securityhealthservice",
        "msmpeng",
    ];
    PROTECTED_PREFIXES
        .iter()
        .any(|p| lower == *p || lower.starts_with(&format!("{}.exe", p)) || lower.contains(p))
}

fn utf16_z_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len]).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::windows::fs::OpenOptionsExt;

    #[test]
    fn test_find_locking_processes_and_critical_guard() {
        let p = std::env::temp_dir().join(format!("dismnext_lock_test_{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&p);

        // Hold an exclusive handle (`share_mode(0)`) to verify Restart Manager executes cleanly
        {
            let mut f = OpenOptions::new()
                .create(true)
                .write(true)
                .share_mode(0)
                .open(&p)
                .unwrap();
            f.write_all(b"lock").unwrap();
            let procs = find_locking_processes(&p);
            // RmGetList completes without crash even on short-lived self lock
            let _ = procs;
        }
        let _ = std::fs::remove_file(&p);

        assert!(terminate_locking_process(4).is_err());
        assert!(terminate_locking_process(std::process::id()).is_err());
    }
}
