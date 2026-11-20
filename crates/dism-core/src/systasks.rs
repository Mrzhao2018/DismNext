use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};
use winreg::enums::{HKEY_LOCAL_MACHINE, HKEY_USERS, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};
use winreg::RegKey;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
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

/// Queries live Windows CompactOS compression state via `compact.exe /CompactOS:query` (cached 30s).
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
    let Ok(output) =
        crate::system_tools::system32_command("compact.exe").and_then(|mut command| {
            command
                .creation_flags(CREATE_NO_WINDOW)
                .arg("/CompactOS:query")
                .output()
        })
    else {
        return (false, "Unable to query CompactOS state".to_string());
    };

    let stdout = decode_cmd_output(&output.stdout);
    let stderr = decode_cmd_output(&output.stderr);
    let raw = format!("{}\n{}", stdout.trim(), stderr.trim()).trim().to_string();
    let enabled = parse_compact_os_output(&raw);
    (enabled, raw)
}

fn parse_compact_os_output(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    let negated = lower.contains("not in the compact state")
        || raw.contains("不处于")
        || raw.contains("未处于")
        || raw.contains("不是处于");
    let positive = lower.contains("is in the compact state") || raw.contains("处于压缩状态");
    positive && !negated
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
    let sys = crate::sysinfo::SystemInfo::collect();
    if !sys.is_admin {
        return Err(anyhow!(
            "System maintenance tasks require Administrator privileges. Please click '一键提权为管理员' first."
        ));
    }

    let output_text = match task_id {
        "compact_os_enable" => exec_cmd("compact.exe", &["/CompactOS:always"])?,
        "compact_os_disable" => exec_cmd("compact.exe", &["/CompactOS:never"])?,
        "component_cleanup" => {
            exec_cmd("dism.exe", &["/Online", "/Cleanup-Image", "/StartComponentCleanup", "/NoRestart"])?
        }
        "restore_point_cleanup" => {
            let drive_arg = format!("/for={}", sys.system_drive);
            exec_cmd("vssadmin.exe", &["delete", "shadows", &drive_arg, "/oldest", "/quiet"])?
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

fn exec_cmd(program: &str, args: &[&str]) -> Result<String> {
    let output = crate::system_tools::system32_command(program)?
        .creation_flags(CREATE_NO_WINDOW)
        .args(args)
        .output()
        .with_context(|| format!("Failed to start {}", program))?;

    let stdout = decode_cmd_output(&output.stdout);
    let stderr = decode_cmd_output(&output.stderr);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_compact_os_and_orphaned_installer_scan() {
        assert!(parse_compact_os_output("The system is in the Compact state."));
        assert!(!parse_compact_os_output(
            "The system is not in the Compact state because Windows has determined that it is not beneficial."
        ));
        assert!(parse_compact_os_output("系统处于压缩状态。"));
        assert!(!parse_compact_os_output(
            "系统不处于压缩状态，因为 Windows 已确定\r\n压缩状态不适用于此系统。"
        ));
        assert!(!parse_compact_os_output("系统未处于压缩状态。"));

        let (_enabled, raw) = query_compact_os();
        assert!(!raw.is_empty());

        let referenced = collect_referenced_installer_packages();
        assert!(referenced.len() >= 10, "Expected referenced MSI/MSP packages in Installer\\UserData");
        let _orphans = find_orphaned_installer_files();
    }
}
