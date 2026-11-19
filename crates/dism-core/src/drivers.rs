use anyhow::{anyhow, Context, Result};
use chrono::DateTime;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};
use winreg::RegKey;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
const DRIVER_PACKAGES_KEY: &str = r"SYSTEM\DriverDatabase\DriverPackages";
const CONTROL_CLASS_KEY: &str = r"SYSTEM\CurrentControlSet\Control\Class";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverPackageInfo {
    pub published_name: String,
    pub original_name: String,
    pub provider: String,
    pub class_name: String,
    pub class_guid: String,
    pub version: String,
    pub date: String,
    pub signer_name: String,
    pub repo_folder: String,
    pub repo_path: String,
    pub size_bytes: u64,
    pub is_outdated_duplicate: bool,
    pub newer_version: Option<String>,
    #[serde(skip)]
    version_rank: u64,
    #[serde(skip)]
    date_filetime: u64,
    #[serde(skip)]
    published_num: u32,
    #[serde(skip)]
    hw_descriptors: HashSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverBackupReport {
    pub target_dir: String,
    pub exported_count: usize,
    pub total_bytes: u64,
    pub manifest_path: String,
}

struct RawDriverMeta {
    repo_folder: String,
    published_name: String,
    published_num: u32,
    original_name: String,
    provider: String,
    signer_name: String,
    class_guid: String,
    version: String,
    version_rank: u64,
    date: String,
    date_filetime: u64,
    fallback_file_size: u64,
    hw_descriptors: HashSet<String>,
}

static DRIVERS_CACHE: OnceLock<RwLock<Option<(Instant, Vec<DriverPackageInfo>)>>> = OnceLock::new();

pub fn invalidate_drivers_cache() {
    if let Some(lock) = DRIVERS_CACHE.get() {
        if let Ok(mut w) = lock.write() {
            *w = None;
        }
    }
}

/// Lists all third-party (`oem*.inf`) driver packages registered in Windows DriverDatabase
/// and detects older duplicate versions superseded by newer packages.
pub fn list_drivers() -> Vec<DriverPackageInfo> {
    let cache = DRIVERS_CACHE.get_or_init(|| RwLock::new(None));
    if let Ok(guard) = cache.read() {
        if let Some((ts, ref list)) = *guard {
            if ts.elapsed() < Duration::from_secs(30) {
                return list.clone();
            }
        }
    }
    let computed = compute_drivers_uncached();
    if let Ok(mut w) = cache.write() {
        *w = Some((Instant::now(), computed.clone()));
    }
    computed
}

pub(crate) fn compute_drivers_uncached() -> Vec<DriverPackageInfo> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let Ok(pkg_root) = hklm.open_subkey_with_flags(DRIVER_PACKAGES_KEY, KEY_READ) else {
        return Vec::new();
    };

    let class_map = load_class_guid_map(&hklm);
    let sys_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    let file_repo_base = PathBuf::from(&sys_root).join(r"System32\DriverStore\FileRepository");

    let mut raw_entries = Vec::new();
    for repo_folder in pkg_root.enum_keys().flatten() {
        let Ok(sk) = pkg_root.open_subkey_with_flags(&repo_folder, KEY_READ) else {
            continue;
        };
        let Ok(published_name) = sk.get_value::<String, _>("") else {
            continue;
        };
        let pub_trimmed = published_name.trim();
        let Some(pub_num) = parse_oem_inf_number(pub_trimmed) else {
            continue;
        };

        let original_name = sk
            .get_value::<String, _>("InfName")
            .unwrap_or_else(|_| repo_folder.split('_').next().unwrap_or(&repo_folder).to_string());
        let provider = sk
            .get_value::<String, _>("Provider")
            .unwrap_or_else(|_| "Unknown".to_string());
        let signer_name = sk.get_value::<String, _>("SignerName").unwrap_or_default();
        let fallback_file_size = sk.get_value::<u32, _>("FileSize").unwrap_or(0) as u64;

        let version_bytes = sk
            .get_raw_value("Version")
            .map(|rv| rv.bytes)
            .unwrap_or_default();
        let (class_guid, date_filetime, date, version_rank, version) =
            decode_driver_version_blob(&version_bytes);
        let hw_descriptors = read_driver_descriptors(&sk);

        raw_entries.push(RawDriverMeta {
            repo_folder,
            published_name: pub_trimmed.to_string(),
            published_num: pub_num,
            original_name,
            provider,
            signer_name,
            class_guid,
            version,
            version_rank,
            date,
            date_filetime,
            fallback_file_size,
            hw_descriptors,
        });
    }

    let mut items: Vec<DriverPackageInfo> = raw_entries
        .into_par_iter()
        .map(|raw| {
            let repo_path = file_repo_base.join(&raw.repo_folder);
            let measured = dir_size_bytes(&repo_path);
            let size_bytes = if measured > 0 {
                measured
            } else {
                raw.fallback_file_size
            };
            let class_name = resolve_class_name(&raw.class_guid, &class_map);

            DriverPackageInfo {
                published_name: raw.published_name,
                original_name: raw.original_name,
                provider: raw.provider,
                class_name,
                class_guid: raw.class_guid,
                version: raw.version,
                date: raw.date,
                signer_name: raw.signer_name,
                repo_folder: raw.repo_folder,
                repo_path: repo_path.to_string_lossy().into_owned(),
                size_bytes,
                is_outdated_duplicate: false,
                newer_version: None,
                version_rank: raw.version_rank,
                date_filetime: raw.date_filetime,
                published_num: raw.published_num,
                hw_descriptors: raw.hw_descriptors,
            }
        })
        .collect();

    mark_outdated_duplicates(&mut items);

    // Sort outdated duplicates first, then largest packages descending
    items.sort_by(|a, b| {
        b.is_outdated_duplicate
            .cmp(&a.is_outdated_duplicate)
            .then_with(|| b.size_bytes.cmp(&a.size_bytes))
            .then_with(|| a.published_num.cmp(&b.published_num))
    });
    items
}

