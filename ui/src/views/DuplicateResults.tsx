import { useMemo } from 'react';
import { Badge, Button, Text, tokens } from '@fluentui/react-components';
import { Delete16Regular } from '@fluentui/react-icons';
import { formatBytes, type DuplicateGroup, type Lang } from '../api';
import { VirtualList } from './VirtualList';

type Row =
  | { key: string; group: DuplicateGroup; ready: boolean }
  | { key: string; file: DuplicateGroup['files'][number]; keeper: boolean };

export function DuplicateResults({ groups, lang, busy, onRecycle }: {
  groups: DuplicateGroup[];
  lang: Lang;
  busy: boolean;
  onRecycle: (group: DuplicateGroup) => void;
}) {
  const rows = useMemo(() => groups.flatMap((group): Row[] => [
    { key: group.hash_hex, group, ready: group.files.every((file) => file.stamp) },
    ...group.files.map((file, index) => ({
      key: `${group.hash_hex}:${file.path}`,
      file,
      keeper: index === 0,
    })),
  ]), [groups]);
  const zh = lang === 'zh';
  return <VirtualList rows={rows} getKey={(row) => row.key} label={zh ? '重复文件' : 'Duplicate files'}
    estimate={30} gap={2} renderRow={(row) => 'group' in row ? (
      <div style={{
        display: 'flex', alignItems: 'center', justifyContent: 'space-between',
        gap: 8, padding: 8, backgroundColor: tokens.colorNeutralBackground1,
        borderBlockStart: `1px solid ${tokens.colorNeutralStroke2}`,
      }}>
        <Text size={200} weight="semibold" style={{ overflowWrap: 'anywhere' }}>
          <Badge size="small" appearance="tint" color="warning">{row.group.files.length}x</Badge>
          {' '}{row.group.files[0]?.name} · {formatBytes(row.group.size_bytes)} (
          {zh ? '候选副本' : 'candidate copies'} {formatBytes(row.group.wasted_bytes)})
        </Text>
        <Button size="small" appearance="primary" icon={<Delete16Regular />}
          disabled={busy || !row.ready} onClick={() => onRecycle(row.group)}>
          {zh ? '保留最新，清理副本' : 'Keep Newest'}
        </Button>
      </div>
    ) : (
      <div style={{
        display: 'flex', justifyContent: 'space-between', alignItems: 'center',
        gap: 8, padding: '4px 8px', fontSize: 11, color: tokens.colorNeutralForeground2,
      }}>
        <span title={row.file.path} style={{
          overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', fontFamily: 'Consolas, monospace',
        }}>{row.keeper ? '★ ' : '• '}{row.file.path}</span>
        <span style={{ flexShrink: 0 }}>{row.file.modified_date}</span>
      </div>
    )} />;
}
