use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives};
use windows_sys::Win32::UI::Shell::IsUserAnAdmin;
use winreg::enums::HKEY_LOCAL_MACHINE;
use winreg::RegKey;

const DRIVE_FIXED: u32 = 3;

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
        let (product_name, display_version, major, minor, build, ubr) = read_windows_version();
        let version_string = format!("{}.{}.{}.{}", major, minor, build, ubr);

        let (arch, arch_code) = match std::env::consts::ARCH {
            "x86_64" => ("x64".to_string(), 9),
            "x86" => ("x86".to_string(), 0),
            "aarch64" => ("ARM64".to_string(), 12),
            other => (other.to_string(), 9),
        };

        let is_admin = unsafe { IsUserAnAdmin() != 0 };
        let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
        let drive_root = if system_drive.ends_with('\\') {
            system_drive.clone()
        } else {
            format!("{}\\", system_drive)
        };
        let system_disk = query_disk_space(&drive_root);
        let fixed_drives = enumerate_fixed_drives();

        Self {
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
            system_disk,
            fixed_drives,
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
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_avail,
            &mut total,
            &mut total_free,
        )
    };
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