/// Returns a sensible default backup directory under Desktop or C:\DriverBackup.
pub fn default_backup_dir() -> String {
    if let Ok(profile) = std::env::var("USERPROFILE") {
        let desktop = PathBuf::from(profile).join("Desktop").join("DismNext_DriverBackup");
        return desktop.to_string_lossy().into_owned();
    }
    r"C:\DismNext_DriverBackup".to_string()
}

/// Exports third-party driver packages directly from DriverStore FileRepository into `target_dir`,
/// along with `driver_backup_manifest.json` and `Restore-Drivers.cmd`.
pub fn export_drivers(
    target_dir: &Path,
    published_names: Option<&[String]>,
) -> Result<DriverBackupReport> {
    if target_dir.as_os_str().is_empty() {
        return Err(anyhow!("Backup target directory cannot be empty"));
    }

    let all_drivers = list_drivers();
    let selected: Vec<DriverPackageInfo> = match published_names {
        None => all_drivers,
        Some(names) => {
            if names.is_empty() {
                return Err(anyhow!("No driver packages selected for backup"));
            }
            let wanted: HashSet<String> = names.iter().map(|s| s.trim().to_ascii_lowercase()).collect();
            all_drivers
                .into_iter()
                .filter(|d| wanted.contains(&d.published_name.to_ascii_lowercase()))
                .collect()
        }
    };

    if selected.is_empty() {
        return Err(anyhow!("No matching third-party driver packages found to export"));
    }

    fs::create_dir_all(target_dir)
        .with_context(|| format!("Failed to create backup directory {}", target_dir.display()))?;

    let mut exported_count = 0usize;
    let mut total_bytes = 0u64;
    let mut manifest_entries = Vec::with_capacity(selected.len());

    for drv in &selected {
        let src = Path::new(&drv.repo_path);
        if !src.exists() {
            continue;
        }
        let inf_stem = Path::new(&drv.original_name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&drv.original_name);
        let pub_stem = Path::new(&drv.published_name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&drv.published_name);

        let folder_name = format!(
            "{}_{}_v{}_{}",
            sanitize_component(&drv.provider),
            sanitize_component(inf_stem),
            sanitize_component(&drv.version),
            sanitize_component(pub_stem)
        );
        let dest_pkg_dir = target_dir.join(&folder_name);
        let copied = copy_dir_recursive(src, &dest_pkg_dir)
            .with_context(|| format!("Failed copying driver {} from {}", drv.published_name, src.display()))?;

        exported_count += 1;
        total_bytes += copied;
        manifest_entries.push(serde_json::json!({
            "published_name": drv.published_name,
            "original_name": drv.original_name,
            "provider": drv.provider,
            "class_name": drv.class_name,
            "version": drv.version,
            "date": drv.date,
            "signer_name": drv.signer_name,
            "backup_folder": folder_name,
            "bytes": copied,
        }));
    }

    let manifest_path = target_dir.join("driver_backup_manifest.json");
    let manifest_json = serde_json::to_string_pretty(&serde_json::json!({
        "created_at": chrono::Local::now().to_rfc3339(),
        "exported_count": exported_count,
        "total_bytes": total_bytes,
        "drivers": manifest_entries,
    }))?;
    fs::write(&manifest_path, manifest_json)?;

    let restore_script = target_dir.join("Restore-Drivers.cmd");
    let script_body = "@echo off\r\n\
echo Restoring backed-up drivers via Windows PnPUtil (Run as Administrator)...\r\n\
pnputil.exe /add-driver \"%~dp0*.inf\" /subdirs /install\r\n\
echo.\r\n\
pause\r\n";
    fs::write(restore_script, script_body)?;

    Ok(DriverBackupReport {
        target_dir: target_dir.to_string_lossy().into_owned(),
        exported_count,
        total_bytes,
        manifest_path: manifest_path.to_string_lossy().into_owned(),
    })
}

