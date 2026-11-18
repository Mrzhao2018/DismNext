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
    headers: { 'Content-Type': 'application/json' },
    ...init,
  });
  const data = await res.json();
  if (!res.ok) {
    throw new Error(data?.error || `HTTP ${res.status}`);
  }
  return data as T;
}

export const api = {
  getSysInfo: () => reqJson<SysInfoPayload>('/api/sysinfo'),
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
};
