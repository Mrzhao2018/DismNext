import type { CleanupScanResult, CleanupScanSummary } from './api';

export function calculateSelectedBytes(
  entries: readonly { rule: Pick<CleanupScanResult, 'id' | 'cleanable_bytes'> }[],
  summary: CleanupScanSummary | null
): number | null {
  if (entries.length === 0) return 0;
  if (!summary) return null;
  const selected = new Set(entries.map(({ rule }) => rule.id));
  let bytes = entries.reduce((total, { rule }) => total + rule.cleanable_bytes, 0);
  for (const overlap of summary.overlaps) {
    const count = overlap.rule_ids.filter((id) => selected.has(id)).length;
    if (count > 1) bytes -= (count - 1) * overlap.cleanable_bytes;
  }
  return bytes;
}
