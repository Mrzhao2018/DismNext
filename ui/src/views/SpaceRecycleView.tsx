import React, { useMemo, useState } from 'react';
import {
  Badge,
  Button,
  Checkbox,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Input,
  MessageBar,
  MessageBarBody,
  MessageBarTitle,
  Spinner,
  Text,
  Tooltip,
  makeStyles,
  tokens,
} from '@fluentui/react-components';
import {
  Broom24Regular,
  CheckmarkCircle16Regular,
  Delete24Regular,
  Info16Regular,
  LockClosed16Regular,
  PlayRegular,
  Search20Regular,
  ShieldLock16Regular,
  Warning16Regular,
} from '@fluentui/react-icons';
import {
  CleanupExecutionReport,
  CleanupRuleEntry,
  CleanupScanResult,
  Lang,
  SystemInfo,
  api,
  formatBytes,
  pickText,
} from '../api';

interface Props {
  lang: Lang;
  entries: CleanupRuleEntry[];
  scanning: boolean;
  onScan: () => Promise<void>;
  onExecuted: (report: CleanupExecutionReport) => void;
  onSystemTaskDone?: (sys: SystemInfo) => Promise<void> | void;
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
    padding: '12px 16px',
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
  },
  toolbarLeft: {
    display: 'flex',
    alignItems: 'center',
    gap: '8px',
    flexWrap: 'wrap',
  },
  toolbarRight: {
    display: 'flex',
    alignItems: 'center',
    gap: '8px',
    flexWrap: 'wrap',
  },
  pills: {
    display: 'flex',
    alignItems: 'center',
    gap: '6px',
    flexWrap: 'wrap',
  },
  split: {
    display: 'grid',
    gridTemplateColumns: '1fr 420px',
    gap: '12px',
    flex: 1,
    minHeight: 0,
  },
  listCard: {
    display: 'flex',
    flexDirection: 'column',
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    overflow: 'hidden',
    minHeight: 0,
  },
  listScroll: {
    flex: 1,
    overflowY: 'auto',
    padding: '8px',
    display: 'flex',
    flexDirection: 'column',
    gap: '4px',
  },
  row: {
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'space-between',
    gap: '10px',
    padding: '8px 12px',
    borderRadius: tokens.borderRadiusMedium,
    cursor: 'pointer',
    border: '1px solid transparent',
    ':hover': {
      backgroundColor: tokens.colorNeutralBackground1Hover,
    },
  },
  rowActive: {
    backgroundColor: tokens.colorNeutralBackground1Selected,
    border: `1px solid ${tokens.colorBrandStroke1}`,
  },
  rowMain: {
    display: 'flex',
    alignItems: 'center',
    gap: '8px',
    minWidth: 0,
    flex: 1,
  },
  rowTitleWrap: {
    display: 'flex',
    flexDirection: 'column',
    minWidth: 0,
    flex: 1,
  },
  rowTitleLine: {
    display: 'flex',
    alignItems: 'center',
    gap: '6px',
    flexWrap: 'wrap',
  },
  rowMetrics: {
    display: 'flex',
    alignItems: 'center',
    gap: '8px',
    flexShrink: 0,
  },
  drawer: {
    display: 'flex',
    flexDirection: 'column',
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    padding: '14px',
    gap: '10px',
    minHeight: 0,
    overflow: 'hidden',
  },
  fileList: {
    flex: 1,
    overflowY: 'auto',
    display: 'flex',
    flexDirection: 'column',
    gap: '6px',
    paddingRight: '4px',
  },
  fileRow: {
    display: 'flex',
    flexDirection: 'column',
    gap: '3px',
    padding: '8px',
    borderRadius: tokens.borderRadiusMedium,
    backgroundColor: tokens.colorNeutralBackground1,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
  },
  footer: {
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'space-between',
    padding: '10px 16px',
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
  },
});