/// Deletes an `oemXX.inf` driver package safely using `pnputil.exe /delete-driver <published_name>`
/// (without `/force`, so active hardware drivers currently in use remain protected by Windows).
pub fn delete_driver(published_name: &str) -> Result<String> {
    let trimmed = published_name.trim();
    if parse_oem_inf_number(trimmed).is_none() {
        return Err(anyhow!(
            "Invalid driver published name '{}': must match oem<number>.inf",
            trimmed
        ));
    }

    if !crate::sysinfo::SystemInfo::collect().is_admin {
        return Err(anyhow!(
            "Deleting DriverStore packages requires Administrator privileges. Please click '一键提权为管理员' first."
        ));
    }

    let output = Command::new("pnputil.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["/delete-driver", trimmed])
        .output()
        .context("Failed to launch pnputil.exe")?;

    let stdout = crate::systasks::decode_cmd_output(&output.stdout);
    let stderr = crate::systasks::decode_cmd_output(&output.stderr);
    let combined = format!("{}\n{}", stdout.trim(), stderr.trim()).trim().to_string();

    if !output.status.success() {
        return Err(anyhow!(
            "pnputil /delete-driver {} failed ({}): {}",
            trimmed,
            output.status.code().unwrap_or(-1),
            combined
        ));
    }

    invalidate_drivers_cache();
    Ok(combined)
}

fn read_driver_descriptors(pkg_key: &RegKey) -> HashSet<String> {
    let mut set = HashSet::new();
    let Ok(desc_root) = pkg_key.open_subkey_with_flags("Descriptors", KEY_READ) else {
        return set;
    };
    for bus in desc_root.enum_keys().flatten() {
        let Ok(bus_key) = desc_root.open_subkey_with_flags(&bus, KEY_READ) else {
            continue;
        };
        let mut has_child = false;
        for hwid in bus_key.enum_keys().flatten() {
            has_child = true;
            set.insert(format!("{}\\{}", bus, hwid).to_ascii_lowercase());
        }
        if !has_child {
            set.insert(bus.to_ascii_lowercase());
        }
    }
    set
}

fn mark_outdated_duplicates(items: &mut [DriverPackageInfo]) {
    let mut groups: HashMap<(String, String, String), Vec<usize>> = HashMap::new();
    for (idx, item) in items.iter().enumerate() {
        let key = (
            item.provider.trim().to_ascii_lowercase(),
            item.original_name.trim().to_ascii_lowercase(),
            item.class_guid.to_ascii_lowercase(),
        );
        groups.entry(key).or_default().push(idx);
    }

    for indices in groups.into_values() {
        if indices.len() <= 1 {
            continue;
        }
        let mut sorted = indices;
        sorted.sort_by(|&a, &b| {
            let da = &items[a];
            let db = &items[b];
            db.version_rank
                .cmp(&da.version_rank)
                .then_with(|| db.date_filetime.cmp(&da.date_filetime))
                .then_with(|| db.published_num.cmp(&da.published_num))
        });

        let newest_idx = sorted[0];
        let newest_key = (items[newest_idx].version_rank, items[newest_idx].date_filetime);
        let newest_label = if items[newest_idx].date.is_empty() {
            format!("{} ({})", items[newest_idx].version, items[newest_idx].published_name)
        } else {
            format!(
                "{} ({}, {})",
                items[newest_idx].version,
                items[newest_idx].published_name,
                items[newest_idx].date
            )
        };

        for &idx in &sorted[1..] {
            let cur_key = (items[idx].version_rank, items[idx].date_filetime);
            let hw_covered = items[idx]
                .hw_descriptors
                .is_subset(&items[newest_idx].hw_descriptors);
            if cur_key < newest_key && hw_covered {
                items[idx].is_outdated_duplicate = true;
                items[idx].newer_version = Some(newest_label.clone());
            }
        }
    }
}

fn parse_oem_inf_number(name: &str) -> Option<u32> {
    let lower = name.trim().to_ascii_lowercase();
    let mid = lower.strip_prefix("oem")?.strip_suffix(".inf")?;
    if mid.is_empty() || !mid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    mid.parse::<u32>().ok()
}

fn decode_driver_version_blob(bytes: &[u8]) -> (String, u64, String, u64, String) {
    let class_guid = if bytes.len() >= 24 {
        let d1 = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        let d2 = u16::from_le_bytes([bytes[12], bytes[13]]);
        let d3 = u16::from_le_bytes([bytes[14], bytes[15]]);
        format!(
            "{{{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
            d1,
            d2,
            d3,
            bytes[16],
            bytes[17],
            bytes[18],
            bytes[19],
            bytes[20],
            bytes[21],
            bytes[22],
            bytes[23]
        )
    } else {
        "{00000000-0000-0000-0000-000000000000}".to_string()
    };

    let (date_filetime, date) = if bytes.len() >= 32 {
        let ft = u64::from_le_bytes([
            bytes[24], bytes[25], bytes[26], bytes[27], bytes[28], bytes[29], bytes[30], bytes[31],
        ]);
        let formatted = filetime_to_ymd(ft);
        (ft, formatted)
    } else {
        (0, String::new())
    };

    let (version_rank, version) = if bytes.len() >= 40 {
        let rev = u16::from_le_bytes([bytes[32], bytes[33]]);
        let build = u16::from_le_bytes([bytes[34], bytes[35]]);
        let minor = u16::from_le_bytes([bytes[36], bytes[37]]);
        let major = u16::from_le_bytes([bytes[38], bytes[39]]);
        let rank = ((major as u64) << 48)
            | ((minor as u64) << 32)
            | ((build as u64) << 16)
            | (rev as u64);
        (rank, format!("{}.{}.{}.{}", major, minor, build, rev))
    } else {
        (0, "0.0.0.0".to_string())
    };

    (class_guid, date_filetime, date, version_rank, version)
}

fn filetime_to_ymd(ft: u64) -> String {
    if ft < 116_444_736_000_000_000 {
        return String::new();
    }
    let unix_secs = ((ft - 116_444_736_000_000_000) / 10_000_000) as i64;
    DateTime::from_timestamp(unix_secs, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn load_class_guid_map(hklm: &RegKey) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Ok(class_root) = hklm.open_subkey_with_flags(CONTROL_CLASS_KEY, KEY_READ) {
        for guid in class_root.enum_keys().flatten() {
            if let Ok(sk) = class_root.open_subkey_with_flags(&guid, KEY_READ) {
                if let Ok(cls) = sk.get_value::<String, _>("Class") {
                    if !cls.trim().is_empty() {
                        map.insert(guid.to_ascii_lowercase(), cls);
                    }
                }
            }
        }
    }
    map
}

fn resolve_class_name(guid: &str, reg_map: &HashMap<String, String>) -> String {
    let lower = guid.to_ascii_lowercase();
    if let Some(name) = reg_map.get(&lower) {
        return name.clone();
    }
    match lower.as_str() {
        "{4d36e968-e325-11ce-bfc1-08002be10318}" => "Display".to_string(),
        "{4d36e96c-e325-11ce-bfc1-08002be10318}" => "MEDIA".to_string(),
        "{4d36e972-e325-11ce-bfc1-08002be10318}" => "Net".to_string(),
        "{4d36e97d-e325-11ce-bfc1-08002be10318}" => "System".to_string(),
        "{e0cbf06c-cd8b-4647-bb8a-263b43f0f974}" => "Bluetooth".to_string(),
        "{36fc9e60-c465-11cf-8056-444553540000}" => "USB".to_string(),
        "{745a17a0-74d3-11d0-b6fe-00a0c90f57da}" => "HIDClass".to_string(),
        "{4d36e97b-e325-11ce-bfc1-08002be10318}" => "SCSIAdapter".to_string(),
        "{f2e7dd72-6468-4e36-b6f1-6488f42c1b52}" => "Firmware".to_string(),
        "{5c4c3332-344d-483c-8739-259e934c9cc8}" => "SoftwareComponent".to_string(),
        "{c166523c-fe0c-4a94-a586-f1a80cfbbf3e}" => "AudioEndpoint".to_string(),
        "{88bae032-5a81-49f0-bc3d-a4ff138216d6}" => "USBDevice".to_string(),
        "{d48179be-ec20-11d1-b6b8-00c04fa372a7}" => "SBP2".to_string(),
        _ => "Other".to_string(),
    }
}

fn dir_size_bytes(dir: &Path) -> u64 {
    if !dir.is_dir() {
        return 0;
    }
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(rd) = fs::read_dir(&cur) else {
            continue;
        };
        for entry in rd.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                continue;
            }
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<u64> {
    fs::create_dir_all(dst)?;
    let mut total = 0u64;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let p = entry.path();
        let meta = fs::symlink_metadata(&p)?;
        if (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
            continue;
        }
        let target = dst.join(entry.file_name());
        if meta.is_dir() {
            total += copy_dir_recursive(&p, &target)?;
        } else if meta.is_file() {
            let bytes = fs::copy(&p, &target)?;
            total += bytes;
        }
    }
    Ok(total)
}

fn sanitize_component(s: &str) -> String {
    let cleaned: String = s
        .trim()
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | ' ' => '_',
            _ => c,
        })
        .collect();
    if cleaned.is_empty() {
        "Unknown".to_string()
    } else {
        cleaned.chars().take(48).collect()
    }
}

