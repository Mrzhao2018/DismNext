export type Lang = 'zh' | 'en';

export interface LocalizedText {
  zh: string;
  en: string;
}

export interface DiskStats {
  root: string;
  total_bytes: number;
  free_bytes: number;
  used_bytes: number;
}

export interface SystemInfo {
  product_name: string;
  display_version: string;
  major: number;
  minor: number;
  build: number;
  ubr: number;
  version_string: string;
  arch: string;
  arch_code: number;
  is_admin: boolean;
  system_drive: string;
  system_disk: DiskStats;
  fixed_drives: string[];
}

export interface SysInfoPayload {
  system: SystemInfo;
  catalog: {
    base_version: string;
    cleanup_rules_total: number;
    optimization_groups_total: number;
    optimization_items_total: number;
  };
}

export type FileStatus = 'Ready' | 'Locked' | 'Protected';

export interface LockingProcessInfo {
  pid: number;
  app_name: string;
  service_name: string | null;
  app_type: string;
  is_critical: boolean;
}

export interface PreviewFile {
  path: string;
  size_bytes: number;
  status: FileStatus;
  reason: string | null;
  locking_processes?: LockingProcessInfo[];
}

export interface CleanupScanResult {
  id: string;
  name: LocalizedText;
  group: LocalizedText;
  description: LocalizedText;
  warning: LocalizedText | null;
  level: number;
  source: string;
  applicable: boolean;
  is_system_task: boolean;
  system_task_proc: string | null;
  system_task_hint: LocalizedText | null;
  cleanable_bytes: number;
  cleanable_count: number;
  skipped_locked_bytes: number;
  skipped_locked_count: number;
  skipped_protected_bytes: number;
  skipped_protected_count: number;
  preview_files: PreviewFile[];
}

export interface CleanupRuleEntry {
  scanned: boolean;
  rule: CleanupScanResult;
}

export interface CleanupScanSummary {
  cleanable_bytes: number;
  overlaps: { rule_ids: string[]; cleanable_bytes: number }[];
}

export interface CleanupRulesPayload {
  items: CleanupRuleEntry[];
  summary: CleanupScanSummary | null;
}

export interface CleanupScanPayload {
  items: CleanupScanResult[];
  summary: CleanupScanSummary;
}

export interface CleanupTargetTotals {
  bytes: number;
  count: number;
}

export interface RuleCleanOutcome {
  id: string;
  name: LocalizedText;
  description: LocalizedText | null;
  warning: LocalizedText | null;
  level: number | null;
  dry_run: boolean;
  deleted_bytes: number;
  deleted_files: number;
  actions: {
    permanent_delete: CleanupTargetTotals;
    recycle: CleanupTargetTotals;
    remove_driver: CleanupTargetTotals;
  };
  skipped_locked_files: number;
  skipped_protected_files: number;
  failed_files: number;
  errors: string[];
  review?: {
    directories: number;
    skipped_changed: number;
    skipped_missing: number;
  };
}

export interface CleanupExecutionReport {
  dry_run: boolean;
  total_deleted_bytes: number;
  total_deleted_files: number;
  total_skipped_files: number;
  outcomes: RuleCleanOutcome[];
}

export interface CleanupReview {
  plan_id: string | null;
  report: CleanupExecutionReport;
}

export type ScopeName = 'current' | 'default' | 'system';

export interface ScopeCheckBoxStatus {
  kind: 'checkbox';
  enabled: boolean;
  true_warning: LocalizedText | null;
  false_warning: LocalizedText | null;
}

export interface DropdownOptionStatus {
  index: number;
  name: LocalizedText;
  warning: LocalizedText | null;
}

export interface ScopeComboStatus {
  kind: 'combo';
  selected_index: number | null;
  options: DropdownOptionStatus[];
}

export type ScopeStatus = ScopeCheckBoxStatus | ScopeComboStatus;

