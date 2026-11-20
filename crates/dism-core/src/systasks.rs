use crate::maintenance::{run_system_command, MaintenanceProgress};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};
use winreg::enums::{HKEY_LOCAL_MACHINE, HKEY_USERS, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};
use winreg::RegKey;

const COMPACT_OS_KEY: &str = r"SYSTEM\Setup";
const INSTALLER_USERDATA_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Installer\UserData";

#[link(name = "kernel32")]
extern "system" {
    fn MultiByteToWideChar(
        code_page: u32,
        dw_flags: u32,
        lp_multi_byte_str: *const u8,
        cb_multi_byte: i32,
        lp_wide_char_str: *mut u16,
        cch_wide_char: i32,
    ) -> i32;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemTaskStatus {
    pub compact_os_enabled: bool,
    pub compact_os_raw_output: String,
    pub outdated_drivers_count: usize,
    pub outdated_drivers_bytes: u64,
    pub orphaned_installer_count: usize,
    pub orphaned_installer_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemTaskExecOutcome {
    pub task_id: String,
    pub success: bool,
    pub output: String,
    pub status: SystemTaskStatus,
}

/// Decodes raw stdout/stderr bytes from Windows CLI utilities (`dism`, `pnputil`, `compact`, `vssadmin`),
/// preferring valid UTF-8 and falling back to Windows OEM/ANSI codepage via `MultiByteToWideChar`.
pub fn decode_cmd_output(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    if let Some(rest) = bytes.strip_prefix(b"\xff\xfe") {
        return decode_utf16le(rest);
    }
    if bytes.len() >= 4 && bytes.len() % 2 == 0 {
        let pairs = bytes.chunks_exact(2);
        let high_zeroes = pairs.clone().filter(|pair| pair[1] == 0).count();
        let low_zeroes = pairs.filter(|pair| pair[0] == 0).count();
        if high_zeroes > low_zeroes && high_zeroes * 4 >= bytes.len() / 2 {
            return decode_utf16le(bytes);
        }
    }
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    // CP_OEMCP = 1; fallback to CP_ACP = 0
    for cp in [1u32, 0u32] {
        let len = unsafe {
            MultiByteToWideChar(cp, 0, bytes.as_ptr(), bytes.len() as i32, std::ptr::null_mut(), 0)
        };
        if len > 0 {
            let mut wide = vec![0u16; len as usize];
            let written = unsafe {
                MultiByteToWideChar(
                    cp,
                    0,
                    bytes.as_ptr(),
                    bytes.len() as i32,
                    wide.as_mut_ptr(),
                    len,
                )
            };
            if written > 0 {
                return String::from_utf16_lossy(&wide[..written as usize]);
            }
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

pub(crate) fn decode_utf16le(bytes: &[u8]) -> String {
    let units: Vec<_> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

static COMPACT_OS_CACHE: OnceLock<RwLock<Option<(Instant, (bool, String))>>> = OnceLock::new();
static ORPHAN_INST_CACHE: OnceLock<RwLock<Option<(Instant, Vec<(PathBuf, u64)>)>>> =
    OnceLock::new();

pub fn invalidate_systasks_cache() {
    if let Some(lock) = COMPACT_OS_CACHE.get() {
        if let Ok(mut w) = lock.write() {
            *w = None;
        }
    }
    if let Some(lock) = ORPHAN_INST_CACHE.get() {
        if let Ok(mut w) = lock.write() {
            *w = None;
        }
    }
}

/// Reads the Windows CompactOS state (cached for 30 seconds).
pub fn query_compact_os() -> (bool, String) {
    let cache = COMPACT_OS_CACHE.get_or_init(|| RwLock::new(None));
    if let Ok(guard) = cache.read() {
        if let Some((ts, ref val)) = *guard {
            if ts.elapsed() < Duration::from_secs(30) {
                return val.clone();
            }
        }
    }

    let val = query_compact_os_uncached();
    if let Ok(mut w) = cache.write() {
        *w = Some((Instant::now(), val.clone()));
    }
    val
}

fn query_compact_os_uncached() -> (bool, String) {
    use windows_sys::Win32::Storage::FileSystem::{WofIsExternalFile, WOF_PROVIDER_FILE};

    let setup = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(COMPACT_OS_KEY, KEY_READ);
    if let Ok(value @ (0 | 1)) = setup.and_then(|key| key.get_value::<u32, _>("Compact")) {
        let enabled = value == 1;
        return (
            enabled,
            format!("CompactOS {}", if enabled { "enabled" } else { "disabled" }),
        );
    }

    // An automatic installation may have no explicit setting. Inspect inbox OS files
    // with WOF and distinguish CompactOS file backing from WIM-backed files.
    if let Ok(system32) = crate::system_tools::system32_dir() {
        let mut observed = false;
        for name in ["shell32.dll", "kernel32.dll", "win32kfull.sys"] {
            let path = system32.join(name);
            let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let (mut external, mut provider) = (0, 0);
            let result = unsafe {
                WofIsExternalFile(
                    wide.as_ptr(),
                    &mut external,
                    &mut provider,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if result >= 0 {
                observed = true;
                if external != 0 && provider == WOF_PROVIDER_FILE {
                    return (
                        true,
                        "CompactOS enabled (WOF-backed system binaries)".into(),
                    );
                }
            }
        }
        if observed {
            return (
                false,
                "CompactOS disabled (no WOF-backed system binaries detected)".into(),
            );
        }
    }
    (false, "Unable to query CompactOS state".into())
}

/// Finds orphaned `.msi` and `.msp` packages inside `%SystemRoot%\Installer` (cached 30s)
/// that have zero references across Installer registry databases.
pub fn find_orphaned_installer_files() -> Vec<(PathBuf, u64)> {
    let cache = ORPHAN_INST_CACHE.get_or_init(|| RwLock::new(None));
    if let Ok(guard) = cache.read() {
        if let Some((ts, ref val)) = *guard {
            if ts.elapsed() < Duration::from_secs(30) {
                return val.clone();
            }
        }
    }

    let val = compute_orphaned_installer_files_uncached();
    if let Ok(mut w) = cache.write() {
        *w = Some((Instant::now(), val.clone()));
    }
    val
}

pub(crate) fn compute_orphaned_installer_files_uncached() -> Vec<(PathBuf, u64)> {
    let referenced = collect_referenced_installer_packages();
    // Safety guard: never classify Installer files as orphaned if registry DB could not be read reliably
    if referenced.len() < 10 {
        return Vec::new();
    }

    let sys_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    let installer_dir = PathBuf::from(sys_root).join("Installer");
    let Ok(rd) = fs::read_dir(&installer_dir) else {
        return Vec::new();
    };

    let mut orphans = Vec::new();
    for entry in rd.flatten() {
        let p = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Some(ext) = p.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !ext.eq_ignore_ascii_case("msi") && !ext.eq_ignore_ascii_case("msp") {
            continue;
        }
        let full_norm = normalize_installer_ref(&p.to_string_lossy());
        let file_norm = p
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();

        if !referenced.contains(&full_norm) && !referenced.contains(&file_norm) {
            orphans.push((p, meta.len()));
        }
    }
    orphans.sort_by(|a, b| b.1.cmp(&a.1));
    orphans
}

fn collect_referenced_installer_packages() -> HashSet<String> {
    let mut set = HashSet::new();
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let Ok(ud_root) = hklm.open_subkey_with_flags(INSTALLER_USERDATA_KEY, KEY_READ) else {
        return set;
    };

    for sid in ud_root.enum_keys().flatten() {
        let Ok(sid_key) = ud_root.open_subkey_with_flags(&sid, KEY_READ) else {
            continue;
        };

        if let Ok(products) = sid_key.open_subkey_with_flags("Products", KEY_READ) {
            for prod_guid in products.enum_keys().flatten() {
                let Ok(prod_key) = products.open_subkey_with_flags(&prod_guid, KEY_READ) else {
                    continue;
                };
                if let Ok(props) = prod_key.open_subkey_with_flags("InstallProperties", KEY_READ) {
                    record_local_package(&props, &mut set);
                }
                if let Ok(prod_patches) = prod_key.open_subkey_with_flags("Patches", KEY_READ) {
                    for patch_guid in prod_patches.enum_keys().flatten() {
                        if let Ok(pk) = prod_patches.open_subkey_with_flags(&patch_guid, KEY_READ) {
                            record_local_package(&pk, &mut set);
                        }
                    }
                }
            }
        }

        if let Ok(patches) = sid_key.open_subkey_with_flags("Patches", KEY_READ) {
            for patch_guid in patches.enum_keys().flatten() {
                if let Ok(pk) = patches.open_subkey_with_flags(&patch_guid, KEY_READ) {
                    record_local_package(&pk, &mut set);
                    if let Ok(props) = pk.open_subkey_with_flags("InstallProperties", KEY_READ) {
                        record_local_package(&props, &mut set);
                    }
                }
            }
        }
    }

    // Also include Classes\Installer, HKEY_USERS Installer, and Uninstall references as extra safelists
    for cls_sub in [
        r"SOFTWARE\Classes\Installer\Products",
        r"SOFTWARE\Classes\Installer\Patches",
    ] {
        collect_sourcelist_refs(&hklm, cls_sub, KEY_READ, &mut set);
    }

    let hku = RegKey::predef(HKEY_USERS);
    for sid in hku.enum_keys().flatten() {
        for sub in [
            format!(r"{}\Software\Microsoft\Installer\Products", sid),
            format!(r"{}\Software\Microsoft\Installer\Patches", sid),
        ] {
            collect_sourcelist_refs(&hku, &sub, KEY_READ, &mut set);
        }
    }

    for sam in [KEY_READ | KEY_WOW64_64KEY, KEY_READ | KEY_WOW64_32KEY] {
        if let Ok(uninst) =
            hklm.open_subkey_with_flags(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall", sam)
        {
            for app_key in uninst.enum_keys().flatten() {
                if let Ok(ak) = uninst.open_subkey_with_flags(&app_key, sam) {
                    for val_name in ["UninstallString", "ModifyPath", "InstallSource"] {
                        if let Ok(raw) = ak.get_value::<String, _>(val_name) {
                            record_ms_tokens(&raw, &mut set);
                        }
                    }
                }
            }
        }
    }

    set
}

fn collect_sourcelist_refs(root: &RegKey, subkey: &str, sam: u32, set: &mut HashSet<String>) {
    let Ok(parent) = root.open_subkey_with_flags(subkey, sam) else {
        return;
    };
    for guid in parent.enum_keys().flatten() {
        let Ok(sk) = parent.open_subkey_with_flags(&guid, sam) else {
            continue;
        };
        if let Ok(src) = sk.open_subkey_with_flags("SourceList", sam) {
            if let Ok(pkg_name) = src.get_value::<String, _>("PackageName") {
                record_ms_tokens(&pkg_name, set);
            }
            if let Ok(net) = src.open_subkey_with_flags("Net", sam) {
                for (_, rv) in net.enum_values().flatten() {
                    let s = crate::evaluator::reg_value_to_string(&rv);
                    record_ms_tokens(&s, set);
                }
            }
        }
    }
}

fn record_ms_tokens(raw: &str, set: &mut HashSet<String>) {
    let norm = normalize_installer_ref(raw);
    if norm.is_empty() {
        return;
    }
    for part in norm.split(['"', ';', ',']) {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(fname) = Path::new(trimmed).file_name().and_then(|s| s.to_str()) {
            let lower = fname.to_ascii_lowercase();
            if lower.ends_with(".msi") || lower.ends_with(".msp") {
                set.insert(lower);
            }
        }
        if trimmed.ends_with(".msi") || trimmed.ends_with(".msp") {
            set.insert(trimmed.to_string());
        }
    }
}

fn record_local_package(key: &RegKey, set: &mut HashSet<String>) {
    if let Ok(val) = key.get_value::<String, _>("LocalPackage") {
        let norm = normalize_installer_ref(&val);
        if !norm.is_empty() {
            if let Some(fname) = Path::new(&norm).file_name().and_then(|s| s.to_str()) {
                set.insert(fname.to_ascii_lowercase());
            }
            set.insert(norm);
        }
    }
}

fn normalize_installer_ref(s: &str) -> String {
    s.trim().replace('/', "\\").to_ascii_lowercase()
}

/// Aggregates live status for CompactOS, outdated duplicate drivers, and orphaned Installer packages.
pub fn get_system_task_status() -> SystemTaskStatus {
    let (compact_os_enabled, compact_os_raw_output) = query_compact_os();
    let drivers = crate::drivers::list_drivers();
    let outdated: Vec<_> = drivers.iter().filter(|d| d.is_outdated_duplicate).collect();
    let outdated_drivers_count = outdated.len();
    let outdated_drivers_bytes: u64 = outdated.iter().map(|d| d.size_bytes).sum();

    let orphans = find_orphaned_installer_files();
    let orphaned_installer_count = orphans.len();
    let orphaned_installer_bytes: u64 = orphans.iter().map(|(_, sz)| *sz).sum();

    SystemTaskStatus {
        compact_os_enabled,
        compact_os_raw_output,
        outdated_drivers_count,
        outdated_drivers_bytes,
        orphaned_installer_count,
        orphaned_installer_bytes,
    }
}

/// Executes an explicit system maintenance task (requires Administrator privileges).
pub fn run_system_task(task_id: &str) -> Result<SystemTaskExecOutcome> {
    run_system_task_impl(task_id, None)
}

pub fn run_system_task_with_progress(
    task_id: &str,
    progress: &MaintenanceProgress,
) -> Result<SystemTaskExecOutcome> {
    run_system_task_impl(task_id, Some(progress))
}

fn run_system_task_impl(
    task_id: &str,
    progress: Option<&MaintenanceProgress>,
) -> Result<SystemTaskExecOutcome> {
    if let Some(progress) = progress {
        progress.check()?;
    }
    let sys = crate::sysinfo::SystemInfo::collect();
    if !sys.is_admin {
        return Err(anyhow!(
            "System maintenance tasks require Administrator privileges. Please click '一键提权为管理员' first."
        ));
    }

    let output_text = match task_id {
        "compact_os_enable" => exec_cmd("compact.exe", &["/CompactOS:always"], progress)?,
        "compact_os_disable" => exec_cmd("compact.exe", &["/CompactOS:never"], progress)?,
        "component_cleanup" => exec_cmd(
            "dism.exe",
            &[
                "/Online",
                "/Cleanup-Image",
                "/StartComponentCleanup",
                "/NoRestart",
            ],
            progress,
        )?,
        "restore_point_cleanup" => {
            let drive_arg = format!("/for={}", sys.system_drive);
            exec_cmd(
                "vssadmin.exe",
                &["delete", "shadows", &drive_arg, "/oldest", "/quiet"],
                progress,
            )?
        }
        "driver_cleanup" => {
            let drivers = crate::drivers::list_drivers();
            let outdated: Vec<_> = drivers
                .into_iter()
                .filter(|d| d.is_outdated_duplicate)
                .collect();
            if outdated.is_empty() {
                "No outdated duplicate driver packages found.".to_string()
            } else {
                let mut logs = Vec::new();
                for drv in outdated {
                    if let Some(progress) = progress {
                        progress.check()?;
                    }
                    match crate::drivers::delete_driver(&drv.published_name) {
                        Ok(msg) => logs.push(format!(
                            "Deleted {} ({} v{}): {}",
                            drv.published_name, drv.original_name, drv.version, msg.replace('\n', " ")
                        )),
                        Err(e) => logs.push(format!("Skipped {} ({}): {}", drv.published_name, drv.original_name, e)),
                    }
                }
                logs.join("\n")
            }
        }
        other => return Err(anyhow!("Unknown system task '{}'", other)),
    };

    invalidate_systasks_cache();

    Ok(SystemTaskExecOutcome {
        task_id: task_id.to_string(),
        success: true,
        output: output_text,
        status: get_system_task_status(),
    })
}

fn exec_cmd(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_utf16_before_utf8() {
        let text = "Windows 资源保护\r\n";
        let wide: Vec<_> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(decode_cmd_output(&wide), text);
        assert_eq!(
            decode_cmd_output(&[b"\xff\xfe".as_slice(), &wide].concat()),
            text
        );
        assert_eq!(decode_cmd_output(text.as_bytes()), text);
    }

    #[test]
    fn test_query_compact_os_and_orphaned_installer_scan() {
        let (_enabled, raw) = query_compact_os();
        assert!(!raw.is_empty());

        let referenced = collect_referenced_installer_packages();
        assert!(referenced.len() >= 10, "Expected referenced MSI/MSP packages in Installer\\UserData");
        let _orphans = find_orphaned_installer_files();
    }
}