/// Shared helper to ensure OsStr wide strings stay well-formed if needed.
#[allow(dead_code)]
fn _to_wide(s: &Path) -> Vec<u16> {
    s.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_driver_version_blob_known_sample() {
        // Real Version blob captured from Lenovo AcpiVpc oem77.inf (2026-05-19, 15.11.30.14)
        let blob: Vec<u8> = vec![
            0x00, 0xFF, 0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x7D, 0xE9, 0x36, 0x4D, 0x25, 0xE3,
            0xCE, 0x11, 0xBF, 0xC1, 0x08, 0x00, 0x2B, 0xE1, 0x03, 0x18, 0x00, 0x80, 0x82, 0x6F,
            0x22, 0xE7, 0xDC, 0x01, 0x0E, 0x00, 0x1E, 0x00, 0x0B, 0x00, 0x0F, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let (guid, _ft, date, _rank, ver) = decode_driver_version_blob(&blob);
        assert_eq!(guid, "{4d36e97d-e325-11ce-bfc1-08002be10318}");
        assert_eq!(date, "2026-05-19");
        assert_eq!(ver, "15.11.30.14");
    }

    #[test]
    fn test_list_drivers_and_export_sandbox() {
        let drivers = list_drivers();
        assert!(drivers
            .iter()
            .all(|d| d.published_name.to_ascii_lowercase().starts_with("oem")));

        // Some Windows runner images have no third-party driver packages to export.
        if let Some(smallest) = drivers.iter().min_by_key(|d| d.size_bytes) {
            let temp_dir = std::env::temp_dir()
                .join(format!("dismnext_drv_backup_{}", std::process::id()));
            let _ = fs::remove_dir_all(&temp_dir);
            let names = vec![smallest.published_name.clone()];
            let report = export_drivers(&temp_dir, Some(&names)).unwrap();
            assert_eq!(report.exported_count, 1);
            assert!(Path::new(&report.manifest_path).exists());
            assert!(temp_dir.join("Restore-Drivers.cmd").exists());
            let _ = fs::remove_dir_all(&temp_dir);
        }
    }

    #[test]
    #[ignore = "Measures the host's driver store, run separately from functional tests"]
    fn measure_uncached_driver_listing() {
        let started = Instant::now();
        let drivers = compute_drivers_uncached();
        println!(
            "Enumerated {} drivers in {:?}",
            drivers.len(),
            started.elapsed()
        );
    }

    #[test]
    fn test_delete_driver_rejects_invalid_names() {
        assert!(delete_driver("acpivpc.inf").is_err());
        assert!(delete_driver("oemabc.inf").is_err());
        assert!(delete_driver("oem1.sys").is_err());
    }

    #[test]
    fn test_outdated_duplicate_requires_provider_and_descriptor_coverage() {
        let make_drv = |pub_name: &str, provider: &str, ver: &str, rank: u64, hw: &[&str]| DriverPackageInfo {
            published_name: pub_name.to_string(),
            original_name: "usbdev.inf".to_string(),
            provider: provider.to_string(),
            class_name: "USB".to_string(),
            class_guid: "{36fc9e60-c465-11cf-8056-444553540000}".to_string(),
            version: ver.to_string(),
            date: "2026-01-01".to_string(),
            signer_name: provider.to_string(),
            repo_folder: pub_name.to_string(),
            repo_path: pub_name.to_string(),
            size_bytes: 1024,
            is_outdated_duplicate: false,
            newer_version: None,
            version_rank: rank,
            date_filetime: 100,
            published_num: 1,
            hw_descriptors: hw.iter().map(|s| s.to_string()).collect(),
        };

        let mut list = vec![
            make_drv("oem10.inf", "VendorA", "2.0.0.0", 2, &["usb\\vid_1111&pid_0001"]),
            // Same provider, covered HWID -> should be marked outdated
            make_drv("oem11.inf", "VendorA", "1.0.0.0", 1, &["usb\\vid_1111&pid_0001"]),
            // Same provider, extra HWID not in oem10 -> must NOT be marked outdated
            make_drv("oem12.inf", "VendorA", "1.0.0.0", 1, &["usb\\vid_1111&pid_0002"]),
            // Different provider with same INF name -> must NOT be marked outdated
            make_drv("oem13.inf", "VendorB", "1.0.0.0", 1, &["usb\\vid_1111&pid_0001"]),
        ];

        mark_outdated_duplicates(&mut list);
        assert!(!list[0].is_outdated_duplicate);
        assert!(list[1].is_outdated_duplicate);
        assert!(!list[2].is_outdated_duplicate);
        assert!(!list[3].is_outdated_duplicate);
    }
}
