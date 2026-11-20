use super::{
    collect_general_action, record_file_with_size, CleanupPath, CollectedFiles, FileStatus,
    PreviewFile, TargetKey,
};
use crate::catalog::{Action, CleanupScan, LocalizedText};
use crate::sysinfo::SystemInfo;
use crate::win_util::{has_reparse_point_in_ancestors, windows_dir_or_default};
use std::collections::HashSet;
use std::path::PathBuf;

/// Returns true if the Builtin custom proc is triggered via dedicated system task actions
/// rather than normal batch file-checkbox deletion.
pub fn is_deferred_system_task_proc(proc_name: &str) -> bool {
    matches!(
        proc_name,
        "BuiltinComponentCleanup"
            | "BuiltinCompactOs"
            | "BuiltinHardLinkMerge"
            | "BuiltinAppxCleanup"
            | "BuiltinCorruptedAppxCleanup"
            | "BuiltinRestorePointCleanup"
    )
}

/// Returns the first heavy system-task `Custom` proc name associated with a scan, if any.
pub fn scan_custom_proc_name(scan: &CleanupScan) -> Option<String> {
    for action in &scan.actions {
        if let Action::Custom { proc_name, .. } = action {
            if is_deferred_system_task_proc(proc_name)
                || matches!(
                    proc_name.as_str(),
                    "BuiltinDriverCleanup" | "BuiltinInstallerCleanup"
                )
            {
                return Some(proc_name.clone());
            }
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_custom_proc(
    proc_name: &str,
    sys: &SystemInfo,
    files: &mut CollectedFiles<'_>,
    seen_paths: &mut HashSet<TargetKey>,
    removable_dirs: &mut Vec<CleanupPath>,
    is_system_task: &mut bool,
    system_task_proc: &mut Option<String>,
    system_task_hint: &mut Option<LocalizedText>,
    probe_budget: &mut usize,
) {
    match proc_name {
        "BuiltinEventLogCleanup" => {
            collect_general_action(
                r"%SystemRoot%\System32\winevt\Logs",
                "File",
                &["*.evtx".to_string()],
                &[],
                sys,
                files,
                seen_paths,
                removable_dirs,
                probe_budget,
            );
        }
        "BuiltinDeliveryOptCleanup" => {
            for p in [
                r"%SystemRoot%\SoftwareDistribution\DeliveryOptimization",
                r"%SystemRoot%\DeliveryOptimization",
                r"%SystemRoot%\ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization",
            ] {
                collect_general_action(
                    p,
                    "Directory|File",
                    &[],
                    &[],
                    sys,
                    files,
                    seen_paths,
                    removable_dirs,
                    probe_budget,
                );
            }
        }
        "BuiltinPackageCacheCleanup" => {
            collect_general_action(
                r"%ProgramData%\Package Cache",
                "Directory|File",
                &[],
                &[],
                sys,
                files,
                seen_paths,
                removable_dirs,
                probe_budget,
            );
        }
        "BuiltinVsPackageCacheCleanup" => {
            collect_general_action(
                r"%ProgramData%\Microsoft\VisualStudio\Packages",
                "Directory|File",
                &[],
                &[],
                sys,
                files,
                seen_paths,
                removable_dirs,
                probe_budget,
            );
        }
        "BuiltinInstallerCleanup" => {
            let orphans = crate::systasks::find_orphaned_installer_files();
            let orphan_count = orphans.len();
            for (path, sz) in orphans {
                if files.cancelled() {
                    return;
                }
                let reparse = has_reparse_point_in_ancestors(&path, true);
                record_file_with_size(
                    &path,
                    sz,
                    reparse,
                    files,
                    seen_paths,
                    sys.is_admin,
                    probe_budget,
                );
            }
            *is_system_task = false;
            *system_task_proc = Some(proc_name.to_string());
            *system_task_hint = Some(LocalizedText {
                zh: format!(
                    "已交叉核验 Installer\\UserData / Classes / Uninstall 数据库，检出 {} 个无引用 .msi/.msp 安装缓存；执行清理时将先移入回收站以便随时恢复。",
                    orphan_count
                ),
                en: format!(
                    "Cross-checked Installer\\UserData, Classes, and Uninstall databases and found {} unreferenced .msi/.msp file(s); cleanup moves files to the Windows Recycle Bin for reversibility.",
                    orphan_count
                ),
            });
        }
        "BuiltinComponentCleanup" => {
            for p in [
                r"%SystemRoot%\WinSxS\Temp\PendingDeletes",
                r"%SystemRoot%\WinSxS\Temp\PendingRenames",
            ] {
                collect_general_action(
                    p,
                    "File",
                    &[],
                    &[],
                    sys,
                    files,
                    seen_paths,
                    removable_dirs,
                    probe_budget,
                );
            }
            *is_system_task = true;
            *system_task_proc = Some(proc_name.to_string());
            *system_task_hint = Some(LocalizedText {
                zh: "已启用 DISM 组件存储深度清理：可在右侧操作区直接点击“执行 DISM 组件存储清理”启动 /StartComponentCleanup。".to_string(),
                en: "WinSxS component cleanup ready: trigger DISM /StartComponentCleanup directly from the action panel.".to_string(),
            });
        }
        "BuiltinCompactOs" => {
            let (enabled, _raw) = crate::systasks::query_compact_os();
            *is_system_task = true;
            *system_task_proc = Some(proc_name.to_string());
            *system_task_hint = Some(LocalizedText {
                zh: format!(
                    "当前状态：{} CompactOS 系统文件压缩（可在右侧直接启用或还原）。",
                    if enabled { "已启用" } else { "未启用" }
                ),
                en: format!(
                    "Live state: CompactOS compression is currently {} (toggle via action panel).",
                    if enabled { "ENABLED" } else { "DISABLED" }
                ),
            });
        }
        "BuiltinHardLinkMerge" => {
            *is_system_task = true;
            *system_task_proc = Some(proc_name.to_string());
            *system_task_hint = Some(LocalizedText {
                zh: "NTFS 硬链接合并涉及底层卷修改，建议先完成系统组件清理与驱动去重。".to_string(),
                en: "NTFS hardlink merge operates at volume level; prefer WinSxS cleanup and driver deduplication first.".to_string(),
            });
        }
        "BuiltinDriverCleanup" => {
            let drivers = crate::drivers::list_drivers();
            let outdated: Vec<_> = drivers
                .into_iter()
                .filter(|d| d.is_outdated_duplicate)
                .collect();
            for d in &outdated {
                let key = TargetKey::Driver(d.published_name.to_ascii_lowercase());
                if let Some((size_bytes, status, _)) =
                    files.observe(&key, seen_paths, d.size_bytes, false, || {
                        (FileStatus::Ready, None)
                    })
                {
                    let newer = d.newer_version.as_deref().unwrap_or("newer");
                    files.record(
                        key,
                        &driver_inf_path(&d.published_name),
                        PreviewFile {
                            path: driver_target_label(d),
                            size_bytes,
                            status,
                            reason: Some(format!(
                                "已被更新版本 {} 取代，将通过 pnputil 安全移除 ({})",
                                newer, d.published_name
                            )),
                            locking_processes: Vec::new(),
                        },
                    );
                }
            }
            *is_system_task = false;
            *system_task_proc = Some(proc_name.to_string());
            *system_task_hint = Some(LocalizedText {
                zh: format!(
                    "已识别 {} 个被新版本取代的重复旧驱动包（勾选执行或点击右侧按钮即可安全卸载，当前生效驱动保留）。",
                    outdated.len()
                ),
                en: format!(
                    "Detected {} outdated duplicate driver package(s) safely removable via pnputil while keeping active drivers.",
                    outdated.len()
                ),
            });
        }
        "BuiltinAppxCleanup" | "BuiltinCorruptedAppxCleanup" => {
            for p in [
                r"%ProgramFiles%\WindowsApps\DeletedAllUserPackages",
                r"%ProgramFiles%\WindowsApps\Deleted",
                r"%LOCALAPPDATA%\Packages\*\TempState",
            ] {
                collect_general_action(
                    p,
                    "Directory|File",
                    &[],
                    &[],
                    sys,
                    files,
                    seen_paths,
                    removable_dirs,
                    probe_budget,
                );
            }
            *is_system_task = true;
            *system_task_proc = Some(proc_name.to_string());
            *system_task_hint = Some(LocalizedText {
                zh: "已扫描遗留 Appx 临时缓存；如需整包卸载预装 UWP 应用，请前往左侧“预装应用管理”页面。".to_string(),
                en: "Scanned leftover Appx caches; use the AppX Manager view in the sidebar for full package removal.".to_string(),
            });
        }
        "BuiltinRestorePointCleanup" => {
            *is_system_task = true;
            *system_task_proc = Some(proc_name.to_string());
            *system_task_hint = Some(LocalizedText {
                zh: "可通过右侧系统任务按钮调用 vssadmin 删除最旧卷影复制还原点以释放磁盘空间。".to_string(),
                en: "Run oldest VSS restore point cleanup directly from the right-hand system task panel.".to_string(),
            });
        }
        _ => {
            *is_system_task = true;
            *system_task_proc = Some(proc_name.to_string());
        }
    }
}

pub(super) fn driver_target_label(driver: &crate::drivers::DriverPackageInfo) -> String {
    format!(
        "{} [{} · {} v{} -> {}]",
        driver.repo_path,
        driver.published_name,
        driver.original_name,
        driver.version,
        driver.newer_version.as_deref().unwrap_or("newer")
    )
}

fn driver_inf_path(name: &str) -> PathBuf {
    windows_dir_or_default().join("INF").join(name)
}