export interface OptimizationItemStatus {
  id: string;
  name: LocalizedText;
  item_type: string;
  source: string;
  current: ScopeStatus | null;
  default: ScopeStatus | null;
  system: ScopeStatus | null;
}

export interface OptimizationGroupStatus {
  id: string;
  name: LocalizedText;
  items: OptimizationItemStatus[];
}

export interface SnapshotRecord {
  kind: 'ValueState' | 'KeyTreeState';
  key: string;
  value_name?: string;
  sam: number;
  previous: unknown;
}

export interface RegistrySnapshot {
  id: string;
  created_at: string;
  item_id: string;
  item_name: LocalizedText;
  scope: string;
  action_summary: string;
  restored: boolean;
  records: SnapshotRecord[];
}

export function pickText(t: LocalizedText | null | undefined, lang: Lang): string {
  if (!t) return '';
  return lang === 'en' ? t.en || t.zh : t.zh || t.en;
}

export function formatBytes(bytes: number): string {
  if (bytes <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let val = bytes;
  let idx = 0;
  while (val >= 1024 && idx < units.length - 1) {
    val /= 1024;
    idx += 1;
  }
  return `${val >= 10 || idx === 0 ? val.toFixed(idx === 0 ? 0 : 1) : val.toFixed(2)} ${units[idx]}`;
}

export class ApiError extends Error {
  constructor(message: string, readonly status: number) {
    super(message);
  }
}

async function reqJson<T>(url: string, init?: RequestInit): Promise<T> {
  const res = await fetch(url, {
    ...init,
    headers: {
      'Content-Type': 'application/json',
      'X-DismNext-Client': '1',
      ...(init?.headers ?? {}),
    },
  });
  const data = await res.json();
  if (!res.ok) {
    throw new ApiError(data?.error || `HTTP ${res.status}`, res.status);
  }
  return data as T;
}

export interface DiskTreeNode {
  name: string;
  path: string;
  is_dir: boolean;
  is_aggregate: boolean;
  is_cache_hint: boolean;
  aggregate_count: number;
  size_bytes: number;
  file_count: number;
  dir_count: number;
  ext: string | null;
  children: DiskTreeNode[];
}

export interface ExtensionStat {
  ext: string;
  description: LocalizedText;
  size_bytes: number;
  file_count: number;
  color: string;
}

export interface LargeFileEntry {
  path: string;
  name: string;
  ext: string;
  size_bytes: number;
  allocated_bytes?: number;
}

export interface DiskPreset {
  label: LocalizedText;
  path: string;
}

export interface DiskAnalysisReport {
  root_path: string;
  scan_started_at: string;
  elapsed_ms: number;
  total_bytes: number;
  allocation?: DiskAllocation;
  total_files: number;
  total_dirs: number;
  tree: DiskTreeNode;
  extensions: ExtensionStat[];
  largest_files: LargeFileEntry[];
}

export type DiskAllocation =
  | {
      status: 'measured';
      allocated_bytes: number;
      hard_link_aliases: number;
      unmeasured_files: number;
      unreadable_entries: number;
    }
  | { status: 'unsupported_file_system' | 'unavailable' };

export type StartupScope = 'user' | 'system';

export interface StartupItem {
  id: string;
  name: string;
  command: string;
  target_path: string | null;
  location: string;
  scope: StartupScope;
  enabled: boolean;
  target_exists: boolean;
}

export type StartupSignature =
  | 'valid'
  | 'unsigned'
  | 'untrusted'
  | 'hash_mismatch'
  | 'unsupported'
  | 'missing_target'
  | 'unresolved_target'
  | 'unavailable';

export interface StartupInspection {
  origin: { kind: 'registry'; key_path: string; value_name: string }
    | { kind: 'startup_folder'; path: string };
  target: {
    path: string | null;
    signature: StartupSignature;
    signer: string | null;
    company: string | null;
  };
}

export interface WimInspection {
  path: string;
  images: {
    index: number;
    name: string | null;
    description: string | null;
    edition: string | null;
    architecture: string | null;
    version: string | null;
  }[];
  mounts: { status: 'available'; entries: MountedWim[] }
    | { status: 'access_denied' }
    | { status: 'unavailable'; error: string };
}

export interface MountedWim {
  image_path: string;
  mount_path: string;
  index: number;
  state: 'mounted' | 'mounting' | 'remountable' | 'invalid' | 'unknown';
  read_write: boolean;
}

export type AppxCategory = 'bloatware' | 'user_app' | 'framework' | 'system_core';

export interface AppxPackageInfo {
  package_full_name: string;
  name: string;
  display_name: LocalizedText;
  description: LocalizedText;
  version: string;
  arch: string;
  publisher_id: string;
  install_location: string;
  category: AppxCategory;
  protected: boolean;
}

export interface DriverPackageInfo {
  published_name: string;
  original_name: string;
  provider: string;
  class_name: string;
  class_guid: string;
  version: string;
  date: string;
  signer_name: string;
  repo_folder: string;
  repo_path: string;
  size_bytes: number;
  is_outdated_duplicate: boolean;
  newer_version: string | null;
}

export interface DriverListPayload {
  drivers: DriverPackageInfo[];
  total_bytes: number;
  outdated_count: number;
  outdated_bytes: number;
  default_backup_dir: string;
  deleted?: string[];
  errors?: string[];
}

export interface DriverBackupReport {
  target_dir: string;
  exported_count: number;
  total_bytes: number;
  manifest_path: string;
}

export type FeatureCategory =
  | 'virtualization'
  | 'developer'
  | 'security_ai'
  | 'network_smb'
  | 'print_media'
  | 'iis_web';

export interface WindowsFeatureInfo {
  name: string;
  display_name: LocalizedText;
  description: LocalizedText;
  category: FeatureCategory;
  enabled: boolean;
  high_impact: boolean;
}

export interface FeatureToggleOutcome {
  name: string;
  enabled: boolean;
  restart_required: boolean;
  output: string;
}

export interface SystemTaskStatus {
  compact_os_enabled: boolean;
  compact_os_raw_output: string;
  outdated_drivers_count: number;
  outdated_drivers_bytes: number;
  orphaned_installer_count: number;
  orphaned_installer_bytes: number;
}

export interface SystemTaskExecOutcome {
  task_id: string;
  success: boolean;
  output: string;
  status: SystemTaskStatus;
}

export interface ThirdPartyServiceItem {
  name: string;
  display_name: string;
  description: string;
  image_path: string;
  target_path: string | null;
  start_mode: 'auto' | 'manual' | 'disabled' | string;
  delayed_auto: boolean;
  target_exists: boolean;
}

export interface ScheduledTaskItem {
  path: string;
  name: string;
  command: string;
  enabled: boolean;
  state: string;
}

export interface OptimizationProfileEntry {
  item_id: string;
  scope: ScopeName;
  value: boolean | number;
}

export interface OptimizationPreset {
  id: string;
  name: LocalizedText;
  description: LocalizedText;
  entries: OptimizationProfileEntry[];
}

export interface OptimizationProfile {
  name: string;
  created_at: string;
  entries: OptimizationProfileEntry[];
}

export interface BatchApplyOutcome {
  applied_count: number;
  snapshots_created: number;
  warnings: string[];
}

export interface RestorePointInfo {
  sequence_number: number;
  description: string;
  creation_time: string;
  event_type: string;
}

export interface RescueActionOutcome {
  action_id: string;
  success: boolean;
  requires_restart: boolean;
  output: string;
}

export interface DuplicateFileEntry {
  path: string;
  name: string;
  modified_date: string;
  modified_unix: number;
  stamp?: string;
}

export interface DuplicateRecycleTarget {
  path: string;
  stamp: string;
}

export type DuplicateRecycleResult = { path: string } & (
  | { status: 'recycled'; size_bytes: number }
  | { status: 'skipped'; reason: 'keeper_unavailable' | 'changed' | 'missing' | 'same_file' | 'protected' | 'different_content' }
  | { status: 'failed'; error: string }
);

export type PathRecycleSkipReason = 'changed' | 'missing' | 'protected' | 'unverifiable';

export type PathRecyclePreview =
  | { status: 'ready'; stamp: string; size_bytes: number; file_count: number; dir_count: number }
  | { status: 'blocked'; reason: PathRecycleSkipReason };

export type PathRecycleResult = { path: string } & (
  | { status: 'recycled'; size_bytes: number; file_count: number; dir_count: number }
  | { status: 'skipped'; reason: PathRecycleSkipReason }
  | { status: 'failed'; error: string }
);

export interface DuplicateGroup {
  hash_hex: string;
  size_bytes: number;
  wasted_bytes: number;
  files: DuplicateFileEntry[];
}

export interface DuplicateScanReport {
  root_path: string;
  elapsed_ms: number;
  total_groups: number;
  total_wasted_bytes: number;
  groups: DuplicateGroup[];
}

export interface ScanProgress {
  phase: 'walking' | 'sampling' | 'hashing' | 'finishing';
  processed_files: number;
  total_files: number | null;
  bytes_read: number;
}

export type ScanRequest =
  | { kind: 'disk_analysis'; path: string; max_depth: number; measure_allocation?: boolean }
  | { kind: 'duplicate_files'; path: string; min_size_bytes: number }
  | { kind: 'cleanup_rules' };

export type ScanStatus<T> =
  | { state: 'running' | 'cancelling'; progress: ScanProgress }
  | { state: 'completed'; result: T }
  | { state: 'cancelled' }
  | { state: 'failed'; error: string };

export interface OrphanDirEntry {
  name: string;
  path: string;
  scope_label: string;
  size_bytes: number;
  file_count: number;
  modified_date: string;
  scan_started_at: string;
}

export interface Win32AppInfo {
  id: string;
  key_name: string;
  display_name: string;
  publisher: string;
  display_version: string;
  install_date: string;
  install_location: string;
  uninstall_string: string;
  quiet_uninstall_string: string;
  estimated_size_bytes: number;
  scope: string;
  is_msi: boolean;
}

export interface AppLeftoverItem {
  path: string;
  kind: string;
  match_basis: 'registered_install_location' | 'application_name' | 'publisher_and_application_name';
  size_bytes: number;
  file_count: number;
  review?: { status: 'ready'; stamp: string } | { status: 'blocked'; reason: LeftoverSkipReason };
}

export type LeftoverSkipReason = 'changed' | 'missing' | 'protected' | 'unverifiable';

export type LeftoverRecycleResult = { path: string } & (
  | { status: 'recycled'; size_bytes: number }
  | { status: 'skipped'; reason: LeftoverSkipReason }
  | { status: 'failed'; error: string }
);

export interface Win32UninstallStatus {
  app_id: string;
  display_name: string;
  state: 'running' | 'awaiting_verification' | 'completed' | 'cancelled' | 'restart_required' | 'failed';
  exit_code: number | null;
}

export interface CustomRuleInput {
  id?: string;
  name_zh: string;
  name_en?: string;
  description_zh?: string;
  description_en?: string;
  level?: number;
  root_path: string;
  queries: string[];
  excluded: string[];
  flags?: string;
}

export interface ReportRuleSummary {
  id: string;
  name_zh: string;
  name_en: string;
  cleanable_bytes: number;
}

export interface HealthReportPayload {
  generated_at: string;
  health_score: number;
  score_notes: string[];
  os_summary: string;
  disk_root: string;
  disk_total_bytes: number;
  disk_free_bytes: number;
  disk_used_bytes: number;
  reclaimable_bytes: number;
  outdated_drivers_count: number;
  outdated_drivers_bytes: number;
  orphaned_installer_count: number;
  orphaned_installer_bytes: number;
  enabled_startups: number;
  auto_services: number;
  enabled_tasks: number;
  installed_win32_apps: number;
  compact_os_enabled: boolean;
  top_cleanup_rules: ReportRuleSummary[];
  markdown: string;
  html: string;
}

export const api = {
  getSysInfo: () => reqJson<SysInfoPayload>('/api/sysinfo'),
  elevate: () =>
    reqJson<{ already_admin: boolean; relaunching: boolean }>('/api/sys/elevate', {
      method: 'POST',
    }),
  getCleanupRules: () => reqJson<CleanupRulesPayload>('/api/cleanup/rules'),
  previewCleanup: (id: string) =>
    reqJson<CleanupScanResult>(`/api/cleanup/preview?id=${encodeURIComponent(id)}`),
  simulateCleanup: (rule_ids: string[]) =>
    reqJson<{ report: CleanupExecutionReport }>('/api/cleanup/execute', {
      method: 'POST',
      body: JSON.stringify({ rule_ids, dry_run: true }),
    }),
  reviewCleanup: (rule_ids: string[]) =>
    reqJson<CleanupReview>('/api/cleanup/review', {
      method: 'POST',
      body: JSON.stringify({ rule_ids }),
    }),
  confirmCleanup: (plan_id: string) =>
    reqJson<{ report: CleanupExecutionReport; system: SystemInfo }>('/api/cleanup/confirm', {
      method: 'POST',
      body: JSON.stringify({ plan_id }),
    }),
  discardCleanupReview: (plan_id: string) =>
    reqJson<{ ok: boolean }>('/api/cleanup/discard-review', {
      method: 'POST',
      body: JSON.stringify({ plan_id }),
    }),
  getOptimizerGroups: () => reqJson<{ groups: OptimizationGroupStatus[] }>('/api/optimizer/groups'),
  applyOptimization: (item_id: string, scope: ScopeName, value: boolean | number) =>
    reqJson<{
      outcome: { snapshot: RegistrySnapshot | null; restart_hint: string | null; warnings: string[] };
      groups: OptimizationGroupStatus[];
    }>('/api/optimizer/apply', {
      method: 'POST',
      body: JSON.stringify({ item_id, scope, value }),
    }),
  getSnapshots: () => reqJson<{ snapshots: RegistrySnapshot[] }>('/api/snapshots'),
  restoreSnapshot: (snapshot_id: string) =>
    reqJson<{ restored: RegistrySnapshot; groups: OptimizationGroupStatus[] }>(
      '/api/snapshots/restore',
      {
        method: 'POST',
        body: JSON.stringify({ snapshot_id }),
      }
    ),
  getDiskPresets: () => reqJson<{ presets: DiskPreset[] }>('/api/diskmap/presets'),
  startScan: (request: ScanRequest) => reqJson<{ id: number }>('/api/scans/start', {
    method: 'POST',
    body: JSON.stringify(request),
  }),
  getScanStatus: <T>(id: number) => reqJson<ScanStatus<T>>(`/api/scans/status?id=${id}`, {
    cache: 'no-store',
  }),
  cancelScan: (id: number) => reqJson<{ ok: boolean }>('/api/scans/cancel', {
    method: 'POST',
    body: JSON.stringify({ id }),
    keepalive: true,
  }),
  revealInExplorer: (path: string) =>
    reqJson<{ ok: boolean }>('/api/diskmap/reveal', {
      method: 'POST',
      body: JSON.stringify({ path }),
    }),
  reviewRecyclePath: (path: string, is_dir: boolean, scan_started_at: string) =>
    reqJson<PathRecyclePreview>('/api/diskmap/recycle/review', {
      method: 'POST',
      body: JSON.stringify({ path, is_dir, scan_started_at }),
    }),
  recyclePath: (path: string, stamp: string) =>
    reqJson<{ item: PathRecycleResult; system: SystemInfo }>('/api/diskmap/recycle', {
      method: 'POST',
      body: JSON.stringify({ path, stamp }),
    }),
  recycleDuplicates: (keeper: DuplicateRecycleTarget, candidates: DuplicateRecycleTarget[]) =>
    reqJson<{ items: DuplicateRecycleResult[]; system: SystemInfo }>('/api/diskmap/recycle-duplicates', {
      method: 'POST',
      body: JSON.stringify({ keeper, candidates }),
    }),
  getStartupItems: () => reqJson<{ items: StartupItem[] }>('/api/startup/list'),
  inspectStartupItem: (id: string) =>
    reqJson<StartupInspection>('/api/startup/inspect', {
      method: 'POST',
      body: JSON.stringify({ id }),
    }),
  toggleStartup: (id: string, enabled: boolean) =>
    reqJson<{ item: StartupItem; items: StartupItem[] }>('/api/startup/toggle', {
      method: 'POST',
      body: JSON.stringify({ id, enabled }),
    }),
  getAppxPackages: () => reqJson<{ packages: AppxPackageInfo[] }>('/api/appx/list'),
  removeAppxPackage: (package_full_name: string) =>
    reqJson<{ removed: string; packages: AppxPackageInfo[] }>('/api/appx/remove', {
      method: 'POST',
      body: JSON.stringify({ package_full_name }),
    }),
  getDrivers: () => reqJson<DriverListPayload>('/api/drivers/list'),
  exportDrivers: (target_dir: string, published_names: string[] | null) =>
    reqJson<DriverBackupReport>('/api/drivers/export', {
      method: 'POST',
      body: JSON.stringify({ target_dir, published_names }),
    }),
  deleteDrivers: (published_names: string[]) =>
    reqJson<DriverListPayload>('/api/drivers/delete', {
      method: 'POST',
      body: JSON.stringify({ published_names }),
    }),
  getFeatures: () => reqJson<{ features: WindowsFeatureInfo[] }>('/api/features/list'),
  toggleFeature: (name: string, enabled: boolean) =>
    reqJson<{ outcome: FeatureToggleOutcome; features: WindowsFeatureInfo[] }>(
      '/api/features/toggle',
      {
        method: 'POST',
        body: JSON.stringify({ name, enabled }),
      }
    ),
  getSysTaskStatus: () => reqJson<SystemTaskStatus>('/api/systasks/status'),
  runSysTask: (task_id: string) =>
    reqJson<{ outcome: SystemTaskExecOutcome; system: SystemInfo }>('/api/systasks/run', {
      method: 'POST',
      body: JSON.stringify({ task_id }),
    }),
  inspectFileLocks: (path: string) =>
    reqJson<{ processes: LockingProcessInfo[] }>('/api/lockcheck/inspect', {
      method: 'POST',
      body: JSON.stringify({ path }),
    }),
  terminateLockingProcess: (pid: number) =>
    reqJson<{ ok: boolean; message: string }>('/api/lockcheck/terminate', {
      method: 'POST',
      body: JSON.stringify({ pid }),
    }),
  getServices: () => reqJson<{ services: ThirdPartyServiceItem[] }>('/api/startup/services'),
  setServiceMode: (name: string, start_mode: string) =>
    reqJson<{ service: ThirdPartyServiceItem; services: ThirdPartyServiceItem[] }>(
      '/api/startup/services/mode',
      {
        method: 'POST',
        body: JSON.stringify({ name, start_mode }),
      }
    ),
  getScheduledTasks: () => reqJson<{ tasks: ScheduledTaskItem[] }>('/api/startup/tasks'),
  toggleScheduledTask: (path: string, enabled: boolean) =>
    reqJson<{ tasks: ScheduledTaskItem[] }>('/api/startup/tasks/toggle', {
      method: 'POST',
      body: JSON.stringify({ path, enabled }),
    }),
  getOptPresets: () => reqJson<{ presets: OptimizationPreset[] }>('/api/optimizer/presets'),
  applyOptBatch: (entries: OptimizationProfileEntry[]) =>
    reqJson<{ outcome: BatchApplyOutcome; groups: OptimizationGroupStatus[] }>(
      '/api/optimizer/batch',
      {
        method: 'POST',
        body: JSON.stringify({ entries }),
      }
    ),
  exportOptProfile: () => reqJson<OptimizationProfile>('/api/optimizer/profile/export'),
  getRestorePoints: () => reqJson<{ points: RestorePointInfo[] }>('/api/rescue/restore-points'),
  createRestorePoint: (description: string) =>
    reqJson<{ message: string; points: RestorePointInfo[] }>(
      '/api/rescue/restore-points/create',
      {
        method: 'POST',
        body: JSON.stringify({ description }),
      }
    ),
  runRescueAction: (action_id: string) =>
    reqJson<RescueActionOutcome>('/api/rescue/run', {
      method: 'POST',
      body: JSON.stringify({ action_id }),
    }),
  inspectWim: (path: string) =>
    reqJson<WimInspection>('/api/rescue/wim/inspect', {
      method: 'POST',
      body: JSON.stringify({ path }),
    }),
  getOrphanDirs: () => reqJson<{ orphans: OrphanDirEntry[] }>('/api/diskmap/orphans'),
  getWin32Apps: () => reqJson<{ apps: Win32AppInfo[] }>('/api/uninstaller/list'),
  scanAppLeftovers: (display_name: string, publisher: string, install_location: string) =>
    reqJson<{ leftovers: AppLeftoverItem[] }>('/api/uninstaller/leftovers', {
      method: 'POST',
      body: JSON.stringify({ display_name, publisher, install_location }),
    }),
  recycleAppLeftovers: (targets: { path: string; stamp: string }[]) =>
    reqJson<{ items: LeftoverRecycleResult[] }>('/api/uninstaller/leftovers/recycle', {
      method: 'POST',
      body: JSON.stringify({ targets }),
    }),
  uninstallWin32App: (app_id: string, use_quiet: boolean) =>
    reqJson<Win32UninstallStatus>('/api/uninstaller/uninstall', {
      method: 'POST',
      body: JSON.stringify({ app_id, use_quiet }),
    }),
  getWin32UninstallStatus: () =>
    reqJson<{ uninstall: Win32UninstallStatus | null }>('/api/uninstaller/uninstall/status'),
  scanWin32UninstallLeftovers: (app_id: string) =>
    reqJson<{ leftovers: AppLeftoverItem[] }>('/api/uninstaller/uninstall/leftovers', {
      method: 'POST',
      body: JSON.stringify({ app_id }),
    }),
  dismissWin32Uninstall: (app_id: string) =>
    reqJson<Record<string, never>>('/api/uninstaller/uninstall/dismiss', {
      method: 'POST',
      body: JSON.stringify({ app_id }),
    }),
  testCustomRule: (input: CustomRuleInput) =>
    reqJson<CleanupScanResult>('/api/cleanup/custom/test', {
      method: 'POST',
      body: JSON.stringify(input),
    }),
  saveCustomRule: (input: CustomRuleInput) =>
    reqJson<{ saved: CleanupScanResult; items: CleanupRuleEntry[] }>('/api/cleanup/custom/save', {
      method: 'POST',
      body: JSON.stringify(input),
    }),
  deleteCustomRule: (rule_id: string) =>
    reqJson<{ items: CleanupRuleEntry[] }>('/api/cleanup/custom/delete', {
      method: 'POST',
      body: JSON.stringify({ rule_id }),
    }),
  exportHealthReport: () => reqJson<HealthReportPayload>('/api/report/export'),
};
