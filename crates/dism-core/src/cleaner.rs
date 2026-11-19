use crate::catalog::{Action, CleanupRule, CleanupScan, LocalizedText};
use crate::evaluator::{
    eval_condition, expand_expression, resolve_wildcard_roots_until, wildcard_match_ci,
};
use crate::scan_progress::{ScanPhase, ScanProgress};
use crate::sysinfo::SystemInfo;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{hash_map::Entry, BTreeMap, HashMap, HashSet};
use std::ffi::OsStr;
use std::hash::{BuildHasher, RandomState};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
    #[serde(default)]
    pub locking_processes: Vec<crate::lockcheck::LockingProcessInfo>,
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
    pub system_task_proc: Option<String>,
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
    #[serde(default)]
    pub description: Option<LocalizedText>,
    #[serde(default)]
    pub warning: Option<LocalizedText>,
    #[serde(default)]
    pub level: Option<u8>,
    pub dry_run: bool,
    // Keep the aggregate fields for existing CLI, HTTP and MCP clients.
    pub deleted_bytes: u64,
    pub deleted_files: usize,
    #[serde(default)]
    pub actions: CleanupActionTotals,
    pub skipped_locked_files: usize,
    pub skipped_protected_files: usize,
    pub failed_files: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanupActionTotals {
    pub permanent_delete: CleanupTargetTotals,
    pub recycle: CleanupTargetTotals,
    pub remove_driver: CleanupTargetTotals,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupExecutionReport {
    pub dry_run: bool,
    pub total_deleted_bytes: u64,
    pub total_deleted_files: usize,
    pub total_skipped_files: usize,
    pub outcomes: Vec<RuleCleanOutcome>,
}

#[derive(Debug, Default, Serialize)]
pub struct CleanupScanSummary {
    pub cleanable_bytes: u64,
    pub skipped_locked_bytes: u64,
    pub skipped_protected_bytes: u64,
    pub overlaps: Vec<CleanupOverlap>,
}

#[derive(Debug, Serialize)]
pub struct CleanupOverlap {
    pub rule_ids: Vec<String>,
    pub cleanable_bytes: u64,
}

#[derive(Debug, Default)]
pub struct CleanupBatchScan {
    pub items: Vec<CleanupScanResult>,
    pub summary: CleanupScanSummary,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum TargetKey {
    File(String),
    Driver(String),
}

impl TargetKey {
    fn file(path: &Path) -> Self {
        let absolute = (!path.is_absolute()
            || path.components().any(|part| part == Component::ParentDir))
        .then(|| std::path::absolute(path).ok())
        .flatten();
        let path = absolute.as_deref().unwrap_or(path);
        let normalized = path.components().collect::<PathBuf>();
        let name = normalized
            .to_string_lossy()
            .replace('/', "\\")
            .to_ascii_lowercase();
        if let Some(rest) = name.strip_prefix(r"\\?\unc\") {
            return Self::File(format!(r"\\{rest}"));
        }
        let name = match name.strip_prefix(r"\\?\") {
            Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_owned(),
            _ => name,
        };
        Self::File(name)
    }
}

struct IndexedTarget {
    size: u64,
    status: FileStatus,
    reason: Option<String>,
    first_rule: usize,
    other_rules: Vec<usize>,
}

const INDEX_SHARDS: usize = 32;

#[derive(Default)]
struct ScanIndex<'a> {
    hasher: RandomState,
    targets: [Mutex<HashMap<TargetKey, IndexedTarget>>; INDEX_SHARDS],
    progress: Option<&'a ScanProgress>,
}

impl ScanIndex<'_> {
    fn observe(
        &self,
        key: &TargetKey,
        rule: usize,
        size: u64,
        diagnose: impl FnOnce() -> (FileStatus, Option<String>),
    ) -> Option<(u64, FileStatus, Option<String>)> {
        let shard = self.hasher.hash_one(key) as usize % INDEX_SHARDS;
        let mut targets = self.targets[shard].lock().unwrap();
        if self.progress.is_some_and(ScanProgress::is_cancelled) {
            return None;
        }
        // Diagnose a shared path once so parallel probes never lock each other out.
        match targets.entry(key.clone()) {
            Entry::Vacant(entry) => {
                let (status, reason) = diagnose();
                if let Some(progress) = self.progress {
                    progress.file_processed();
                }
                entry.insert(IndexedTarget {
                    size,
                    status,
                    reason: reason.clone(),
                    first_rule: rule,
                    other_rules: Vec::new(),
                });
                Some((size, status, reason))
            }
            Entry::Occupied(mut entry) => {
                let target = entry.get_mut();
                if target.first_rule == rule || target.other_rules.contains(&rule) {
                    return None;
                }
                target.other_rules.push(rule);
                Some((target.size, target.status, target.reason.clone()))
            }
        }
    }

    fn summarize(self, rules: &[CleanupRule]) -> anyhow::Result<CleanupScanSummary> {
        let mut summary = CleanupScanSummary::default();
        let mut overlaps = BTreeMap::<Vec<usize>, u64>::new();
        for shard in self.targets {
            for mut target in shard.into_inner().unwrap().into_values() {
                if let Some(progress) = self.progress {
                    progress.check()?;
                }
                match target.status {
                    FileStatus::Ready => {
                        summary.cleanable_bytes += target.size;
                        if !target.other_rules.is_empty() && target.size != 0 {
                            target.other_rules.push(target.first_rule);
                            target.other_rules.sort_unstable();
                            *overlaps.entry(target.other_rules).or_default() += target.size;
                        }
                    }
                    FileStatus::Locked => summary.skipped_locked_bytes += target.size,
                    FileStatus::Protected => summary.skipped_protected_bytes += target.size,
                }
            }
        }
        summary.overlaps = overlaps
            .into_iter()
            .map(|(indices, cleanable_bytes)| CleanupOverlap {
                rule_ids: indices
                    .into_iter()
                    .map(|index| rules[index].id.clone())
                    .collect(),
                cleanable_bytes,
            })
            .collect();
        Ok(summary)
    }
}

