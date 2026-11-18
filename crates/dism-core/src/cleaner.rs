use crate::catalog::{Action, CleanupRule, CleanupScan, LocalizedText};
use crate::evaluator::{
    eval_condition, expand_expression, resolve_wildcard_roots, wildcard_match_ci,
};
use crate::sysinfo::SystemInfo;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, DELETE, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
};

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
const MAX_PREVIEW_FILES: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileStatus {
    Ready,
    Locked,
    Protected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewFile {
    pub path: String,
    pub size_bytes: u64,
    pub status: FileStatus,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupScanResult {
    pub id: String,
    pub name: LocalizedText,
    pub group: LocalizedText,
    pub description: LocalizedText,
    pub warning: Option<LocalizedText>,
    pub level: u8,
    pub source: String,
    pub applicable: bool,
    pub is_system_task: bool,
    pub system_task_hint: Option<LocalizedText>,
    pub cleanable_bytes: u64,
    pub cleanable_count: usize,
    pub skipped_locked_bytes: u64,
    pub skipped_locked_count: usize,
    pub skipped_protected_bytes: u64,
    pub skipped_protected_count: usize,
    pub preview_files: Vec<PreviewFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleCleanOutcome {
    pub id: String,
    pub name: LocalizedText,
    pub dry_run: bool,
    pub deleted_bytes: u64,
    pub deleted_files: usize,
    pub skipped_locked_files: usize,
    pub skipped_protected_files: usize,
    pub failed_files: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupExecutionReport {
    pub dry_run: bool,
    pub total_deleted_bytes: u64,
    pub total_deleted_files: usize,
    pub total_skipped_files: usize,
    pub outcomes: Vec<RuleCleanOutcome>,
}

struct CollectedTargets {
    files: Vec<PreviewFile>,
    removable_dirs: Vec<PathBuf>,
    is_system_task: bool,
    system_task_hint: Option<LocalizedText>,
}

/// Scans all applicable cleanup rules in parallel without modifying any files.
pub fn scan_all(rules: &[CleanupRule], sys: &SystemInfo) -> Vec<CleanupScanResult> {
    rules
        .par_iter()
        .map(|rule| scan_rule(rule, sys))
        .filter(|res| res.applicable)
        .collect()
}

/// Scans a single cleanup rule and produces file-level diagnostics + up to 100 largest preview entries.
pub fn scan_rule(rule: &CleanupRule, sys: &SystemInfo) -> CleanupScanResult {
    let Some(active_scan) = select_active_scan(rule, sys) else {
        return empty_result(rule, false);
    };

    let name = active_scan
        .name_override
        .clone()
        .unwrap_or_else(|| rule.name.clone());
    let description = active_scan
        .description_override
        .clone()
        .unwrap_or_else(|| rule.description.clone());
    let warning = active_scan
        .warning_override
        .clone()
        .or_else(|| rule.warning.clone());

    let collected = collect_scan_targets(active_scan, sys);
    let mut cleanable_bytes = 0u64;
    let mut cleanable_count = 0usize;
    let mut skipped_locked_bytes = 0u64;
    let mut skipped_locked_count = 0usize;
    let mut skipped_protected_bytes = 0u64;
    let mut skipped_protected_count = 0usize;

    for f in &collected.files {
        match f.status {
            FileStatus::Ready => {
                cleanable_bytes += f.size_bytes;
                cleanable_count += 1;
            }
            FileStatus::Locked => {
                skipped_locked_bytes += f.size_bytes;
                skipped_locked_count += 1;
            }
            FileStatus::Protected => {
                skipped_protected_bytes += f.size_bytes;
                skipped_protected_count += 1;
            }
        }
    }

    let mut preview_files = collected.files;
    preview_files.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    preview_files.truncate(MAX_PREVIEW_FILES);

    CleanupScanResult {
        id: rule.id.clone(),
        name,
        group: rule.group.clone(),
        description,
        warning,
        level: rule.level,
        source: rule.source.clone(),
        applicable: true,
        is_system_task: collected.is_system_task,
        system_task_hint: collected.system_task_hint,
        cleanable_bytes,
        cleanable_count,
        skipped_locked_bytes,
        skipped_locked_count,
        skipped_protected_bytes,
        skipped_protected_count,
        preview_files,
    }
}

/// Executes cleanup (or Dry-Run simulation when `dry_run == true`) for selected rule IDs.
pub fn execute_cleanup(
    rules: &[CleanupRule],
    rule_ids: &[String],
    sys: &SystemInfo,
    dry_run: bool,
) -> CleanupExecutionReport {
    let wanted: HashSet<&str> = rule_ids.iter().map(|s| s.as_str()).collect();
    let selected: Vec<&CleanupRule> = rules
        .iter()
        .filter(|r| wanted.contains(r.id.as_str()))
        .collect();

    let outcomes: Vec<RuleCleanOutcome> = selected
        .par_iter()
        .map(|rule| execute_single_rule(rule, sys, dry_run))
        .collect();

    let total_deleted_bytes = outcomes.iter().map(|o| o.deleted_bytes).sum();
    let total_deleted_files = outcomes.iter().map(|o| o.deleted_files).sum();
    let total_skipped_files = outcomes
        .iter()
        .map(|o| o.skipped_locked_files + o.skipped_protected_files)
        .sum();

    CleanupExecutionReport {
        dry_run,
        total_deleted_bytes,
        total_deleted_files,
        total_skipped_files,
        outcomes,
    }
}

fn execute_single_rule(rule: &CleanupRule, sys: &SystemInfo, dry_run: bool) -> RuleCleanOutcome {
    let Some(active_scan) = select_active_scan(rule, sys) else {
        return RuleCleanOutcome {
            id: rule.id.clone(),
            name: rule.name.clone(),
            dry_run,
            deleted_bytes: 0,
            deleted_files: 0,
            skipped_locked_files: 0,
            skipped_protected_files: 0,
            failed_files: 0,
            errors: Vec::new(),
        };
    };

    let collected = collect_scan_targets(active_scan, sys);
    let mut deleted_bytes = 0u64;
    let mut deleted_files = 0usize;
    let mut skipped_locked_files = 0usize;
    let mut skipped_protected_files = 0usize;
    let mut failed_files = 0usize;
    let mut errors = Vec::new();

    for f in &collected.files {
        match f.status {
            FileStatus::Locked => {
                skipped_locked_files += 1;
            }
            FileStatus::Protected => {
                skipped_protected_files += 1;
            }
            FileStatus::Ready => {
                if dry_run {
                    deleted_bytes += f.size_bytes;
                    deleted_files += 1;
                } else {
                    match remove_file_safe(Path::new(&f.path)) {
                        Ok(()) => {
                            deleted_bytes += f.size_bytes;
                            deleted_files += 1;
                        }
                        Err(e) => {
                            failed_files += 1;
                            if errors.len() < 10 {
                                errors.push(format!("{}: {}", f.path, e));
                            }
                        }
                    }
                }
            }
        }
    }

    // Remove emptied subdirectories deepest-first (never deleting preserved root folders)
    if !dry_run {
        let mut dirs = collected.removable_dirs;
        dirs.sort_by(|a, b| b.components().count().cmp(&a.components().count()));
        for d in dirs {
            let _ = std::fs::remove_dir(&d);
        }
    }

    RuleCleanOutcome {
        id: rule.id.clone(),
        name: active_scan
            .name_override
            .clone()
            .unwrap_or_else(|| rule.name.clone()),
        dry_run,
        deleted_bytes,
        deleted_files,
        skipped_locked_files,
        skipped_protected_files,
        failed_files,
        errors,
    }
}

fn remove_file_safe(path: &Path) -> std::io::Result<()> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        let mut perms = meta.permissions();
        if perms.readonly() {
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    std::fs::remove_file(path)
}

fn empty_result(rule: &CleanupRule, applicable: bool) -> CleanupScanResult {
    CleanupScanResult {
        id: rule.id.clone(),
        name: rule.name.clone(),
        group: rule.group.clone(),
        description: rule.description.clone(),
        warning: rule.warning.clone(),
        level: rule.level,
        source: rule.source.clone(),
        applicable,
        is_system_task: false,
        system_task_hint: None,
        cleanable_bytes: 0,
        cleanable_count: 0,
        skipped_locked_bytes: 0,
        skipped_locked_count: 0,
        skipped_protected_bytes: 0,
        skipped_protected_count: 0,
        preview_files: Vec::new(),
    }
}

fn select_active_scan<'a>(rule: &'a CleanupRule, sys: &SystemInfo) -> Option<&'a CleanupScan> {
    rule.scans
        .iter()
        .find(|scan| eval_condition(&scan.applicable, sys))
}

fn collect_scan_targets(scan: &CleanupScan, sys: &SystemInfo) -> CollectedTargets {
    let mut files = Vec::new();
    let mut seen_paths = HashSet::<String>::new();
    let mut removable_dirs = Vec::new();
    let mut is_system_task = false;
    let mut system_task_hint: Option<LocalizedText> = None;

    for action in &scan.actions {
        match action {
            Action::General {
                root_path,
                flags,
                id,
                queries,
                excluded,
            } => {
                if let Some(cond_id) = id {
                    if let Some(cond) = scan.named_applicables.get(cond_id) {
                        if !eval_condition(cond, sys) {
                            continue;
                        }
                    }
                }
                collect_general_action(
                    root_path,
                    flags,
                    queries,
                    excluded,
                    sys,
                    &mut files,
                    &mut seen_paths,
                    &mut removable_dirs,
                );
            }
            Action::Custom { proc_name, .. } => {
                handle_custom_proc(
                    proc_name,
                    sys,
                    &mut files,
                    &mut seen_paths,
                    &mut removable_dirs,
                    &mut is_system_task,
                    &mut system_task_hint,
                );
            }
            _ => {}
        }
    }

    CollectedTargets {
        files,
        removable_dirs,
        is_system_task,
        system_task_hint,
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_general_action(
    root_path: &str,
    flags: &str,
    queries: &[String],
    excluded: &[String],
    sys: &SystemInfo,
    files: &mut Vec<PreviewFile>,
    seen_paths: &mut HashSet<String>,
    removable_dirs: &mut Vec<PathBuf>,
) {
    let Some(expanded_root) = expand_expression(root_path, None) else {
        return;
    };

    let allow_dirs = flags_allows_dirs(flags);

    // Handle trailing wildcard file pattern directly in RootPath when queries is empty (e.g. `...\CLR_*\*.log`)
    let (effective_root, effective_queries): (String, Vec<String>) =
        if queries.is_empty() && has_leaf_wildcard(&expanded_root) {
            let norm = expanded_root.replace('/', "\\");
            if let Some(pos) = norm.rfind('\\') {
                (norm[..pos].to_string(), vec![norm[pos + 1..].to_string()])
            } else {
                (expanded_root, Vec::new())
            }
        } else {
            (expanded_root, queries.to_vec())
        };

    let roots = resolve_wildcard_roots(&effective_root, &sys.fixed_drives);
    for root_dir in roots {
        if !root_dir.exists() {
            continue;
        }
        let root_str = root_dir.to_string_lossy().into_owned();
        let expanded_exclusions: Vec<String> = excluded
            .iter()
            .filter_map(|ex| expand_expression(ex, Some(&root_str)))
            .filter(|s| !s.trim().is_empty())
            .collect();

        if effective_queries.is_empty() {
            if root_dir.is_file() {
                record_file(&root_dir, files, seen_paths);
            } else {
                walk_dir_filtered(
                    &root_dir,
                    &root_dir,
                    &expanded_exclusions,
                    allow_dirs,
                    false,
                    files,
                    seen_paths,
                    removable_dirs,
                );
            }
        } else {
            for q in &effective_queries {
                let q_norm = q.replace('/', "\\");
                let pattern = format!(
                    "{}\\{}",
                    root_str.trim_end_matches('\\'),
                    q_norm.trim_start_matches('\\')
                );
                let matched_roots = resolve_wildcard_roots(&pattern, &sys.fixed_drives);
                for target in matched_roots {
                    if is_excluded(&target, &root_dir, &expanded_exclusions) {
                        continue;
                    }
                    if target.is_file() {
                        record_file(&target, files, seen_paths);
                    } else if target.is_dir() && !is_reparse_point(&target) {
                        walk_dir_filtered(
                            &target,
                            &root_dir,
                            &expanded_exclusions,
                            allow_dirs,
                            allow_dirs,
                            files,
                            seen_paths,
                            removable_dirs,
                        );
                    }
                }
            }
        }
    }
}

fn handle_custom_proc(
    proc_name: &str,
    sys: &SystemInfo,
    files: &mut Vec<PreviewFile>,
    seen_paths: &mut HashSet<String>,
    removable_dirs: &mut Vec<PathBuf>,
    is_system_task: &mut bool,
    system_task_hint: &mut Option<LocalizedText>,
) {
    match proc_name {
        "NCWinEventLogOnlineCleanup" => {
            collect_general_action(
                r"%SystemRoot%\System32\winevt\Logs",
                "File",
                &["*.evtx".to_string()],
                &[],
                sys,
                files,
                seen_paths,
                removable_dirs,
            );
        }
        "NCDOCacheOnlineCleanup" => {
            for p in [
                r"%SystemRoot%\SoftwareDistribution\DeliveryOptimization",
                r"%SystemRoot%\DeliveryOptimization",
                r"%SystemRoot%\ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization",
            ] {
                collect_general_action(p, "Directory|File", &[], &[], sys, files, seen_paths, removable_dirs);
            }
        }
        "NCPackageCacheFolderCleanup" => {
            collect_general_action(
                r"%ProgramData%\Package Cache",
                "Directory|File",
                &[],
                &[],
                sys,
                files,
                seen_paths,
                removable_dirs,
            );
        }
        "NCVSPackageCacheCleanup" => {
            collect_general_action(
                r"%ProgramData%\Microsoft\VisualStudio\Packages",
                "Directory|File",
                &[],
                &[],
                sys,
                files,
                seen_paths,
                removable_dirs,
            );
        }
        "NCInstallerFolderCleanup" => {
            *is_system_task = true;
            *system_task_hint = Some(LocalizedText {
                zh: "Windows Installer 孤儿补丁需要严格校验 MSI 数据库引用，Phase 1 暂不直接删除以保安全。".to_string(),
                en: "Orphaned Windows Installer (.msp) cleanup requires MSI database reference verification and is skipped in Phase 1 for safety.".to_string(),
            });
        }
        "DismComponentCleanup" => {
            *is_system_task = true;
            *system_task_hint = Some(LocalizedText {
                zh: "系统组件存储清理需调用 Windows DISM 接口 (Dism.exe /Online /Cleanup-Image /StartComponentCleanup)，将在 Phase 2 接入。".to_string(),
                en: "WinSxS component cleanup uses Windows DISM (/StartComponentCleanup) and will be wired natively in Phase 2.".to_string(),
            });
        }
        "DismCompactOs" => {
            *is_system_task = true;
            *system_task_hint = Some(LocalizedText {
                zh: "CompactOS 文件系统压缩属于系统级操作 (compact.exe /CompactOS:always)，将在 Phase 2 接入。".to_string(),
                en: "CompactOS compression is a system-level operation (compact.exe /CompactOS:always) scheduled for Phase 2.".to_string(),
            });
        }
        "DismHardLinkMerge" => {
            *is_system_task = true;
            *system_task_hint = Some(LocalizedText {
                zh: "NTFS 硬链接合并属于底层系统级任务，将在后续阶段评估接入。".to_string(),
                en: "NTFS hardlink merge is an advanced disk operation excluded from Phase 1 safe cleanup.".to_string(),
            });
        }
        "DismDriverCleanup" => {
            *is_system_task = true;
            *system_task_hint = Some(LocalizedText {
                zh: "过期驱动程序清理需枚举 DriverStore 包引用，将在 Phase 2 接入。".to_string(),
                en: "Outdated driver store cleanup requires DriverStore enumeration scheduled for Phase 2.".to_string(),
            });
        }
        "DismAppxsCleanup" | "NCCorruptedAppXOnlineCleanup" => {
            *is_system_task = true;
            *system_task_hint = Some(LocalizedText {
                zh: "Appx 应用包清理将在 Phase 2 通过 WinRT PackageManager 接口原生实现。".to_string(),
                en: "Appx package cleanup will use native WinRT PackageManager APIs in Phase 2.".to_string(),
            });
        }
        "NCSRPointOnlineCleanup" => {
            *is_system_task = true;
            *system_task_hint = Some(LocalizedText {
                zh: "系统还原点管理需调用 VSS 卷影复制接口，将在 Phase 2 接入。".to_string(),
                en: "System Restore point management requires VSS APIs scheduled for Phase 2.".to_string(),
            });
        }
        _ => {
            *is_system_task = true;
        }
    }
}

fn has_leaf_wildcard(path: &str) -> bool {
    let norm = path.replace('/', "\\");
    if let Some(last) = norm.rsplit('\\').next() {
        last.contains('*') || last.contains('?')
    } else {
        false
    }
}

fn flags_allows_dirs(flags: &str) -> bool {
    if flags.trim().is_empty() {
        return true;
    }
    flags.split('|').any(|f| f.trim().eq_ignore_ascii_case("Directory"))
}

#[allow(clippy::too_many_arguments)]
fn walk_dir_filtered(
    dir: &Path,
    anchor_root: &Path,
    exclusions: &[String],
    allow_dirs: bool,
    remove_self: bool,
    files: &mut Vec<PreviewFile>,
    seen_paths: &mut HashSet<String>,
    removable_dirs: &mut Vec<PathBuf>,
) {
    let mut dirs_in_walk = Vec::new();
    let walker = WalkDir::new(dir).follow_links(false).into_iter();
    for entry in walker.filter_entry(|e| {
        if e.path() == dir {
            return true;
        }
        if is_reparse_point(e.path()) {
            return false;
        }
        !is_excluded(e.path(), anchor_root, exclusions)
    }) {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if entry.file_type().is_file() {
            record_file(path, files, seen_paths);
        } else if entry.file_type().is_dir() && allow_dirs && (path != dir || remove_self) {
            dirs_in_walk.push(path.to_path_buf());
        }
    }
    removable_dirs.extend(dirs_in_walk);
}

fn is_excluded(path: &Path, anchor_root: &Path, exclusions: &[String]) -> bool {
    if exclusions.is_empty() {
        return false;
    }
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let rel = path
        .strip_prefix(anchor_root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file_name.clone());

    for ex in exclusions {
        if wildcard_match_ci(ex, &file_name) || wildcard_match_ci(ex, &rel) {
            return true;
        }
    }
    false
}

fn is_reparse_point(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| (m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0)
        .unwrap_or(false)
}

fn record_file(path: &Path, files: &mut Vec<PreviewFile>, seen_paths: &mut HashSet<String>) {
    let path_str = path.to_string_lossy().into_owned();
    let key = path_str.to_ascii_lowercase();
    if !seen_paths.insert(key) {
        return;
    }
    let size_bytes = std::fs::symlink_metadata(path).map(|m| m.len()).unwrap_or(0);
    let (status, reason) = diagnose_file(path, &path_str);
    files.push(PreviewFile {
        path: path_str,
        size_bytes,
        status,
        reason,
    });
}

fn diagnose_file(path: &Path, path_str: &str) -> (FileStatus, Option<String>) {
    let lower = path_str.to_ascii_lowercase();
    if lower.contains(r"\windows defender\") {
        return (
            FileStatus::Protected,
            Some("Windows Defender self-protection (0x5)".to_string()),
        );
    }
    if lower.contains(r"\windows\assembly\nativeimages_") {
        return (
            FileStatus::Protected,
            Some("Protected .NET NativeImages ACL (0x5)".to_string()),
        );
    }

    let wide: Vec<u16> = OsStr::new(path).encode_wide().chain(Some(0)).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            DELETE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle != INVALID_HANDLE_VALUE {
        unsafe { CloseHandle(handle) };
        return (FileStatus::Ready, None);
    }

    let err = unsafe { GetLastError() };
    if err == ERROR_SHARING_VIOLATION || err == ERROR_LOCK_VIOLATION {
        (
            FileStatus::Locked,
            Some(format!("In use by another process (0x{:X})", err)),
        )
    } else if err == ERROR_ACCESS_DENIED {
        (
            FileStatus::Protected,
            Some("Access denied / system ACL protected (0x5)".to_string()),
        )
    } else {
        (
            FileStatus::Protected,
            Some(format!("Restricted handle (0x{:X})", err)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{CleanupScan, Condition};
    use std::collections::HashMap;
    use std::fs::{self, File};
    use std::io::Write;

    #[test]
    fn test_scan_exclusion_and_dry_run_vs_execute_in_sandbox() {
        let base = std::env::temp_dir().join(format!("dismnext_clean_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let sub = base.join("cache_dir");
        fs::create_dir_all(&sub).unwrap();

        let keep_file = base.join("keep.ini");
        let junk_a = base.join("junk1.tmp");
        let junk_b = sub.join("junk2.tmp");
        File::create(&keep_file).unwrap().write_all(b"keep").unwrap();
        File::create(&junk_a).unwrap().write_all(b"1234567890").unwrap();
        File::create(&junk_b).unwrap().write_all(b"abcdef").unwrap();

        let rule = CleanupRule {
            id: "test-001".to_string(),
            name: LocalizedText {
                zh: "测试规则".into(),
                en: "Test Rule".into(),
            },
            group: LocalizedText {
                zh: "临时文件".into(),
                en: "Temp".into(),
            },
            description: LocalizedText {
                zh: "".into(),
                en: "".into(),
            },
            warning: None,
            level: 3,
            work: None,
            source: "test".to_string(),
            scans: vec![CleanupScan {
                work: 1,
                scan_type: None,
                name_override: None,
                description_override: None,
                warning_override: None,
                applicable: Condition::True,
                named_applicables: HashMap::new(),
                actions: vec![Action::General {
                    root_path: base.to_string_lossy().into_owned(),
                    flags: "Directory|File".to_string(),
                    id: None,
                    queries: Vec::new(),
                    excluded: vec!["keep.ini".to_string()],
                }],
                rescan: false,
                restart: None,
            }],
        };

        let sys = SystemInfo::collect();
        let scan_res = scan_rule(&rule, &sys);
        assert!(scan_res.applicable);
        assert_eq!(scan_res.cleanable_count, 2);
        assert_eq!(scan_res.cleanable_bytes, 16);

        let dry = execute_cleanup(&[rule.clone()], &["test-001".to_string()], &sys, true);
        assert!(dry.dry_run);
        assert_eq!(dry.total_deleted_files, 2);
        assert!(junk_a.exists() && junk_b.exists() && keep_file.exists());

        let real = execute_cleanup(&[rule], &["test-001".to_string()], &sys, false);
        assert!(!real.dry_run);
        assert_eq!(real.total_deleted_files, 2);
        assert_eq!(real.total_deleted_bytes, 16);
        assert!(!junk_a.exists() && !junk_b.exists());
        assert!(keep_file.exists());
        assert!(base.exists()); // root folder preserved when queries empty

        let _ = fs::remove_dir_all(&base);
    }
}
