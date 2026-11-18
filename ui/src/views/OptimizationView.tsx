import React, { useMemo, useState } from 'react';
import {
  Badge,
  Button,
  Dropdown,
  Input,
  MessageBar,
  MessageBarBody,
  Option,
  Switch,
  Tab,
  TabList,
  Text,
  Tooltip,
  makeStyles,
  tokens,
} from '@fluentui/react-components';
import { Search20Regular, Warning16Regular } from '@fluentui/react-icons';
import {
  Lang,
  OptimizationGroupStatus,
  RegistrySnapshot,
  ScopeName,
  ScopeStatus,
  api,
  pickText,
} from '../api';

interface Props {
  lang: Lang;
  groups: OptimizationGroupStatus[];
  onGroupsUpdated: (groups: OptimizationGroupStatus[]) => void;
  onSnapshotCreated: (snap: RegistrySnapshot) => void;
  onJumpToSnapshots: () => void;
}

const useStyles = makeStyles({
  root: {
    display: 'flex',
    flexDirection: 'column',
    height: '100%',
    gap: '12px',
    overflow: 'hidden',
  },
  toolbar: {
    display: 'flex',
    flexWrap: 'wrap',
    alignItems: 'center',
    justifyContent: 'space-between',
    gap: '10px',
    padding: '10px 16px',
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
  },
  split: {
    display: 'grid',
    gridTemplateColumns: '240px 1fr',
    gap: '12px',
    flex: 1,
    minHeight: 0,
  },
  groupSidebar: {
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    padding: '8px',
    overflowY: 'auto',
    display: 'flex',
    flexDirection: 'column',
    gap: '4px',
  },
  groupBtn: {
    justifyContent: 'space-between',
    textAlign: 'left',
  },
  itemsCard: {
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    padding: '10px',
    overflowY: 'auto',
    display: 'flex',
    flexDirection: 'column',
    gap: '8px',
  },
  itemRow: {
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'space-between',
    gap: '12px',
    padding: '10px 14px',
    borderRadius: tokens.borderRadiusMedium,
    backgroundColor: tokens.colorNeutralBackground1,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
  },
});