struct CollectedTargets<'a> {
    files: CollectedFiles<'a>,
    removable_dirs: Vec<PathBuf>,
    is_system_task: bool,
    system_task_proc: Option<String>,
    system_task_hint: Option<LocalizedText>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanupTargetTotals {
    pub bytes: u64,
    pub count: usize,
}

#[derive(Default)]
struct CollectedFiles<'a> {
    preview_only: bool,
    index: Option<(&'a ScanIndex<'a>, usize)>,
    targets: Vec<(TargetKey, PreviewFile)>,
    items: Vec<PreviewFile>,
    ready: CleanupTargetTotals,
    locked: CleanupTargetTotals,
    protected: CleanupTargetTotals,
}

impl CollectedFiles<'_> {
    fn cancelled(&self) -> bool {
        self.index
            .and_then(|(index, _)| index.progress)
            .is_some_and(ScanProgress::is_cancelled)
    }

    fn observe(
        &self,
        key: &TargetKey,
        seen_paths: &mut HashSet<TargetKey>,
        size: u64,
        diagnose: impl FnOnce() -> (FileStatus, Option<String>),
    ) -> Option<(u64, FileStatus, Option<String>)> {
        if let Some((index, rule)) = self.index {
            return index.observe(key, rule, size, diagnose);
        }
        if !seen_paths.insert(key.clone()) {
            return None;
        }
        let (status, reason) = diagnose();
        Some((size, status, reason))
    }

    fn record(&mut self, key: TargetKey, file: PreviewFile) {
        if self.preview_only {
            self.push_preview(file);
        } else {
            self.targets.push((key, file));
        }
    }

    fn push_preview(&mut self, file: PreviewFile) {
        let totals = match file.status {
            FileStatus::Ready => &mut self.ready,
            FileStatus::Locked => &mut self.locked,
            FileStatus::Protected => &mut self.protected,
        };
        totals.bytes += file.size_bytes;
        totals.count += 1;
        self.items.push(file);
        if self.items.len() == MAX_PREVIEW_FILES * 2 {
            self.trim_preview();
        }
    }

    fn trim_preview(&mut self) {
        self.items.sort_by_key(|file| Reverse(file.size_bytes));
        self.items.truncate(MAX_PREVIEW_FILES);
    }
}

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

/// Scans all applicable cleanup rules in parallel without modifying any files.
pub fn scan_all(rules: &[CleanupRule], sys: &SystemInfo) -> Vec<CleanupScanResult> {
    rules
        .par_iter()
        .map(|rule| scan_rule(rule, sys))
        .filter(|res| res.applicable)
        .collect()
}

pub fn scan_all_with_summary(rules: &[CleanupRule], sys: &SystemInfo) -> CleanupBatchScan {
    scan_all_with_summary_inner(rules, sys, None)
        .expect("Untracked cleanup scans cannot be cancelled")
}

