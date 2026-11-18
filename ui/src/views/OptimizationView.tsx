import React, { useEffect, useMemo, useRef, useState } from 'react';
import {
  Badge,
  Button,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Dropdown,
  Input,
  MessageBar,
  MessageBarBody,
  Option,
  Spinner,
  Switch,
  Tab,
  TabList,
  Text,
  Tooltip,
  makeStyles,
  tokens,
} from '@fluentui/react-components';
import { ArrowDownloadRegular, Search20Regular, Warning16Regular } from '@fluentui/react-icons';
import {
  Lang,
  OptimizationGroupStatus,
  OptimizationPreset,
  OptimizationProfile,
  OptimizationProfileEntry,
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
  const deferredSearch = React.useDeferredValue(search);
  const [onlyEnabled, setOnlyEnabled] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [presets, setPresets] = useState<OptimizationPreset[]>([]);
  const [pendingBatch, setPendingBatch] = useState<{
    title: string;
    description: string;
    entries: OptimizationProfileEntry[];
  } | null>(null);
  const [batchApplying, setBatchApplying] = useState(false);
  const [notice, setNotice] = useState<{ snap?: RegistrySnapshot; text: string } | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    api
      .getOptPresets()
      .then((res) => setPresets(res.presets))
      .catch(() => {});
  }, []);

  const visibleItems = useMemo(() => {
    const q = deferredSearch.trim().toLowerCase();
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
  }, [groups, selectedGroupId, scope, deferredSearch, onlyEnabled, lang]);

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

  const itemLookup = useMemo(() => {
    const map = new Map<string, OptimizationGroupStatus['items'][number]>();
    for (const g of groups) {
      for (const it of g.items) {
        map.set(it.id, it);
      }
    }
    return map;
  }, [groups]);

  const diffPreviewRows = useMemo(() => {
    if (!pendingBatch) return [];
    return pendingBatch.entries
      .map((entry) => {
        const it = itemLookup.get(entry.item_id);
        if (!it) return null;
        const sc = it[entry.scope];
        if (!sc) return null;
        let currentLabel = '—';
        let targetLabel = String(entry.value);
        let changed = true;

        if (sc.kind === 'checkbox' && typeof entry.value === 'boolean') {
          currentLabel = sc.enabled
            ? lang === 'zh'
              ? '已开启'
              : 'ON'
            : lang === 'zh'
            ? '未启用'
            : 'OFF';
          targetLabel = entry.value
            ? lang === 'zh'
              ? '开启'
              : 'ON'
            : lang === 'zh'
            ? '关闭'
            : 'OFF';
          changed = sc.enabled !== entry.value;
        } else if (sc.kind === 'combo' && typeof entry.value === 'number') {
          const curOpt =
            sc.selected_index !== null ? sc.options[sc.selected_index] : undefined;
          const nxtOpt = sc.options[entry.value];
          currentLabel = curOpt ? pickText(curOpt.name, lang) : 'Custom';
          targetLabel = nxtOpt ? pickText(nxtOpt.name, lang) : `#${entry.value}`;
          changed = sc.selected_index !== entry.value;
        }

        return {
          entry,
          name: pickText(it.name, lang),
          currentLabel,
          targetLabel,
          changed,
        };
      })
      .filter((r): r is NonNullable<typeof r> => r !== null);
  }, [pendingBatch, itemLookup, lang]);

  const handleConfirmBatch = async () => {
    if (!pendingBatch) return;
    setBatchApplying(true);
    setErrorMsg(null);
    try {
      const changedEntries = diffPreviewRows.filter((r) => r.changed).map((r) => r.entry);
      const toApply = changedEntries.length > 0 ? changedEntries : pendingBatch.entries;
      const res = await api.applyOptBatch(toApply);
      onGroupsUpdated(res.groups);
      setNotice({
        text:
          lang === 'zh'
            ? `批量方案已执行：成功应用 ${res.outcome.applied_count} 项设置，自动创建 ${res.outcome.snapshots_created} 份注册表快照。`
            : `Batch applied: ${res.outcome.applied_count} items, ${res.outcome.snapshots_created} snapshots created.`,
      });
      if (res.outcome.warnings.length > 0) {
        setErrorMsg(res.outcome.warnings.join('; '));
      }
      setPendingBatch(null);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setBatchApplying(false);
    }
  };

  const handleExportProfile = async () => {
    setErrorMsg(null);
    try {
      const profile = await api.exportOptProfile();
      const blob = new Blob([JSON.stringify(profile, null, 2)], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `dismnext_profile_${new Date().toISOString().slice(0, 10)}.json`;
      a.click();
      URL.revokeObjectURL(url);
      setNotice({
        text:
          lang === 'zh'
            ? `已导出当前 ${profile.entries.length} 项启用配置到 JSON 配方文件。`
            : `Exported ${profile.entries.length} active optimization settings.`,
      });
    } catch (e) {
      setErrorMsg(String(e));
    }
  };

  const handleImportProfileFile = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = '';
    if (!file) return;
    try {
      const text = await file.text();
      const parsed = JSON.parse(text) as OptimizationProfile;
      if (!Array.isArray(parsed.entries)) {
        throw new Error(lang === 'zh' ? '无效的配方 JSON 格式' : 'Invalid profile JSON');
      }
      setPendingBatch({
        title: lang === 'zh' ? `导入配方：${file.name}` : `Import Profile: ${file.name}`,
        description:
          lang === 'zh'
            ? `包含 ${parsed.entries.length} 项配置，应用前将为每项自动生成可回滚快照。`
            : `Contains ${parsed.entries.length} entries; snapshots will be created before applying.`,
        entries: parsed.entries,
      });
    } catch (err) {
      setErrorMsg(String(err));
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
            placeholder={lang === 'zh' ? '搜索 175 项系统优化...' : 'Search optimizations...'}
            value={search}
            onChange={(_, d) => setSearch(d.value)}
            style={{ width: 240 }}
          />
        </div>
      </div>

      {/* Presets & Profile Bar */}
      <div
        className={styles.toolbar}
        style={{ padding: '8px 14px', justifyContent: 'space-between', flexShrink: 0 }}
      >
        <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
          <Text size={200} weight="semibold" style={{ color: tokens.colorNeutralForeground2 }}>
            {lang === 'zh' ? '场景一键预设：' : 'Presets:'}
          </Text>
          {presets.map((p) => (
            <Button
              key={p.id}
              size="small"
              appearance={p.id === 'windows_default' ? 'subtle' : 'outline'}
              onClick={() =>
                setPendingBatch({
                  title: pickText(p.name, lang),
                  description: pickText(p.description, lang),
                  entries: p.entries,
                })
              }
            >
              {pickText(p.name, lang)}
            </Button>
          ))}
        </div>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <Button size="small" icon={<ArrowDownloadRegular />} onClick={handleExportProfile}>
            {lang === 'zh' ? '导出配方 (.json)' : 'Export Profile'}
          </Button>
          <Button
            size="small"
            appearance="outline"
            onClick={() => fileInputRef.current?.click()}
          >
            {lang === 'zh' ? '导入配方' : 'Import Profile'}
          </Button>
          <input
            ref={fileInputRef}
            type="file"
            accept=".json,application/json"
            style={{ display: 'none' }}
            onChange={handleImportProfileFile}
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

      <Dialog
        open={Boolean(pendingBatch)}
        onOpenChange={(_, d) => {
          if (!d.open && !batchApplying) setPendingBatch(null);
        }}
      >
        <DialogSurface style={{ maxWidth: 640 }}>
          <DialogBody>
            <DialogTitle>{pendingBatch?.title}</DialogTitle>
            <DialogContent>
              <Text block style={{ marginBottom: 10 }}>
                {pendingBatch?.description}
              </Text>
              <div
                style={{
                  maxHeight: 280,
                  overflowY: 'auto',
                  display: 'flex',
                  flexDirection: 'column',
                  gap: 6,
                  padding: 8,
                  borderRadius: tokens.borderRadiusMedium,
                  backgroundColor: tokens.colorNeutralBackground3,
                }}
              >
                {diffPreviewRows.map((row) => (
                  <div
                    key={`${row.entry.item_id}-${row.entry.scope}`}
                    style={{
                      display: 'flex',
                      justifyContent: 'space-between',
                      alignItems: 'center',
                      gap: 10,
                      fontSize: 12,
                    }}
                  >
                    <Text size={200} style={{ flex: 1 }}>
                      {row.name}
                    </Text>
                    <Badge size="small" appearance="tint" color={row.changed ? 'brand' : 'subtle'}>
                      {row.currentLabel} → {row.targetLabel}
                    </Badge>
                  </div>
                ))}
              </div>
              <Text size={100} block style={{ marginTop: 8, color: tokens.colorNeutralForeground3 }}>
                {lang === 'zh'
                  ? `共 ${diffPreviewRows.filter((r) => r.changed).length} 项待变更，应用前将自动生成注册表回滚快照。`
                  : `${diffPreviewRows.filter((r) => r.changed).length} item(s) will change with automatic rollback snapshots.`}
              </Text>
            </DialogContent>
            <DialogActions>
              <Button
                appearance="secondary"
                disabled={batchApplying}
                onClick={() => setPendingBatch(null)}
              >
                {lang === 'zh' ? '取消' : 'Cancel'}
              </Button>
              <Button appearance="primary" disabled={batchApplying} onClick={handleConfirmBatch}>
                {batchApplying ? (
                  <Spinner size="tiny" />
                ) : lang === 'zh' ? (
                  '确认并批量应用'
                ) : (
                  'Apply Batch'
                )}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </div>
  );
};