export const SpaceRecycleView: React.FC<Props> = ({
  lang,
  entries,
  scanning,
  onScan,
  onExecuted,
  onSystemTaskDone,
}) => {
  const styles = useStyles();
  const [search, setSearch] = useState('');
  const [groupFilter, setGroupFilter] = useState<string>('ALL');
  const [selectedIds, setSelectedIds] = useState<Set<string>>(() => {
    const init = new Set<string>();
    entries.forEach((e) => {
      if (e.rule.level === 3 && !e.rule.is_system_task) init.add(e.rule.id);
    });
    return init;
  });
  const [activeId, setActiveId] = useState<string | null>(
    entries.length > 0 ? entries[0].rule.id : null
  );
  const [previewItem, setPreviewItem] = useState<CleanupScanResult | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [executing, setExecuting] = useState(false);
  const [taskRunning, setTaskRunning] = useState<string | null>(null);
  const [taskNotice, setTaskNotice] = useState<string | null>(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [lastReport, setLastReport] = useState<CleanupExecutionReport | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const groups = useMemo(() => {
    const map = new Map<string, string>();
    entries.forEach((e) => {
      const key = e.rule.group.zh;
      if (!map.has(key)) map.set(key, pickText(e.rule.group, lang));
    });
    return Array.from(map.entries());
  }, [entries, lang]);

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    return entries.filter((e) => {
      if (groupFilter !== 'ALL' && e.rule.group.zh !== groupFilter) return false;
      if (!q) return true;
      return (
        e.rule.name.zh.toLowerCase().includes(q) ||
        e.rule.name.en.toLowerCase().includes(q) ||
        e.rule.description.zh.toLowerCase().includes(q) ||
        e.rule.description.en.toLowerCase().includes(q)
      );
    });
  }, [entries, groupFilter, search]);

  const activeEntry = useMemo(() => {
    if (previewItem && previewItem.id === activeId) return previewItem;
    return entries.find((e) => e.rule.id === activeId)?.rule || null;
  }, [entries, activeId, previewItem]);

  const selectedEntries = useMemo(
    () => entries.filter((e) => selectedIds.has(e.rule.id) && !e.rule.is_system_task),
    [entries, selectedIds]
  );

  const selectedBytes = useMemo(
    () => selectedEntries.reduce((acc, e) => acc + e.rule.cleanable_bytes, 0),
    [selectedEntries]
  );

  const totalScanBytes = useMemo(
    () => entries.reduce((acc, e) => acc + e.rule.cleanable_bytes, 0),
    [entries]
  );

  const hasRiskySelection = useMemo(
    () => selectedEntries.some((e) => e.rule.level === 0 || Boolean(e.rule.warning)),
    [selectedEntries]
  );

  const toggleSelect = (id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const applyPreset = (mode: 'recommended' | 'standard' | 'none') => {
    const next = new Set<string>();
    if (mode !== 'none') {
      entries.forEach((e) => {
        if (e.rule.is_system_task) return;
        if (mode === 'recommended' && e.rule.level === 3) next.add(e.rule.id);
        if (mode === 'standard' && e.rule.level >= 2) next.add(e.rule.id);
      });
    }
    setSelectedIds(next);
  };

  const handleSelectRow = async (entry: CleanupRuleEntry) => {
    setActiveId(entry.rule.id);
    if (!entry.scanned || entry.rule.preview_files.length === 0) {
      setPreviewLoading(true);
      try {
        const res = await api.previewCleanup(entry.rule.id);
        setPreviewItem(res);
      } catch (e) {
        setErrorMsg(String(e));
      } finally {
        setPreviewLoading(false);
      }
    } else {
      setPreviewItem(entry.rule);
    }
  };

  const runCleanup = async (dryRun: boolean) => {
    const ids = selectedEntries.map((e) => e.rule.id);
    if (ids.length === 0) return;
    setExecuting(true);
    setErrorMsg(null);
    setTaskNotice(null);
    try {
      const { report } = await api.executeCleanup(ids, dryRun);
      setLastReport(report);
      if (!dryRun) {
        onExecuted(report);
      }
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setExecuting(false);
      setConfirmOpen(false);
    }
  };

  const handleRunSystemTask = async (taskId: string) => {
    setTaskRunning(taskId);
    setErrorMsg(null);
    setTaskNotice(null);
    try {
      const { outcome, system } = await api.runSysTask(taskId);
      setTaskNotice(outcome.output || (lang === 'zh' ? '系统任务执行完成' : 'Task completed'));
      if (activeId) {
        const refreshed = await api.previewCleanup(activeId);
        setPreviewItem(refreshed);
      }
      if (onSystemTaskDone) {
        await onSystemTaskDone(system);
      } else {
        await onScan();
      }
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setTaskRunning(null);
    }
  };

  const levelBadge = (lv: number) => {
    if (lv === 3)
      return (
        <Badge size="small" appearance="tint" color="success">
          {lang === 'zh' ? 'L3 建议清理' : 'L3 Safe'}
        </Badge>
      );
    if (lv === 2)
      return (
        <Badge size="small" appearance="tint" color="informative">
          {lang === 'zh' ? 'L2 可清理' : 'L2 Optional'}
        </Badge>
      );
    if (lv === 1)
      return (
        <Badge size="small" appearance="tint" color="warning">
          {lang === 'zh' ? 'L1 谨慎' : 'L1 Caution'}
        </Badge>
      );
    return (
      <Badge size="small" appearance="filled" color="danger">
        {lang === 'zh' ? 'L0 高危专家' : 'L0 Expert'}
      </Badge>
    );
  };

  return (
    <div className={styles.root}>
      {errorMsg && (
        <MessageBar intent="error">
          <MessageBarBody>{errorMsg}</MessageBarBody>
        </MessageBar>
      )}
      {lastReport && (
        <MessageBar intent={lastReport.dry_run ? 'info' : 'success'}>
          <MessageBarBody>
            <MessageBarTitle>
              {lastReport.dry_run
                ? lang === 'zh'
                  ? 'Dry-Run 模拟完成：'
                  : 'Dry-Run Simulation:'
                : lang === 'zh'
                ? '清理完成：'
                : 'Cleanup Complete:'}
            </MessageBarTitle>
            {lang === 'zh'
              ? `${lastReport.dry_run ? '预计可释放' : '实际释放'} ${formatBytes(lastReport.total_deleted_bytes)}（${lastReport.total_deleted_files} 个文件），自动跳过 ${lastReport.total_skipped_files} 个占用/受保护文件。`
              : `${lastReport.dry_run ? 'Estimated' : 'Freed'} ${formatBytes(lastReport.total_deleted_bytes)} (${lastReport.total_deleted_files} files), safely skipped ${lastReport.total_skipped_files} locked/protected files.`}
          </MessageBarBody>
        </MessageBar>
      )}

      <div className={styles.toolbar}>
        <div className={styles.toolbarLeft}>
          <Button
            appearance="primary"
            icon={scanning ? <Spinner size="tiny" /> : <Broom24Regular />}
            disabled={scanning}
            onClick={() => onScan()}
          >
            {scanning
              ? lang === 'zh'
                ? '正在全盘扫描...'
                : 'Scanning...'
              : lang === 'zh'
              ? '一键扫描垃圾'
              : 'Scan Space'}
          </Button>
          <Button size="small" onClick={() => applyPreset('recommended')}>
            {lang === 'zh' ? '推荐安全项 (L3)' : 'Recommended (L3)'}
          </Button>
          <Button size="small" onClick={() => applyPreset('standard')}>
            {lang === 'zh' ? '标准清理 (L2+L3)' : 'Standard (L2+L3)'}
          </Button>
          <Button size="small" appearance="subtle" onClick={() => applyPreset('none')}>
            {lang === 'zh' ? '取消全选' : 'Clear'}
          </Button>
        </div>
        <div className={styles.toolbarRight}>
          <Input
            contentBefore={<Search20Regular />}
            placeholder={lang === 'zh' ? '搜索清理规则...' : 'Search cleanup rules...'}
            value={search}
            onChange={(_, d) => setSearch(d.value)}
            style={{ width: 220 }}
          />
        </div>
      </div>

      <div className={styles.pills}>
        <Button
          size="small"
          appearance={groupFilter === 'ALL' ? 'primary' : 'outline'}
          onClick={() => setGroupFilter('ALL')}
        >
          {lang === 'zh' ? `全部 (${entries.length})` : `All (${entries.length})`}
        </Button>
        {groups.map(([zhKey, label]) => (
          <Button
            key={zhKey}
            size="small"
            appearance={groupFilter === zhKey ? 'primary' : 'outline'}
            onClick={() => setGroupFilter(zhKey)}
          >
            {label}
          </Button>
        ))}
      </div>

      <div className={styles.split}>
        <div className={styles.listCard}>
          <div className={styles.listScroll}>
            {filtered.map((entry) => {
              const r = entry.rule;
              const isSelected = selectedIds.has(r.id);
              const isActive = activeId === r.id;
              return (
                <div
                  key={r.id}
                  className={`${styles.row} ${isActive ? styles.rowActive : ''}`}
                  onClick={() => handleSelectRow(entry)}
                >
                  <div className={styles.rowMain}>
                    <Checkbox
                      checked={isSelected}
                      disabled={r.is_system_task}
                      onClick={(ev) => ev.stopPropagation()}
                      onChange={() => toggleSelect(r.id)}
                    />
                    <div className={styles.rowTitleWrap}>
                      <div className={styles.rowTitleLine}>
                        <Text weight="semibold" truncate>
                          {pickText(r.name, lang)}
                        </Text>
                        {r.source === 'win11-modern' && (
                          <Badge size="small" appearance="outline" color="brand">
                            Win11
                          </Badge>
                        )}
                        {levelBadge(r.level)}
                        {r.is_system_task && (
                          <Badge size="small" appearance="tint" color="subtle">
                            {lang === 'zh' ? '系统任务' : 'System Task'}
                          </Badge>
                        )}
                      </div>
                      <Text size={200} style={{ color: tokens.colorNeutralForeground3 }} truncate>
                        {pickText(r.group, lang)} · {pickText(r.description, lang)}
                      </Text>
                    </div>
                  </div>
                  <div className={styles.rowMetrics}>
                    {(r.skipped_locked_count > 0 || r.skipped_protected_count > 0) && (
                      <Tooltip
                        content={
                          lang === 'zh'
                            ? `已自动跳过：${r.skipped_locked_count} 个被占用，${r.skipped_protected_count} 个受保护`
                            : `Auto-skipped: ${r.skipped_locked_count} locked, ${r.skipped_protected_count} protected`
                        }
                        relationship="label"
                      >
                        <Badge size="small" appearance="tint" color="warning">
                          {r.skipped_locked_count + r.skipped_protected_count}
                        </Badge>
                      </Tooltip>
                    )}
                    <Text
                      weight="semibold"
                      style={{
                        minWidth: 74,
                        textAlign: 'right',
                        color:
                          r.cleanable_bytes > 0
                            ? tokens.colorBrandForeground1
                            : tokens.colorNeutralForeground3,
                      }}
                    >
                      {entry.scanned ? formatBytes(r.cleanable_bytes) : '—'}
                    </Text>
                  </div>
                </div>
              );
            })}
          </div>
        </div>

        <div className={styles.drawer}>
          {activeEntry ? (
            <>
              <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
                <Text size={400} weight="bold">
                  {pickText(activeEntry.name, lang)}
                </Text>
                {levelBadge(activeEntry.level)}
              </div>
              <Text size={200} style={{ color: tokens.colorNeutralForeground2 }}>
                {pickText(activeEntry.description, lang)}
              </Text>
              {activeEntry.warning && (
                <MessageBar intent="warning">
                  <MessageBarBody>
                    <Warning16Regular style={{ verticalAlign: 'middle', marginRight: 4 }} />
                    {pickText(activeEntry.warning, lang)}
                  </MessageBarBody>
                </MessageBar>
              )}
              {activeEntry.system_task_hint && (
                <MessageBar intent="info">
                  <MessageBarBody>
                    <Info16Regular style={{ verticalAlign: 'middle', marginRight: 4 }} />
                    {pickText(activeEntry.system_task_hint, lang)}
                  </MessageBarBody>
                </MessageBar>
              )}

              {activeEntry.system_task_proc && (
                <div
                  style={{
                    display: 'flex',
                    flexDirection: 'column',
                    gap: '8px',
                    padding: '10px',
                    borderRadius: tokens.borderRadiusMedium,
                    backgroundColor: tokens.colorNeutralBackground1,
                    border: `1px solid ${tokens.colorBrandStroke2}`,
                  }}
                >
                  <div style={{ display: 'flex', gap: '8px', flexWrap: 'wrap' }}>
                    {activeEntry.system_task_proc === 'BuiltinComponentCleanup' && (
                      <Button
                        size="small"
                        appearance="primary"
                        icon={taskRunning === 'component_cleanup' ? <Spinner size="tiny" /> : <PlayRegular />}
                        disabled={Boolean(taskRunning)}
                        onClick={() => handleRunSystemTask('component_cleanup')}
                      >
                        {lang === 'zh' ? '执行 DISM 组件存储清理' : 'Run DISM /StartComponentCleanup'}
                      </Button>
                    )}
                    {activeEntry.system_task_proc === 'BuiltinCompactOs' && (
                      <>
                        <Button
                          size="small"
                          appearance="primary"
                          icon={taskRunning === 'compact_os_enable' ? <Spinner size="tiny" /> : <PlayRegular />}
                          disabled={Boolean(taskRunning)}
                          onClick={() => handleRunSystemTask('compact_os_enable')}
                        >
                          {lang === 'zh' ? '启用 CompactOS 系统压缩' : 'Enable CompactOS'}
                        </Button>
                        <Button
                          size="small"
                          appearance="outline"
                          disabled={Boolean(taskRunning)}
                          onClick={() => handleRunSystemTask('compact_os_disable')}
                        >
                          {lang === 'zh' ? '关闭 CompactOS 压缩' : 'Disable CompactOS'}
                        </Button>
                      </>
                    )}
                    {activeEntry.system_task_proc === 'BuiltinRestorePointCleanup' && (
                      <Button
                        size="small"
                        appearance="primary"
                        icon={taskRunning === 'restore_point_cleanup' ? <Spinner size="tiny" /> : <PlayRegular />}
                        disabled={Boolean(taskRunning)}
                        onClick={() => handleRunSystemTask('restore_point_cleanup')}
                      >
                        {lang === 'zh' ? '删除最旧系统还原点 (VSS)' : 'Delete Oldest Restore Point'}
                      </Button>
                    )}
                    {activeEntry.system_task_proc === 'BuiltinDriverCleanup' && (
                      <Button
                        size="small"
                        appearance="primary"
                        icon={taskRunning === 'driver_cleanup' ? <Spinner size="tiny" /> : <PlayRegular />}
                        disabled={Boolean(taskRunning)}
                        onClick={() => handleRunSystemTask('driver_cleanup')}
                      >
                        {lang === 'zh' ? '一键卸载全部过期重复驱动' : 'Clean Outdated Drivers'}
                      </Button>
                    )}
                  </div>
                  {taskNotice && (
                    <Text
                      size={100}
                      style={{
                        fontFamily: 'Consolas, monospace',
                        whiteSpace: 'pre-wrap',
                        color: tokens.colorPaletteGreenForeground1,
                      }}
                    >
                      {taskNotice}
                    </Text>
                  )}
                </div>
              )}

              <div
                style={{
                  display: 'flex',
                  gap: '8px',
                  flexWrap: 'wrap',
                  padding: '8px',
                  borderRadius: tokens.borderRadiusMedium,
                  backgroundColor: tokens.colorNeutralBackground1,
                }}
              >
                <Badge appearance="tint" color="success">
                  {lang === 'zh' ? '可清理' : 'Ready'}: {formatBytes(activeEntry.cleanable_bytes)} (
                  {activeEntry.cleanable_count})
                </Badge>
                <Badge appearance="tint" color="warning">
                  {lang === 'zh' ? '占用跳过' : 'Locked'}:{' '}
                  {formatBytes(activeEntry.skipped_locked_bytes)} ({activeEntry.skipped_locked_count})
                </Badge>
                <Badge appearance="tint" color="danger">
                  {lang === 'zh' ? '受保护' : 'Protected'}:{' '}
                  {formatBytes(activeEntry.skipped_protected_bytes)} (
                  {activeEntry.skipped_protected_count})
                </Badge>
              </div>

              <Text size={200} weight="semibold">
                {lang === 'zh'
                  ? `Dry-Run 文件预览（最多展示前 100 个大文件）`
                  : `Dry-Run File Preview (Top 100 largest files)`}
              </Text>

              <div className={styles.fileList}>
                {previewLoading ? (
                  <Spinner label={lang === 'zh' ? '正在分析文件权限与占用...' : 'Analyzing files...'} />
                ) : activeEntry.preview_files.length === 0 ? (
                  <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                    {lang === 'zh'
                      ? '暂无可展示的文件明细（目录为空或尚未扫描）。'
                      : 'No matching files found for this rule.'}
                  </Text>
                ) : (
                  activeEntry.preview_files.map((f, idx) => (
                    <div key={`${f.path}-${idx}`} className={styles.fileRow}>
                      <div style={{ display: 'flex', justifyContent: 'space-between', gap: 8 }}>
                        <div style={{ display: 'flex', alignItems: 'center', gap: 4 }}>
                          {f.status === 'Ready' && (
                            <CheckmarkCircle16Regular style={{ color: tokens.colorPaletteGreenForeground1 }} />
                          )}
                          {f.status === 'Locked' && (
                            <LockClosed16Regular style={{ color: tokens.colorPaletteDarkOrangeForeground1 }} />
                          )}
                          {f.status === 'Protected' && (
                            <ShieldLock16Regular style={{ color: tokens.colorPaletteRedForeground1 }} />
                          )}
                          <Text size={200} weight="semibold">
                            {f.status}
                          </Text>
                        </div>
                        <Text size={200} weight="semibold">
                          {formatBytes(f.size_bytes)}
                        </Text>
                      </div>
                      <Text
                        size={100}
                        style={{
                          wordBreak: 'break-all',
                          color: tokens.colorNeutralForeground2,
                          fontFamily: 'Consolas, monospace',
                        }}
                      >
                        {f.path}
                      </Text>
                      {f.reason && (
                        <Text size={100} style={{ color: tokens.colorPaletteDarkOrangeForeground2 }}>
                          {f.reason}
                        </Text>
                      )}
                    </div>
                  ))
                )}
              </div>
            </>
          ) : (
            <Text>{lang === 'zh' ? '点击左侧规则查看文件级预览' : 'Select a rule to preview'}</Text>
          )}
        </div>
      </div>

      <div className={styles.footer}>
        <div style={{ display: 'flex', gap: '16px', alignItems: 'center' }}>
          <Text size={300}>
            {lang === 'zh'
              ? `本次扫描总可释放：${formatBytes(totalScanBytes)}`
              : `Total Reclaimable: ${formatBytes(totalScanBytes)}`}
          </Text>
          <Text size={300} weight="bold" style={{ color: tokens.colorBrandForeground1 }}>
            {lang === 'zh'
              ? `已勾选 ${selectedEntries.length} 项：${formatBytes(selectedBytes)}`
              : `Selected ${selectedEntries.length}: ${formatBytes(selectedBytes)}`}
          </Text>
        </div>
        <div style={{ display: 'flex', gap: '8px' }}>
          <Button
            disabled={executing || selectedEntries.length === 0}
            onClick={() => runCleanup(true)}
          >
            {lang === 'zh' ? '模拟清理 (Dry-Run)' : 'Simulate (Dry-Run)'}
          </Button>
          <Button
            appearance="primary"
            icon={<Delete24Regular />}
            disabled={executing || selectedEntries.length === 0}
            onClick={() => {
              if (hasRiskySelection) setConfirmOpen(true);
              else runCleanup(false);
            }}
          >
            {executing
              ? lang === 'zh'
                ? '清理中...'
                : 'Cleaning...'
              : lang === 'zh'
              ? '安全清理选中项'
              : 'Clean Selected'}
          </Button>
        </div>
      </div>

      <Dialog open={confirmOpen} onOpenChange={(_, d) => setConfirmOpen(d.open)}>
        <DialogSurface>
          <DialogBody>
            <DialogTitle>
              {lang === 'zh' ? '确认清理高风险或带警告项？' : 'Confirm Cleanup Warning'}
            </DialogTitle>
            <DialogContent>
              <Text>
                {lang === 'zh'
                  ? '您选中的规则包含 Level 0 专家项或带有官方注意提示的清理项，清理后部分功能或缓存将需要重新生成/下载。确定继续吗？'
                  : 'Your selection includes Level 0 or warning-tagged rules. Continue cleaning?'}
              </Text>
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={() => setConfirmOpen(false)}>
                {lang === 'zh' ? '取消' : 'Cancel'}
              </Button>
              <Button appearance="primary" onClick={() => runCleanup(false)}>
                {lang === 'zh' ? '确认执行清理' : 'Confirm Clean'}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </div>
  );
};
