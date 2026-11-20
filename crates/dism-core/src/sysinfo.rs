use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::sync::OnceLock;
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives,
};
use windows_sys::Win32::UI::Shell::{IsUserAnAdmin, ShellExecuteW};
use winreg::enums::HKEY_LOCAL_MACHINE;
use winreg::RegKey;

const DRIVE_FIXED: u32 = 3;

#[derive(Clone)]
struct StaticSystemInfo {
    product_name: String,
    display_version: String,
    major: u32,
    minor: u32,
    build: u32,
    ubr: u32,
    version_string: String,
    arch: String,
    arch_code: u32,
    is_admin: bool,
    system_drive: String,
    drive_root: String,
    fixed_drives: Vec<String>,
}

static STATIC_SYS_INFO: OnceLock<StaticSystemInfo> = OnceLock::new();

fn static_sys_info() -> &'static StaticSystemInfo {
    STATIC_SYS_INFO.get_or_init(|| {
        let (product_name, display_version, major, minor, build, ubr) = read_windows_version();
        let version_string = format!("{}.{}.{}.{}", major, minor, build, ubr);

        let (arch, arch_code) = match std::env::consts::ARCH {
            "x86_64" => ("x64".to_string(), 9),
            "x86" => ("x86".to_string(), 0),
            "aarch64" => ("ARM64".to_string(), 12),
            other => (other.to_string(), 9),
        };

        let is_admin = is_administrator();
        let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
        let drive_root = if system_drive.ends_with('\\') {
            system_drive.clone()
        } else {
            format!("{}\\", system_drive)
        };
        let fixed_drives = enumerate_fixed_drives();

        StaticSystemInfo {
            product_name,
            display_version,
            major,
            minor,
            build,
            ubr,
            version_string,
            arch,
            arch_code,
            is_admin,
            system_drive,
            drive_root,
            fixed_drives,
        }
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskStats {
    pub root: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub used_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub product_name: String,
    pub display_version: String,
    pub major: u32,
    pub minor: u32,
    pub build: u32,
    pub ubr: u32,
    pub version_string: String,
    pub arch: String,
    pub arch_code: u32,
    pub is_admin: bool,
    pub system_drive: String,
    pub system_disk: DiskStats,
    pub fixed_drives: Vec<String>,
}

impl SystemInfo {
    pub fn collect() -> Self {
        let st = static_sys_info();
        let system_disk = query_disk_space(&st.drive_root);

        Self {
            product_name: st.product_name.clone(),
            display_version: st.display_version.clone(),
            major: st.major,
            minor: st.minor,
            build: st.build,
            ubr: st.ubr,
            version_string: st.version_string.clone(),
            arch: st.arch.clone(),
            arch_code: st.arch_code,
            is_admin: st.is_admin,
            system_drive: st.system_drive.clone(),
            system_disk,
            fixed_drives: st.fixed_drives.clone(),
        }
    }

    pub fn version_tuple(&self) -> [u32; 4] {
        [self.major, self.minor, self.build, self.ubr]
    }
}

fn read_windows_version() -> (String, String, u32, u32, u32, u32) {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let key = hklm
        .open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion")
        .ok();

    let mut product_name = key
        .as_ref()
        .and_then(|k| k.get_value::<String, _>("ProductName").ok())
        .unwrap_or_else(|| "Windows".to_string());

    let display_version = key
        .as_ref()
        .and_then(|k| {
            k.get_value::<String, _>("DisplayVersion")
                .ok()
                .or_else(|| k.get_value::<String, _>("ReleaseId").ok())
        })
        .unwrap_or_default();

    let major = key
        .as_ref()
        .and_then(|k| k.get_value::<u32, _>("CurrentMajorVersionNumber").ok())
        .unwrap_or(10);

    let minor = key
        .as_ref()
        .and_then(|k| k.get_value::<u32, _>("CurrentMinorVersionNumber").ok())
        .unwrap_or(0);

    let build = key
        .as_ref()
        .and_then(|k| k.get_value::<String, _>("CurrentBuildNumber").ok())
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);

    let ubr = key
        .as_ref()
        .and_then(|k| k.get_value::<u32, _>("UBR").ok())
        .unwrap_or(0);

    // Windows 11 keeps "Windows 10" in registry ProductName for app compat; modernize for UI display.
    if build >= 22000 && product_name.contains("Windows 10") {
        product_name = product_name.replace("Windows 10", "Windows 11");
    }

    (product_name, display_version, major, minor, build, ubr)
}

pub fn query_disk_space(root: &str) -> DiskStats {
    let wide: Vec<u16> = OsStr::new(root).encode_wide().chain(Some(0)).collect();
    let mut free_avail: u64 = 0;
    let mut total: u64 = 0;
    let mut total_free: u64 = 0;
    let ok =
        unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free_avail, &mut total, &mut total_free) };
    if ok == 0 {
        return DiskStats {
            root: root.to_string(),
            total_bytes: 0,
            free_bytes: 0,
            used_bytes: 0,
        };
    }
    DiskStats {
        root: root.to_string(),
        total_bytes: total,
        free_bytes: free_avail,
        used_bytes: total.saturating_sub(free_avail),
    }
}

pub fn enumerate_fixed_drives() -> Vec<String> {
    let mask = unsafe { GetLogicalDrives() };
    let mut drives = Vec::new();
    for i in 0..26u32 {
        if (mask & (1 << i)) != 0 {
            let letter = (b'A' + i as u8) as char;
            let root = format!("{}:\\", letter);
            let wide: Vec<u16> = OsStr::new(&root).encode_wide().chain(Some(0)).collect();
            let dtype = unsafe { GetDriveTypeW(wide.as_ptr()) };
            if dtype == DRIVE_FIXED {
                drives.push(root);
            }
        }
    }
    drives
}

pub fn is_administrator() -> bool {
    unsafe { IsUserAnAdmin() != 0 }
}

pub fn open_in_browser(url: &str) -> Result<()> {
    let url: Vec<u16> = OsStr::new(url).encode_wide().chain(Some(0)).collect();
    let dir: Vec<u16> = crate::system_tools::system32_dir()?
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            url.as_ptr(),
            std::ptr::null(),
            dir.as_ptr(),
            1,
        )
    } as isize;
    if result <= 32 {
        return Err(anyhow!(
            "Could not open DismNext in the browser (ShellExecuteW code {result})"
        ));
    }
    Ok(())
}

/// Relaunches the current executable with an elevated UAC prompt (`runas`).
/// Returns `Ok(())` only if ShellExecuteW successfully launched the elevated process (return value > 32).
pub fn relaunch_as_admin(extra_args: &[String]) -> Result<()> {
    let exe = std::env::current_exe()?;
    let op: Vec<u16> = OsStr::new("runas").encode_wide().chain(Some(0)).collect();
    let file: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let params_str = extra_args
        .iter()
        .map(|a| {
            if a.contains(' ') {
                format!("\"{}\"", a)
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let params: Vec<u16> = OsStr::new(&params_str)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| {
        exe.parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf()
    });
    let dir: Vec<u16> = cwd.as_os_str().encode_wide().chain(Some(0)).collect();

    let res = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
            dir.as_ptr(),
            1, // SW_SHOWNORMAL
        )
    } as isize;

    if res <= 32 {
        return Err(anyhow!(
            "UAC elevation cancelled or failed (ShellExecuteW code {})",
            res
        ));
    }
    Ok(())
}
