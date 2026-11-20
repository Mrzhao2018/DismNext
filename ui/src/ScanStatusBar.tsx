import { Button, Spinner, Text } from '@fluentui/react-components';
import { formatBytes, Lang } from './api';
import { useSyncExternalStore } from 'react';
import type { ActiveScan, ScanStatusStore } from './useScanJob';

const phases = {
  walking: { zh: '遍历文件', en: 'Walking files' },
  sampling: { zh: '首尾采样', en: 'Sampling files' },
  hashing: { zh: '全文哈希', en: 'Hashing files' },
  finishing: { zh: '整理结果', en: 'Preparing results' },
  verifying: { zh: '核对回收目标', en: 'Verifying recycle target' },
  recycling: { zh: '正在移入回收站', en: 'Moving to Recycle Bin' },
};

function isRecycleJob(active: ActiveScan) {
  return active.kind === 'recycle_review' || active.kind === 'recycle_path';
}

function describeProgress(active: ActiveScan, lang: Lang) {
  if (active.cancelling) return lang === 'zh' ? '正在取消…' : 'Cancelling…';
  const progress = active.progress;
  if (!progress) return isRecycleJob(active)
    ? (lang === 'zh' ? '正在启动核对…' : 'Starting verification…')
    : (lang === 'zh' ? '正在启动扫描…' : 'Starting scan…');
  const phase = phases[progress.phase][lang];
  if (progress.phase === 'finishing' || progress.phase === 'recycling') return phase;
  const count = progress.processed_files.toLocaleString();
  const fraction = progress.total_files === null ? count : `${count} / ${progress.total_files.toLocaleString()}`;
  const targets = active.kind === 'cleanup_rules';
  const label = lang === 'zh' ? (targets ? '个目标' : '个文件') : (targets ? 'targets' : 'files');
  const read = active.kind === 'duplicate_files' && progress.phase !== 'walking'
    ? ` · ${lang === 'zh' ? '已读取' : 'Read'} ${formatBytes(progress.bytes_read)}`
    : '';
  return `${phase} · ${fraction} ${label}${read}`;
}

interface Props {
  store: ScanStatusStore;
  cancelled: boolean;
  cancel: () => void;
  lang: Lang;
}

export function ScanStatusBar({ store, cancelled, cancel, lang }: Props) {
  const active = useSyncExternalStore(store.subscribe, store.getSnapshot);
  if (!active) {
    return cancelled ? <Text role="status" size={200}>{lang === 'zh' ? '操作已取消。' : 'Operation cancelled.'}</Text> : null;
  }
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
      <Spinner size="tiny" />
      <Text role="status" size={200}>{describeProgress(active, lang)}</Text>
      <Button size="small" disabled={active.cancelling || active.progress?.can_cancel === false} onClick={cancel}>
        {isRecycleJob(active) ? (lang === 'zh' ? '取消核对' : 'Cancel verification') : (lang === 'zh' ? '取消扫描' : 'Cancel scan')}
      </Button>
    </div>
  );
}
