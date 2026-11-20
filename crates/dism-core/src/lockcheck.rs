use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS, FILETIME};

use windows_sys::Win32::System::Threading::{
    GetProcessTimes, IsProcessCritical, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};
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
    pub process_start_time: String,
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
    find_locking_processes_batch(&[path])
}

/// Identifies all processes locking any file in `paths` within a single Restart Manager session.
pub fn find_locking_processes_batch(paths: &[&Path]) -> Vec<LockingProcessInfo> {
    let valid: Vec<&Path> = paths.iter().copied().filter(|p| p.is_file()).collect();
    if valid.is_empty() {
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

    let wide_paths: Vec<Vec<u16>> = valid
        .iter()
        .map(|p| OsStr::new(p).encode_wide().chain(Some(0)).collect())
        .collect();
    let ptr_list: Vec<*const u16> = wide_paths.iter().map(|w| w.as_ptr()).collect();
    let reg_rc = unsafe {
        RmRegisterResources(
            session_handle,
            ptr_list.len() as u32,
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

    for _ in 0..5 {
        if list_rc != ERROR_MORE_DATA || needed as usize <= buf.len() || needed > 65_536 {
            break;
        }
        buf.resize(needed as usize, RmProcessInfo::default());
        count = needed;
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
            process_start_time: filetime_ticks(info.process.process_start_time).to_string(),
            app_name: display_app,
            service_name,
            app_type,
            is_critical,
        });
    }

    results
}

/// Safely terminates a non-critical user-space process holding a lock on a cleanup target.
pub fn terminate_locking_process(path: &Path, pid: u32, process_start_time: u64) -> Result<String> {
    if is_critical_process(pid, std::process::id(), 0, "") {
        return Err(anyhow!(
            "Refusing to terminate system core or host process (PID {pid})"
        ));
    }
    let handle = unsafe {
        OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        )
    };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error()).context("Could not open the locking process");
    }
    let process = unsafe { OwnedHandle::from_raw_handle(handle) };
    let handle = process.as_raw_handle();
    let (mut creation, mut exit, mut kernel, mut user) = unsafe { std::mem::zeroed() };
    if unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("Could not verify process creation time");
    }

    let mut critical = 0;
    if unsafe { IsProcessCritical(handle, &mut critical) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("Could not check for a critical process");
    }
    let mut image = vec![0; 32_768];
    let mut len = image.len() as u32;
    if unsafe { QueryFullProcessImageNameW(handle, 0, image.as_mut_ptr(), &mut len) } == 0 {
        return Err(std::io::Error::last_os_error()).context("Could not verify the process image");
    }
    let image = String::from_utf16_lossy(&image[..len as usize]);
    let executable = Path::new(&image)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("");
    if critical != 0
        || executable.is_empty()
        || is_critical_process(pid, std::process::id(), 0, executable)
    {
        return Err(anyhow!(
            "Refusing to terminate protected process (PID {pid})"
        ));
    }

    let creation_time = filetime_ticks(creation);
    let start_time = creation_time.to_string();
    if process_start_time != creation_time
        || !find_locking_processes(path).iter().any(|entry| {
            entry.pid == pid && entry.process_start_time == start_time && !entry.is_critical
        })
    {
        return Err(anyhow!(
            "The lock changed. Inspect the file again before terminating a process"
        ));
    }
    if unsafe { TerminateProcess(handle, 1) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("Could not terminate the locking process");
    }
    Ok(format!("Terminated PID {pid}"))
}

fn filetime_ticks(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

fn is_critical_process(pid: u32, self_pid: u32, app_type: u32, app_name: &str) -> bool {
    if pid <= 4 || pid == self_pid || matches!(app_type, 3 | 4 | 1000) {
        return true;
    }
    let lower = app_name.trim().to_ascii_lowercase();
    let name = lower.strip_suffix(".exe").unwrap_or(&lower);
    matches!(
        name,
        "system"
            | "smss"
            | "csrss"
            | "wininit"
            | "winlogon"
            | "services"
            | "lsass"
            | "lsaiso"
            | "svchost"
            | "dwm"
            | "explorer"
            | "windows explorer"
            | "dism-app"
            | "dism-cli"
            | "securityhealthservice"
            | "msmpeng"
    )
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
                .truncate(true)
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

        assert!(terminate_locking_process(&p, 4, 0).is_err());
        assert!(terminate_locking_process(&p, std::process::id(), 0).is_err());
        assert!(is_critical_process(120, 121, 0, "LSASS.EXE"));
        assert!(!is_critical_process(120, 121, 1, "Example System Services"));
    }
}
