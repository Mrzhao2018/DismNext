import React, { useState } from 'react';
import {
  Badge,
  Button,
  MessageBar,
  MessageBarBody,
  Text,
  makeStyles,
  tokens,
} from '@fluentui/react-components';
import { ArrowUndo24Regular, History24Regular } from '@fluentui/react-icons';
import { Lang, OptimizationGroupStatus, RegistrySnapshot, api, pickText } from '../api';

interface Props {
  lang: Lang;
  snapshots: RegistrySnapshot[];
  onRefresh: () => Promise<void>;
  onGroupsUpdated: (groups: OptimizationGroupStatus[]) => void;
}

const useStyles = makeStyles({
  root: {
    display: 'flex',
    flexDirection: 'column',
    height: '100%',
    gap: '12px',
    overflow: 'hidden',
  },
  header: {
    display: 'flex',
    justifyContent: 'space-between',
    alignItems: 'center',
    padding: '12px 16px',
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
  },
  list: {
    flex: 1,
    overflowY: 'auto',
    display: 'flex',
    flexDirection: 'column',
    gap: '10px',
  },
  card: {
    display: 'flex',
    flexDirection: 'column',
    gap: '8px',
    padding: '14px 16px',
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
  },
  recItem: {
    fontFamily: 'Consolas, monospace',
    fontSize: '12px',
    padding: '4px 8px',
    borderRadius: tokens.borderRadiusSmall,
    backgroundColor: tokens.colorNeutralBackground1,
    color: tokens.colorNeutralForeground2,
  },
});

export const SnapshotView: React.FC<Props> = ({
  lang,
  snapshots,
  onRefresh,
  onGroupsUpdated,
}) => {
  const styles = useStyles();
  const [busyId, setBusyId] = useState<string | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const handleRestore = async (id: string) => {
    setBusyId(id);
    setMsg(null);
    setErr(null);
    try {
      const res = await api.restoreSnapshot(id);
      onGroupsUpdated(res.groups);
      await onRefresh();
      setMsg(
        lang === 'zh'
          ? `已成功回滚快照 ${id}（${pickText(res.restored.item_name, lang)}）`
          : `Successfully restored snapshot ${id}`
      );
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusyId(null);
    }
  };

  return (
    <div className={styles.root}>
      {msg && (
        <MessageBar intent="success">
          <MessageBarBody>{msg}</MessageBarBody>
        </MessageBar>
      )}
      {err && (
        <MessageBar intent="error">
          <MessageBarBody>{err}</MessageBarBody>
        </MessageBar>
      )}

      <div className={styles.header}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <History24Regular />
          <div>
            <Text size={400} weight="bold" block>
              {lang === 'zh' ? '注册表快照与一键回滚中心' : 'Registry Snapshot & Rollback Center'}
            </Text>
            <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
              {lang === 'zh'
                ? '每次修改系统优化项前均自动记录原始键值，随时可无损恢复'
                : 'Original registry states are saved automatically before every optimization change.'}
            </Text>
          </div>
        </div>
        <Button onClick={() => onRefresh()}>
          {lang === 'zh' ? '刷新快照列表' : 'Refresh'}
        </Button>
      </div>

      <div className={styles.list}>
        {snapshots.length === 0 ? (
          <div className={styles.card}>
            <Text style={{ color: tokens.colorNeutralForeground3 }}>
              {lang === 'zh'
                ? '尚无修改记录。在“系统优化”中切换任意开关后，此处会自动生成回滚快照。'
                : 'No snapshots recorded yet. Modify any optimization setting to create one.'}
            </Text>
          </div>
        ) : (
          snapshots.map((s) => (
            <div key={s.id} className={styles.card}>
              <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
                  <Text size={400} weight="bold">
                    {pickText(s.item_name, lang)}
                  </Text>
                  <Badge appearance="tint" color="informative">
                    {s.scope}
                  </Badge>
                  <Badge appearance="outline">{s.action_summary}</Badge>
                  {s.restored && (
                    <Badge appearance="filled" color="success">
                      {lang === 'zh' ? '已回滚' : 'Restored'}
                    </Badge>
                  )}
                </div>
                <Button
                  appearance="primary"
                  icon={<ArrowUndo24Regular />}
                  disabled={busyId === s.id}
                  onClick={() => handleRestore(s.id)}
                >
                  {busyId === s.id
                    ? lang === 'zh'
                      ? '回滚中...'
                      : 'Restoring...'
                    : lang === 'zh'
                    ? '一键回滚'
                    : 'Rollback'}
                </Button>
              </div>
              <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                {s.id} · {s.created_at} · {s.records.length}{' '}
                {lang === 'zh' ? '条键值记录' : 'registry records'}
              </Text>
              {s.records.slice(0, 6).map((rec, idx) => (
                <div key={idx} className={styles.recItem}>
                  [{rec.kind}] {rec.key}
                  {rec.value_name !== undefined ? ` -> "${rec.value_name || '(Default)'}"` : ''}
                </div>
              ))}
            </div>
          ))
        )}
      </div>
    </div>
  );
};