export const OptimizationView: React.FC<Props> = ({
  lang,
  groups,
  onGroupsUpdated,
  onSnapshotCreated,
  onJumpToSnapshots,
}) => {
  const styles = useStyles();
  const [selectedGroupId, setSelectedGroupId] = useState<string>('ALL');
  const [scope, setScope] = useState<ScopeName>('current');
  const [search, setSearch] = useState('');
  const [onlyEnabled, setOnlyEnabled] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ snap?: RegistrySnapshot; text: string } | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const visibleItems = useMemo(() => {
    const q = search.trim().toLowerCase();
    const list: { groupName: string; item: OptimizationGroupStatus['items'][number]; status: ScopeStatus }[] = [];
    for (const g of groups) {
      if (!q && selectedGroupId !== 'ALL' && g.id !== selectedGroupId) continue;
      for (const it of g.items) {
        const sc = it[scope];
        if (!sc) continue;
        if (onlyEnabled && sc.kind === 'checkbox' && !sc.enabled) continue;
        if (q) {
          const hit =
            it.name.zh.toLowerCase().includes(q) ||
            it.name.en.toLowerCase().includes(q) ||
            g.name.zh.toLowerCase().includes(q) ||
            g.name.en.toLowerCase().includes(q);
          if (!hit) continue;
        }
        list.push({ groupName: pickText(g.name, lang), item: it, status: sc });
      }
    }
    return list;
  }, [groups, selectedGroupId, scope, search, onlyEnabled, lang]);

  const handleApply = async (itemId: string, val: boolean | number) => {
    setBusyId(itemId);
    setErrorMsg(null);
    try {
      const res = await api.applyOptimization(itemId, scope, val);
      onGroupsUpdated(res.groups);
      if (res.outcome.snapshot) {
        onSnapshotCreated(res.outcome.snapshot);
        setNotice({
          snap: res.outcome.snapshot,
          text:
            lang === 'zh'
              ? `已应用修改并自动创建注册表回滚快照 (${res.outcome.snapshot.id})`
              : `Applied change and created rollback snapshot (${res.outcome.snapshot.id})`,
        });
      }
      if (res.outcome.warnings.length > 0) {
        setErrorMsg(res.outcome.warnings.join('; '));
      }
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setBusyId(null);
    }
  };

  return (
    <div className={styles.root}>
      {errorMsg && (
        <MessageBar intent="warning">
          <MessageBarBody>{errorMsg}</MessageBarBody>
        </MessageBar>
      )}
      {notice && (
        <MessageBar intent="success">
          <MessageBarBody>
            {notice.text}{' '}
            {notice.snap && (
              <Button appearance="transparent" size="small" onClick={onJumpToSnapshots}>
                {lang === 'zh' ? '去回滚中心查看' : 'View Snapshots'}
              </Button>
            )}
          </MessageBarBody>
        </MessageBar>
      )}

      <div className={styles.toolbar}>
        <TabList
          selectedValue={scope}
          onTabSelect={(_, d) => setScope(d.value as ScopeName)}
          size="small"
        >
          <Tab value="current">
            {lang === 'zh' ? '当前用户 (HKCU)' : 'Current User (HKCU)'}
          </Tab>
          <Tab value="default">
            {lang === 'zh' ? '新用户模板 (HKU\\.DEFAULT)' : 'Default Profile'}
          </Tab>
          <Tab value="system">
            {lang === 'zh' ? '系统全局 (HKLM)' : 'System (HKLM)'}
          </Tab>
        </TabList>

        <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
          <Switch
            checked={onlyEnabled}
            onChange={(_, d) => setOnlyEnabled(d.checked)}
            label={lang === 'zh' ? '仅显示已启用' : 'Enabled only'}
          />
          <Input
            contentBefore={<Search20Regular />}
            placeholder={lang === 'zh' ? '搜索 167 项系统优化...' : 'Search optimizations...'}
            value={search}
            onChange={(_, d) => setSearch(d.value)}
            style={{ width: 240 }}
          />
        </div>
      </div>

      <div className={styles.split}>
        <div className={styles.groupSidebar}>
          <Button
            className={styles.groupBtn}
            appearance={selectedGroupId === 'ALL' ? 'primary' : 'subtle'}
            onClick={() => setSelectedGroupId('ALL')}
          >
            <span>{lang === 'zh' ? '全部分组' : 'All Groups'}</span>
            <Badge size="small" appearance="tint">
              {groups.reduce((acc, g) => acc + g.items.filter((i) => i[scope]).length, 0)}
            </Badge>
          </Button>
          {groups.map((g) => {
            const count = g.items.filter((i) => i[scope]).length;
            if (count === 0) return null;
            return (
              <Button
                key={g.id}
                className={styles.groupBtn}
                appearance={selectedGroupId === g.id ? 'primary' : 'subtle'}
                onClick={() => setSelectedGroupId(g.id)}
              >
                <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                  {pickText(g.name, lang)}
                </span>
                <Badge size="small" appearance="tint">
                  {count}
                </Badge>
              </Button>
            );
          })}
        </div>

        <div className={styles.itemsCard}>
          {visibleItems.map(({ groupName, item, status }) => (
            <div key={item.id} className={styles.itemRow}>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 2, minWidth: 0, flex: 1 }}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 6, flexWrap: 'wrap' }}>
                  <Text weight="semibold">{pickText(item.name, lang)}</Text>
                  {item.source === 'win11-modern' && (
                    <Badge size="small" appearance="outline" color="brand">
                      Win11
                    </Badge>
                  )}
                  {status.kind === 'checkbox' && (status.true_warning || status.false_warning) && (
                    <Tooltip
                      content={pickText(status.true_warning || status.false_warning, lang)}
                      relationship="label"
                    >
                      <Warning16Regular style={{ color: tokens.colorPaletteDarkOrangeForeground1 }} />
                    </Tooltip>
                  )}
                </div>
                <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                  {groupName} · {item.id}
                </Text>
              </div>

              <div style={{ flexShrink: 0 }}>
                {status.kind === 'checkbox' ? (
                  <Switch
                    checked={status.enabled}
                    disabled={busyId === item.id}
                    onChange={(_, d) => handleApply(item.id, d.checked)}
                  />
                ) : (
                  <Dropdown
                    value={
                      status.selected_index !== null
                        ? pickText(status.options[status.selected_index]?.name, lang)
                        : lang === 'zh'
                        ? '自定义 / 未知'
                        : 'Custom / Unknown'
                    }
                    selectedOptions={
                      status.selected_index !== null ? [String(status.selected_index)] : []
                    }
                    disabled={busyId === item.id}
                    onOptionSelect={(_, d) => {
                      if (d.optionValue !== undefined) {
                        handleApply(item.id, Number(d.optionValue));
                      }
                    }}
                    style={{ minWidth: 200 }}
                  >
                    {status.options.map((opt) => (
                      <Option key={opt.index} value={String(opt.index)}>
                        {pickText(opt.name, lang)}
                      </Option>
                    ))}
                  </Dropdown>
                )}
              </div>
            </div>
          ))}
          {visibleItems.length === 0 && (
            <Text style={{ padding: 20, color: tokens.colorNeutralForeground3 }}>
              {lang === 'zh' ? '当前筛选下无优化项。' : 'No matching optimization items.'}
            </Text>
          )}
        </div>
      </div>
    </div>
  );
};
