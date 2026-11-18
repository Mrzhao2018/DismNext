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

export interface PreviewFile {
  path: string;
  size_bytes: number;
  status: FileStatus;
  reason: string | null;
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

export interface RuleCleanOutcome {
  id: string;
  name: LocalizedText;
  dry_run: boolean;
  deleted_bytes: number;
  deleted_files: number;
  skipped_locked_files: number;
  skipped_protected_files: number;
  failed_files: number;
  errors: string[];
}

export interface CleanupExecutionReport {
  dry_run: boolean;
  total_deleted_bytes: number;
  total_deleted_files: number;
  total_skipped_files: number;
  outcomes: RuleCleanOutcome[];
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
    throw new Error(data?.error || `HTTP ${res.status}`);
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
}

export interface DiskPreset {
  label: LocalizedText;
  path: string;
}

export interface DiskAnalysisReport {
  root_path: string;
  elapsed_ms: number;
  total_bytes: number;
  total_files: number;
  total_dirs: number;
  tree: DiskTreeNode;
  extensions: ExtensionStat[];
  largest_files: LargeFileEntry[];
}

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

export const api = {
  getSysInfo: () => reqJson<SysInfoPayload>('/api/sysinfo'),
  elevate: () =>
    reqJson<{ already_admin: boolean; relaunching: boolean }>('/api/sys/elevate', {
      method: 'POST',
    }),
  getCleanupRules: () => reqJson<{ items: CleanupRuleEntry[] }>('/api/cleanup/rules'),
  scanCleanup: () => reqJson<{ items: CleanupScanResult[] }>('/api/cleanup/scan', { method: 'POST' }),
  previewCleanup: (id: string) =>
    reqJson<CleanupScanResult>(`/api/cleanup/preview?id=${encodeURIComponent(id)}`),
  executeCleanup: (rule_ids: string[], dry_run: boolean) =>
    reqJson<{ report: CleanupExecutionReport; system: SystemInfo }>('/api/cleanup/execute', {
      method: 'POST',
      body: JSON.stringify({ rule_ids, dry_run }),
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
  analyzeDisk: (path: string, max_depth = 4) =>
    reqJson<DiskAnalysisReport>('/api/diskmap/analyze', {
      method: 'POST',
      body: JSON.stringify({ path, max_depth }),
    }),
  revealInExplorer: (path: string) =>
    reqJson<{ ok: boolean }>('/api/diskmap/reveal', {
      method: 'POST',
      body: JSON.stringify({ path }),
    }),
  recyclePath: (path: string) =>
    reqJson<{ freed_bytes: number; system: SystemInfo }>('/api/diskmap/recycle', {
      method: 'POST',
      body: JSON.stringify({ path }),
    }),
  getStartupItems: () => reqJson<{ items: StartupItem[] }>('/api/startup/list'),
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
};