pub fn scan_all_with_progress(
    rules: &[CleanupRule],
    sys: &SystemInfo,
    progress: &ScanProgress,
) -> anyhow::Result<CleanupBatchScan> {
    scan_all_with_summary_inner(rules, sys, Some(progress))
}

fn scan_all_with_summary_inner(
    rules: &[CleanupRule],
    sys: &SystemInfo,
    progress: Option<&ScanProgress>,
) -> anyhow::Result<CleanupBatchScan> {
    let index = ScanIndex {
        progress,
        ..ScanIndex::default()
    };
    let items = rules
        .par_iter()
        .enumerate()
        .map(|(rule_index, rule)| {
            if let Some(progress) = progress {
                progress.check()?;
            }
            Ok(scan_rule_with_budget(
                rule,
                sys,
                64,
                Some((&index, rule_index)),
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    if let Some(progress) = progress {
        progress.check()?;
        progress.set_phase(ScanPhase::Finishing, None);
    }
    let summary = index.summarize(rules)?;
    Ok(CleanupBatchScan {
        items: items
            .into_iter()
            .filter(|result| result.applicable)
            .collect(),
        summary,
    })
}

/// Scans a single rule using fast budgeted handle probing (ideal for bulk `scan_all`).
pub fn scan_rule(rule: &CleanupRule, sys: &SystemInfo) -> CleanupScanResult {
    scan_rule_with_budget(rule, sys, 64, None)
}

/// Scans a single rule with full handle diagnostics and a single batched Restart Manager lock check
/// across up to the top 6 locked files in `preview_files`.
pub fn scan_rule_preview(rule: &CleanupRule, sys: &SystemInfo) -> CleanupScanResult {
    let mut res = scan_rule_with_budget(rule, sys, usize::MAX, None);
    let locked_indices: Vec<usize> = res
        .preview_files
        .iter()
        .enumerate()
        .filter(|(_, f)| f.status == FileStatus::Locked)
        .map(|(idx, _)| idx)
        .take(6)
        .collect();

    if !locked_indices.is_empty() {
        let locked_paths: Vec<&Path> = locked_indices
            .iter()
            .map(|&idx| Path::new(&res.preview_files[idx].path))
            .collect();
        let procs = crate::lockcheck::find_locking_processes_batch(&locked_paths);
        if !procs.is_empty() {
            let summary: Vec<String> = procs
                .iter()
                .take(3)
                .map(|p| format!("{} (PID {})", p.app_name, p.pid))
                .collect();
            let reason_text = format!("被进程占用: {}", summary.join(", "));
            for idx in locked_indices {
                res.preview_files[idx].reason = Some(reason_text.clone());
                res.preview_files[idx].locking_processes = procs.clone();
            }
        }
    }
    res
}

fn scan_rule_with_budget(
    rule: &CleanupRule,
    sys: &SystemInfo,
    probe_budget: usize,
    index: Option<(&ScanIndex<'_>, usize)>,
) -> CleanupScanResult {
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

    let mut collected = collect_scan_targets(active_scan, sys, probe_budget, true, index);
    collected.files.trim_preview();

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
        system_task_proc: collected.system_task_proc,
        system_task_hint: collected.system_task_hint,
        cleanable_bytes: collected.files.ready.bytes,
        cleanable_count: collected.files.ready.count,
        skipped_locked_bytes: collected.files.locked.bytes,
        skipped_locked_count: collected.files.locked.count,
        skipped_protected_bytes: collected.files.protected.bytes,
        skipped_protected_count: collected.files.protected.count,
        preview_files: collected.files.items,
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
    let (mut outcomes, targets, mut dirs) = plan_cleanup(rules, &wanted, sys, dry_run);
    for target in targets {
        let outcome = &mut outcomes[target.owner];
        match target.apply(dry_run) {
            Ok((FileStatus::Ready, bytes)) => {
                let totals = match target.action {
                    CleanupAction::Delete => &mut outcome.actions.permanent_delete,
                    CleanupAction::Recycle => &mut outcome.actions.recycle,
                    CleanupAction::RemoveDriver(_) => &mut outcome.actions.remove_driver,
                };
                totals.count += 1;
                totals.bytes += bytes;
                outcome.deleted_files += 1;
                outcome.deleted_bytes += bytes;
            }
            Ok((FileStatus::Locked, _)) => outcome.skipped_locked_files += 1,
            Ok((FileStatus::Protected, _)) => outcome.skipped_protected_files += 1,
            Err(error) => {
                outcome.failed_files += 1;
                if outcome.errors.len() < 10 {
                    outcome.errors.push(format!("{}: {error}", target.path));
                }
            }
        }
    }

    if !dry_run {
        dirs.sort_by_key(|dir| Reverse(dir.components().count()));
        for dir in dirs {
            if std::fs::symlink_metadata(&dir).is_ok_and(|meta| {
                meta.is_dir() && meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
            }) {
                let _ = std::fs::remove_dir(dir);
            }
        }
    }

    CleanupExecutionReport {
        dry_run,
        total_deleted_bytes: outcomes.iter().map(|o| o.deleted_bytes).sum(),
        total_deleted_files: outcomes.iter().map(|o| o.deleted_files).sum(),
        total_skipped_files: outcomes
            .iter()
            .map(|o| o.skipped_locked_files + o.skipped_protected_files)
            .sum(),
        outcomes,
    }
}

struct PlannedTarget {
    owner: usize,
    path: String,
    size: u64,
    status: FileStatus,
    action: CleanupAction,
}

enum CleanupAction {
    Delete,
    Recycle,
    RemoveDriver(String),
}

impl PlannedTarget {
    fn apply(&self, dry_run: bool) -> anyhow::Result<(FileStatus, u64)> {
        if dry_run || self.status != FileStatus::Ready {
            return Ok((self.status, self.size));
        }
        if let CleanupAction::RemoveDriver(name) = &self.action {
            crate::drivers::delete_driver(name)?;
            return Ok((FileStatus::Ready, self.size));
        }

        let path = Path::new(&self.path);
        let meta = std::fs::symlink_metadata(path)?;
        if !meta.is_file() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Ok((FileStatus::Protected, 0));
        }
        let (status, _) = diagnose_file(path, &self.path);
        if status != FileStatus::Ready {
            return Ok((status, 0));
        }
        if matches!(self.action, CleanupAction::Recycle) {
            crate::duplicate_recycle::move_to_recycle_bin(path)?;
        } else {
            remove_file_safe(path, meta.permissions())?;
        }
        Ok((FileStatus::Ready, meta.len()))
    }
}

fn plan_cleanup(
    rules: &[CleanupRule],
    wanted: &HashSet<&str>,
    sys: &SystemInfo,
    dry_run: bool,
) -> (Vec<RuleCleanOutcome>, Vec<PlannedTarget>, Vec<PathBuf>) {
    let mut outcomes = Vec::new();
    let mut targets: Vec<PlannedTarget> = Vec::new();
    let mut by_key = HashMap::<TargetKey, usize>::new();
    let mut dirs = Vec::new();
    let mut seen_dirs = HashSet::new();

    for rule in rules
        .iter()
        .filter(|rule| wanted.contains(rule.id.as_str()))
    {
        let owner = outcomes.len();
        let active_scan = select_active_scan(rule, sys);
        outcomes.push(RuleCleanOutcome {
            id: rule.id.clone(),
            name: active_scan
                .and_then(|scan| scan.name_override.clone())
                .unwrap_or_else(|| rule.name.clone()),
            description: Some(
                active_scan
                    .and_then(|scan| scan.description_override.clone())
                    .unwrap_or_else(|| rule.description.clone()),
            ),
            warning: active_scan
                .and_then(|scan| scan.warning_override.clone())
                .or_else(|| rule.warning.clone()),
            level: Some(rule.level),
            dry_run,
            deleted_bytes: 0,
            deleted_files: 0,
            actions: CleanupActionTotals::default(),
            skipped_locked_files: 0,
            skipped_protected_files: 0,
            failed_files: 0,
            errors: Vec::new(),
        });
        let Some(active_scan) = active_scan else {
            continue;
        };
        let collected = collect_scan_targets(active_scan, sys, usize::MAX, false, None);
        let recycle = collected.system_task_proc.as_deref() == Some("BuiltinInstallerCleanup");

        for (key, file) in collected.files.targets {
            if let Some(&index) = by_key.get(&key) {
                let target = &mut targets[index];
                target.status = target.status.max(file.status);
                if recycle && matches!(target.action, CleanupAction::Delete) {
                    target.action = CleanupAction::Recycle;
                }
                continue;
            }
            let action = match &key {
                TargetKey::Driver(name) => CleanupAction::RemoveDriver(name.clone()),
                TargetKey::File(_) if recycle => CleanupAction::Recycle,
                TargetKey::File(_) => CleanupAction::Delete,
            };
            by_key.insert(key, targets.len());
            targets.push(PlannedTarget {
                owner,
                path: file.path,
                size: file.size_bytes,
                status: file.status,
                action,
            });
        }
        if !dry_run {
            dirs.extend(
                collected
                    .removable_dirs
                    .into_iter()
                    .filter(|dir| seen_dirs.insert(TargetKey::file(dir))),
            );
        }
    }
    (outcomes, targets, dirs)
}

fn remove_file_safe(path: &Path, mut perms: std::fs::Permissions) -> std::io::Result<()> {
    if perms.readonly() {
        perms.set_readonly(false);
        let _ = std::fs::set_permissions(path, perms);
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
        system_task_proc: None,
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

fn collect_scan_targets<'a>(
    scan: &CleanupScan,
    sys: &SystemInfo,
    mut probe_budget: usize,
    preview_only: bool,
    index: Option<(&'a ScanIndex<'a>, usize)>,
) -> CollectedTargets<'a> {
    let mut files = CollectedFiles {
        preview_only,
        index,
        ..CollectedFiles::default()
    };
    let mut seen_paths = HashSet::<TargetKey>::new();
    let mut removable_dirs = Vec::new();
    let mut is_system_task = false;
    let mut system_task_proc = scan_custom_proc_name(scan);
    let mut system_task_hint: Option<LocalizedText> = None;

    for action in &scan.actions {
        if files.cancelled() {
            break;
        }
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
                    &mut probe_budget,
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
                    &mut system_task_proc,
                    &mut system_task_hint,
                    &mut probe_budget,
                );
            }
            _ => {}
        }
    }

    CollectedTargets {
        files,
        removable_dirs,
        is_system_task,
        system_task_proc,
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
    files: &mut CollectedFiles<'_>,
    seen_paths: &mut HashSet<TargetKey>,
    removable_dirs: &mut Vec<PathBuf>,
    probe_budget: &mut usize,
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

    let roots =
        resolve_wildcard_roots_until(&effective_root, &sys.fixed_drives, &|| files.cancelled());
    for root_dir in roots {
        if files.cancelled() {
            return;
        }
        let Ok(root_meta) = std::fs::symlink_metadata(&root_dir) else {
            continue;
        };
        let root_str = root_dir.to_string_lossy().into_owned();
        let expanded_exclusions: Vec<String> = excluded
            .iter()
            .filter_map(|ex| expand_expression(ex, Some(&root_str)))
            .filter(|s| !s.trim().is_empty())
            .collect();

        if effective_queries.is_empty() {
            if root_meta.is_file() {
                record_file_with_meta(
                    &root_dir,
                    Some(&root_meta),
                    files,
                    seen_paths,
                    sys.is_admin,
                    probe_budget,
                );
            } else {
                walk_dir_filtered(
                    &root_dir,
                    &root_dir,
                    &expanded_exclusions,
                    allow_dirs,
                    false,
                    sys.is_admin,
                    files,
                    seen_paths,
                    removable_dirs,
                    probe_budget,
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
                let matched_roots =
                    resolve_wildcard_roots_until(&pattern, &sys.fixed_drives, &|| {
                        files.cancelled()
                    });
                for target in matched_roots {
                    if files.cancelled() {
                        return;
                    }
                    if is_excluded(&target, &root_dir, &expanded_exclusions) {
                        continue;
                    }
                    let Ok(t_meta) = std::fs::symlink_metadata(&target) else {
                        continue;
                    };
                    if t_meta.is_file() {
                        record_file_with_meta(
                            &target,
                            Some(&t_meta),
                            files,
                            seen_paths,
                            sys.is_admin,
                            probe_budget,
                        );
                    } else if t_meta.is_dir()
                        && (t_meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) == 0
                    {
                        walk_dir_filtered(
                            &target,
                            &root_dir,
                            &expanded_exclusions,
                            allow_dirs,
                            allow_dirs,
                            sys.is_admin,
                            files,
                            seen_paths,
                            removable_dirs,
                            probe_budget,
                        );
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_custom_proc(
    proc_name: &str,
    sys: &SystemInfo,
    files: &mut CollectedFiles<'_>,
    seen_paths: &mut HashSet<TargetKey>,
    removable_dirs: &mut Vec<PathBuf>,
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
                let reparse = std::fs::symlink_metadata(&path)
                    .is_ok_and(|meta| meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0);
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
                    files.observe(&key, seen_paths, d.size_bytes, || (FileStatus::Ready, None))
                {
                    let newer = d.newer_version.as_deref().unwrap_or("newer");
                    files.record(
                        key,
                        PreviewFile {
                            path: format!(
                                "{} [{} · {} v{} -> {}]",
                                d.repo_path, d.published_name, d.original_name, d.version, newer
                            ),
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
    flags
        .split('|')
        .any(|f| f.trim().eq_ignore_ascii_case("Directory"))
}

#[allow(clippy::too_many_arguments)]
fn walk_dir_filtered(
    dir: &Path,
    anchor_root: &Path,
    exclusions: &[String],
    allow_dirs: bool,
    remove_self: bool,
    sys_is_admin: bool,
    files: &mut CollectedFiles<'_>,
    seen_paths: &mut HashSet<TargetKey>,
    removable_dirs: &mut Vec<PathBuf>,
    probe_budget: &mut usize,
) {
    let mut walker = WalkDir::new(dir).follow_links(false).into_iter();
    while !files.cancelled() {
        let Some(entry) = walker.next() else {
            break;
        };
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let is_dir = entry.file_type().is_dir();
        if path != dir
            && (entry.path_is_symlink()
                || (is_dir
                    && entry.metadata().is_ok_and(|meta| {
                        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
                    }))
                || is_excluded(path, anchor_root, exclusions))
        {
            if is_dir {
                walker.skip_current_dir();
            }
            continue;
        }
        if entry.file_type().is_file() {
            let meta = entry.metadata().ok();
            record_file_with_meta(
                path,
                meta.as_ref(),
                files,
                seen_paths,
                sys_is_admin,
                probe_budget,
            );
        } else if is_dir && allow_dirs && !files.preview_only && (path != dir || remove_self) {
            removable_dirs.push(path.to_path_buf());
        }
    }
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

fn record_file_with_meta(
    path: &Path,
    meta: Option<&std::fs::Metadata>,
    files: &mut CollectedFiles<'_>,
    seen_paths: &mut HashSet<TargetKey>,
    sys_is_admin: bool,
    probe_budget: &mut usize,
) {
    let owned;
    let meta = match meta {
        Some(meta) => meta,
        None => {
            let Ok(meta) = std::fs::symlink_metadata(path) else {
                return;
            };
            owned = meta;
            &owned
        }
    };
    let reparse = meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    record_file_with_size(
        path,
        meta.len(),
        reparse,
        files,
        seen_paths,
        sys_is_admin,
        probe_budget,
    );
}

fn record_file_with_size(
    path: &Path,
    size_bytes: u64,
    reparse: bool,
    files: &mut CollectedFiles<'_>,
    seen_paths: &mut HashSet<TargetKey>,
    sys_is_admin: bool,
    probe_budget: &mut usize,
) {
    let path_str = path.to_string_lossy().into_owned();
    let key = TargetKey::file(path);
    let Some((size_bytes, status, reason)) = files.observe(&key, seen_paths, size_bytes, || {
        if reparse {
            (
                FileStatus::Protected,
                Some("Windows reparse point".to_string()),
            )
        } else if *probe_budget > 0 || size_bytes >= 4 * 1024 * 1024 {
            if *probe_budget > 0 {
                *probe_budget -= 1;
            }
            diagnose_file(path, &path_str)
        } else {
            diagnose_file_fast(&path_str, sys_is_admin)
        }
    }) else {
        return;
    };
    files.record(
        key,
        PreviewFile {
            path: path_str,
            size_bytes,
            status,
            reason,
            locking_processes: Vec::new(),
        },
    );
}

fn diagnose_file_fast(path_str: &str, is_admin: bool) -> (FileStatus, Option<String>) {
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
    if !is_admin
        && (lower.contains(r"\windows\installer\")
            || lower.contains(r"\windows\winsxs\")
            || lower.contains(r"\windows\system32\"))
    {
        return (
            FileStatus::Protected,
            Some("Administrator privileges required for system folder".to_string()),
        );
    }
    (FileStatus::Ready, None)
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
    fn cleanup_preview_caps_details_without_losing_totals() {
        let mut files = CollectedFiles {
            preview_only: true,
            ..CollectedFiles::default()
        };
        for id in 0..250 {
            files.push_preview(PreviewFile {
                path: id.to_string(),
                size_bytes: if id == 249 { 10 } else { 1 },
                status: if id == 249 {
                    FileStatus::Locked
                } else {
                    FileStatus::Ready
                },
                reason: None,
                locking_processes: Vec::new(),
            });
        }
        files.trim_preview();

        assert_eq!(files.items.len(), MAX_PREVIEW_FILES);
        assert_eq!(files.items[0].path, "249");
        assert_eq!(files.items[1].path, "0");
        assert_eq!(files.items.last().unwrap().path, "98");
        assert_eq!(files.ready.count, 249);
        assert_eq!(files.ready.bytes, 249);
        assert_eq!(files.locked.count, 1);
        assert_eq!(files.locked.bytes, 10);
    }

    fn test_rule(id: &str, root: &Path, excluded: &[&str]) -> CleanupRule {
        CleanupRule {
            id: id.to_string(),
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
                    root_path: root.to_string_lossy().into_owned(),
                    flags: "Directory|File".to_string(),
                    id: None,
                    queries: Vec::new(),
                    excluded: excluded.iter().map(|name| name.to_string()).collect(),
                }],
                rescan: false,
                restart: None,
            }],
        }
    }

    #[test]
    fn overlapping_rules_share_one_target_even_outside_the_preview() {
        let base = std::env::temp_dir().join(format!("dismnext_overlap_{}", std::process::id()));
        let nested = base.join("nested");
        fs::create_dir_all(&nested).unwrap();
        let shared = nested.join("shared.tmp");
        let only_b = nested.join("only-b.tmp");
        fs::write(&shared, [0]).unwrap();
        fs::write(&only_b, [0; 3]).unwrap();
        for index in 0..110 {
            fs::write(base.join(format!("cache-{index}.tmp")), [0; 2]).unwrap();
        }

        let rule_a = test_rule("a", &base, &["only-b.tmp"]);
        let rule_b = test_rule("b", &nested, &[]);
        let mut rule_c = test_rule("c", &shared, &[]);
        let repeated = rule_c.scans[0].actions[0].clone();
        rule_c.scans[0].actions.push(repeated);
        let rules = [rule_a, rule_b, rule_c];
        let sys = SystemInfo::collect();

        let progress = ScanProgress::default();
        let batch = scan_all_with_progress(&rules, &sys, &progress).unwrap();
        let snapshot = serde_json::to_value(progress.snapshot()).unwrap();
        assert_eq!(snapshot["processed_files"], 112);
        assert_eq!(snapshot["phase"], "finishing");
        progress.cancel();
        assert!(scan_all_with_progress(&rules, &sys, &progress)
            .unwrap_err()
            .is::<crate::scan_progress::ScanCancelled>());
        assert_eq!(
            batch
                .items
                .iter()
                .map(|rule| rule.cleanable_bytes)
                .collect::<Vec<_>>(),
            [221, 4, 1]
        );
        assert!(batch.items[0]
            .preview_files
            .iter()
            .all(|file| !file.path.ends_with("shared.tmp")));
        assert_eq!(batch.summary.cleanable_bytes, 224);
        assert_eq!(batch.summary.overlaps.len(), 1);
        assert_eq!(batch.summary.overlaps[0].rule_ids, ["a", "b", "c"]);
        assert_eq!(batch.summary.overlaps[0].cleanable_bytes, 1);

        let selected = ["b".into(), "c".into()];
        let dry = execute_cleanup(&rules, &selected, &sys, true);
        assert_eq!(dry.total_deleted_bytes, 4);
        assert_eq!(dry.total_deleted_files, 2);
        assert_eq!(dry.outcomes[0].actions.permanent_delete.count, 2);
        assert_eq!(dry.outcomes[0].actions.permanent_delete.bytes, 4);
        assert_eq!(dry.outcomes[1].actions.permanent_delete.count, 0);
        assert!(shared.exists() && only_b.exists());

        let real = execute_cleanup(&rules, &selected, &sys, false);
        assert_eq!(real.total_deleted_bytes, dry.total_deleted_bytes);
        assert_eq!(real.total_deleted_files, dry.total_deleted_files);
        assert_eq!(real.outcomes[1].deleted_files, 0);
        assert_eq!(real.outcomes[0].actions.permanent_delete.count, 2);
        assert_eq!(real.outcomes[0].actions.recycle.count, 0);
        assert_eq!(real.outcomes[0].actions.remove_driver.count, 0);
        assert!(real
            .outcomes
            .iter()
            .all(|outcome| outcome.failed_files == 0));
        assert!(!shared.exists() && !only_b.exists());
        assert!(nested.exists());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn planned_file_is_rechecked_before_deletion() {
        use std::os::windows::fs::OpenOptionsExt;

        let path =
            std::env::temp_dir().join(format!("dismnext_recheck_{}.tmp", std::process::id()));
        fs::write(&path, b"keep").unwrap();
        let rules = [test_rule("test", &path, &[])];
        let (_, targets, _) = plan_cleanup(
            &rules,
            &HashSet::from(["test"]),
            &SystemInfo::collect(),
            false,
        );
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].status, FileStatus::Ready);

        let handle = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        let result = targets[0].apply(false).unwrap();
        drop(handle);
        assert_eq!(result.0, FileStatus::Locked);
        assert!(path.exists());
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn target_keys_normalize_windows_path_spellings() {
        let key = TargetKey::file(Path::new(r"C:\Temp\Cache\sample.tmp"));
        assert_eq!(
            key,
            TargetKey::file(Path::new(r"c:/TEMP/cache/./SAMPLE.TMP"))
        );
        assert_eq!(
            key,
            TargetKey::file(Path::new(r"C:\Temp\Cache\nested\..\sample.tmp"))
        );
        assert_eq!(
            key,
            TargetKey::file(Path::new(r"\\?\C:\Temp\Cache\sample.tmp"))
        );
        assert_ne!(
            key,
            TargetKey::file(Path::new(r"C:\Temp\Cache\another.tmp"))
        );
    }

    #[test]
    fn test_scan_exclusion_and_dry_run_vs_execute_in_sandbox() {
        let base = std::env::temp_dir().join(format!("dismnext_clean_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let sub = base.join("cache_dir");
        fs::create_dir_all(&sub).unwrap();

        let keep_file = base.join("keep.ini");
        let junk_a = base.join("junk1.tmp");
        let junk_b = sub.join("junk2.tmp");
        File::create(&keep_file)
            .unwrap()
            .write_all(b"keep")
            .unwrap();
        File::create(&junk_a)
            .unwrap()
            .write_all(b"1234567890")
            .unwrap();
        File::create(&junk_b).unwrap().write_all(b"abcdef").unwrap();

        let rule = test_rule("test-001", &base, &["keep.ini"]);

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

    #[test]
    fn test_catalog_hygiene_no_author_credits_or_empty_descriptions() {
        let catalog = crate::catalog::Catalog::load(None).expect("embedded catalog should load");
        assert_eq!(catalog.base_version, "DismNext 2.0");
        assert!(catalog.cleanup_rules.len() >= 93);

        let contains_by_credit = |s: &str| -> bool {
            let lower = s.to_ascii_lowercase();
            lower.contains("(by ") || lower.contains("（by ") || lower.contains("1002.2")
        };

        for r in &catalog.cleanup_rules {
            assert!(
                r.source == "dismnext-core" || r.source == "win11-modern" || r.source == "custom",
                "Unexpected source '{}' on rule {}",
                r.source,
                r.id
            );
            assert!(
                !r.name.zh.trim().is_empty() && !r.name.en.trim().is_empty(),
                "Rule {} has empty localized name",
                r.id
            );
            assert!(
                !r.description.zh.trim().is_empty() && !r.description.en.trim().is_empty(),
                "Rule {} has empty description",
                r.id
            );
            assert!(
                !contains_by_credit(&r.name.zh)
                    && !contains_by_credit(&r.name.en)
                    && !contains_by_credit(&r.description.zh)
                    && !contains_by_credit(&r.description.en),
                "Rule {} still contains third-party credit or legacy version string",
                r.id
            );
            for s in &r.scans {
                if let Some(ref ov) = s.name_override {
                    assert!(
                        !contains_by_credit(&ov.zh) && !contains_by_credit(&ov.en),
                        "Rule {} scan name_override contains credit",
                        r.id
                    );
                }
            }
        }

        for g in &catalog.optimization_groups {
            for it in &g.items {
                assert!(
                    it.source == "dismnext-core" || it.source == "win11-modern",
                    "Unexpected source '{}' on opt {}",
                    it.source,
                    it.id
                );
                assert!(
                    !it.name.zh.trim().is_empty() && !it.name.en.trim().is_empty(),
                    "Opt {} has empty localized name",
                    it.id
                );
                assert!(
                    !contains_by_credit(&it.name.zh) && !contains_by_credit(&it.name.en),
                    "Opt {} still contains third-party credit: {} / {}",
                    it.id,
                    it.name.zh,
                    it.name.en
                );
            }
        }
    }
}
