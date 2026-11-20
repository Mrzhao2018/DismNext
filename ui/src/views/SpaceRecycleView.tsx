import { calculateSelectedBytes } from '../cleanupSelection';
import { useMaintenance } from '../MaintenanceJobs';
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
  Add20Regular,
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
  ApiError,
  CleanupExecutionReport,
  CleanupReview,
  CleanupRuleEntry,
  CleanupScanResult,
  CleanupScanSummary,
  Lang,
  SystemInfo,
  api,
  formatBytes,
  pickText,
} from '../api';

interface Props {
  visible: boolean;
  lang: Lang;
  entries: CleanupRuleEntry[];
  summary: CleanupScanSummary | null;
  scanning: boolean;
  onScan: () => Promise<void>;
  onEntriesUpdated?: (items: CleanupRuleEntry[]) => void;
  onExecuted: (system: SystemInfo) => void;
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
  reviewList: {
    display: 'flex',
    flexDirection: 'column',
    gap: '8px',
    maxHeight: '52vh',
    overflowY: 'auto',
    marginTop: '10px',
    paddingRight: '4px',
  },
  reviewMetrics: {
    display: 'flex',
    flexWrap: 'wrap',
    columnGap: '12px',
    rowGap: '4px',
  },
  reviewErrors: {
    margin: 0,
    paddingLeft: '20px',
    overflowWrap: 'anywhere',
    color: tokens.colorPaletteRedForeground1,
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

const CleanupDetails: React.FC<{
  lang: Lang;
  report: CleanupExecutionReport;
  confirming?: boolean;
}> = ({ lang, report, confirming = false }) => {
  const styles = useStyles();
  const zh = lang === 'zh';
  const labels = report.dry_run
    ? {
        permanent_delete: zh ? '将永久删除' : 'Permanently delete',
        recycle: zh ? '将移入回收站' : 'Move to Recycle Bin',
        remove_driver: zh ? '将卸载驱动包' : 'Remove driver packages',
      }
    : {
        permanent_delete: zh ? '已永久删除' : 'Permanently deleted',
        recycle: zh ? '已移入回收站' : 'Moved to Recycle Bin',
        remove_driver: zh ? '已卸载驱动包' : 'Driver packages removed',
      };
  const actionKinds = ['permanent_delete', 'recycle', 'remove_driver'] as const;

  return (
    <div className={styles.reviewList}>
      {report.outcomes.map((outcome) => (
        <div key={outcome.id} className={styles.fileRow}>
          <div className={styles.rowTitleLine}>
            <Text weight="semibold">{pickText(outcome.name, lang)}</Text>
            {confirming && outcome.level !== null && outcome.level <= 1 && (
              <Badge size="small" appearance="tint" color={outcome.level === 0 ? 'danger' : 'warning'}>
                {outcome.level === 0
                  ? (zh ? '高风险' : 'High risk')
                  : (zh ? '谨慎' : 'Caution')}
              </Badge>
            )}
          </div>
          {confirming && outcome.description && (
            <Text size={200}>{pickText(outcome.description, lang)}</Text>
          )}
          {confirming && outcome.warning && (
            <Text size={200} style={{ color: tokens.colorPaletteDarkOrangeForeground1 }}>
              {pickText(outcome.warning, lang)}
            </Text>
          )}
          <div className={styles.reviewMetrics}>
            {actionKinds.map((kind) => {
              const totals = outcome.actions[kind];
              return totals.count > 0 && (
                <Text key={kind} size={200}>
                  {labels[kind]} {totals.count} · {formatBytes(totals.bytes)}
                </Text>
              );
            })}
            {outcome.skipped_locked_files > 0 && (
              <Text size={200}>{zh ? '跳过（占用）' : 'Skipped (locked)'} {outcome.skipped_locked_files}</Text>
            )}
            {outcome.skipped_protected_files > 0 && (
              <Text size={200}>{zh ? '跳过（受保护）' : 'Skipped (protected)'} {outcome.skipped_protected_files}</Text>
            )}
            {!!outcome.review?.directories && (
              <Text size={200}>
                {report.dry_run
                  ? (zh ? '待检查是否为空的目录' : 'Directories to check for emptiness')
                  : (zh ? '已移除空目录' : 'Empty directories removed')} {outcome.review.directories}
              </Text>
            )}
            {!!outcome.review?.skipped_changed && (
              <Text size={200}>{zh ? '跳过（已变更）' : 'Skipped (changed)'} {outcome.review.skipped_changed}</Text>
            )}
            {!!outcome.review?.skipped_missing && (
              <Text size={200}>{zh ? '跳过（已不在原路径）' : 'Skipped (missing)'} {outcome.review.skipped_missing}</Text>
            )}
            {outcome.failed_files > 0 && (
              <Text size={200} weight="semibold">
                {zh ? '失败' : 'Failed'} {outcome.failed_files}
              </Text>
            )}
            {outcome.deleted_files === 0 && outcome.skipped_locked_files === 0
              && outcome.skipped_protected_files === 0 && outcome.failed_files === 0
              && !outcome.review?.directories && !outcome.review?.skipped_changed && !outcome.review?.skipped_missing && (
              <Text size={200}>{zh ? '无待处理目标' : 'No targets assigned to this rule'}</Text>
            )}
          </div>
          {outcome.errors.length > 0 && (
            <ul className={styles.reviewErrors}>
              {outcome.errors.map((error, index) => <li key={index}><Text size={200}>{error}</Text></li>)}
              {outcome.failed_files > outcome.errors.length && (
                <li><Text size={200}>
                  {zh
                    ? `还有 ${outcome.failed_files - outcome.errors.length} 项失败未展开`
                    : `${outcome.failed_files - outcome.errors.length} more failures`}
                </Text></li>
              )}
            </ul>
          )}
        </div>
      ))}
    </div>
  );
};

export const SpaceRecycleView: React.FC<Props> = ({
  visible,
  lang,
  entries,
  summary,
  scanning,
  onScan,
  onEntriesUpdated,
  onExecuted,
  onSystemTaskDone,
}) => {
  const { active: activeMaintenance, run: runMaintenance } = useMaintenance();
  const styles = useStyles();
  const [search, setSearch] = useState('');
  const deferredSearch = React.useDeferredValue(search);
  const [groupFilter, setGroupFilter] = useState<string>('ALL');
  const [customModalOpen, setCustomModalOpen] = useState(false);
  const [customName, setCustomName] = useState('');
  const [customRoot, setCustomRoot] = useState('');
  const [customQueries, setCustomQueries] = useState('*.log, *.tmp');
  const [customExcluded, setCustomExcluded] = useState('');
  const [customLevel, setCustomLevel] = useState<number>(2);
  const [customTesting, setCustomTesting] = useState(false);
  const [customSaving, setCustomSaving] = useState(false);
  const [customPreview, setCustomPreview] = useState<CleanupScanResult | null>(null);
  const [customError, setCustomError] = useState<string | null>(null);
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
  const [cleanupPhase, setCleanupPhase] = useState<'preview' | 'simulate' | 'execute' | null>(null);
  const [taskRunning, setTaskRunning] = useState<string | null>(null);
  const [pendingSysTask, setPendingSysTask] = useState<string | null>(null);
  const [taskNotice, setTaskNotice] = useState<string | null>(null);
  const [lockBusyKey, setLockBusyKey] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState<CleanupReview | null>(null);
  const [lastReport, setLastReport] = useState<CleanupExecutionReport | null>(null);
  const [reportOpen, setReportOpen] = useState(false);
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
    const q = deferredSearch.trim().toLowerCase();
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
  }, [entries, groupFilter, deferredSearch]);

  const activeEntry = useMemo(() => {
    if (previewItem && previewItem.id === activeId) return previewItem;
    return entries.find((e) => e.rule.id === activeId)?.rule || null;
  }, [entries, activeId, previewItem]);

  const selectedEntries = useMemo(
    () => entries.filter((e) => selectedIds.has(e.rule.id) && !e.rule.is_system_task),
    [entries, selectedIds]
  );

  const selectedBytes = useMemo(
    () => calculateSelectedBytes(selectedEntries, summary),
    [selectedEntries, summary]
  );
  const pendingScan = lang === 'zh' ? '待扫描' : 'Pending scan';
  const totalScanLabel = summary ? formatBytes(summary.cleanable_bytes) : pendingScan;
  const selectedLabel = selectedBytes === null ? pendingScan : formatBytes(selectedBytes);

  const failedCount = lastReport?.outcomes.reduce((count, row) => count + row.failed_files, 0) ?? 0;

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

  const runCleanup = async (action: 'preview' | 'simulate' | 'execute') => {
    const ids = selectedEntries.map(({ rule }) => rule.id);
    const planId = confirmation?.plan_id;
    if (scanning || cleanupPhase || (action === 'execute' ? !planId : ids.length === 0)) return;
    setCleanupPhase(action);
    setErrorMsg(null);
    setTaskNotice(null);
    try {
      if (action === 'preview') {
        const review = await api.reviewCleanup(ids);
        if (!review.plan_id) {
          setErrorMsg(lang === 'zh' ? '所选规则已不可用，请刷新后重试。' : 'The selected rules are unavailable. Refresh and try again.');
        } else {
          setConfirmation(review);
        }
      } else if (action === 'execute' && planId) {
        const { report, system } = await api.confirmCleanup(planId);
        setLastReport(report);
        setReportOpen(true);
        onExecuted(system);
      } else {
        const { report } = await api.simulateCleanup(ids);
        setLastReport(report);
        setReportOpen(true);
      }
    } catch (e) {
      setErrorMsg(action === 'execute' && e instanceof ApiError && e.status === 409
        ? (lang === 'zh' ? '这份清理计划已失效，请重新核对后再执行。' : 'This cleanup plan is no longer available. Review the selected rules again.')
        : String(e));
    } finally {
      setCleanupPhase(null);
      if (action === 'execute') setConfirmation(null);
    }
  };

  const dismissConfirmation = () => {
    const planId = confirmation?.plan_id;
    setConfirmation(null);
    if (planId) void api.discardCleanupReview(planId).catch((e) => setErrorMsg(String(e)));
  };

  const handleRunSystemTask = async (taskId: string) => {
    setPendingSysTask(null);
    setTaskRunning(taskId);
    setErrorMsg(null);
    setTaskNotice(null);
    try {
      const result = await runMaintenance({ kind: 'system_task', task_id: taskId }, sysTaskDescription(taskId).title);
      if (!result) return;
      const { outcome, system } = result;
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

  const handleInspectLocks = async (filePath: string) => {
    setLockBusyKey(filePath);
    setErrorMsg(null);
    try {
      const { processes } = await api.inspectFileLocks(filePath);
      setPreviewItem((prev) => {
        if (!prev) return prev;
        return {
          ...prev,
          preview_files: prev.preview_files.map((pf) =>
            pf.path === filePath
              ? {
                  ...pf,
                  locking_processes: processes,
                  reason:
                    processes.length > 0
                      ? `被进程占用: ${processes
                          .map((p) => `${p.app_name} (PID ${p.pid})`)
                          .join(', ')}`
                      : lang === 'zh'
                      ? '未检测到用户态句柄锁定（可能由内核或短期句柄占用）'
                      : pf.reason,
                }
              : pf
          ),
        };
      });
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setLockBusyKey(null);
    }
  };

  const handleKillLockPid = async (path: string, pid: number, startTime: string, appName: string) => {
    setLockBusyKey(`pid:${pid}`);
    setErrorMsg(null);
    try {
      await api.terminateLockingProcess(path, pid, startTime);
      setTaskNotice(
        lang === 'zh'
          ? `已结束占用进程 ${appName} (PID ${pid})，正在刷新文件状态...`
          : `Terminated ${appName} (PID ${pid}), refreshing file preview...`
      );
      if (activeId) {
        const refreshed = await api.previewCleanup(activeId);
        setPreviewItem(refreshed);
      }
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setLockBusyKey(null);
    }
  };

  const sysTaskDescription = (taskId: string | null): { title: string; body: string } => {
    switch (taskId) {
      case 'component_cleanup':
        return {
          title: lang === 'zh' ? '确认执行 DISM 组件存储清理？' : 'Confirm DISM Component Cleanup',
          body:
            lang === 'zh'
              ? '此操作将调用原生 Dism.exe (/StartComponentCleanup) 永久清除已被取代的历史 WinSxS 系统更新备份，执行后旧系统补丁将无法回退。是否继续？'
              : 'This runs DISM /StartComponentCleanup and permanently removes superseded WinSxS component backups. Older updates cannot be uninstalled afterward.',
        };
      case 'compact_os_enable':
        return {
          title: lang === 'zh' ? '确认启用 CompactOS 系统压缩？' : 'Confirm Enable CompactOS',
          body:
            lang === 'zh'
              ? '启用 CompactOS 将对 Windows 系统核心二进制文件执行压缩（耗时数分钟），以节省系统盘空间。确定继续吗？'
              : 'Enabling CompactOS compresses OS binaries to save drive space and may take several minutes. Continue?',
        };
      case 'compact_os_disable':
        return {
          title: lang === 'zh' ? '确认关闭 CompactOS 压缩？' : 'Confirm Disable CompactOS',
          body:
            lang === 'zh'
              ? '关闭 CompactOS 将解压缩 Windows 系统核心文件，并会额外占用数 GB 系统盘空间。确定继续吗？'
              : 'Disabling CompactOS decompresses OS binaries and increases disk usage by several GB. Continue?',
        };
      case 'restore_point_cleanup':
        return {
          title: lang === 'zh' ? '确认删除最旧系统还原点？' : 'Confirm Delete Oldest Restore Point',
          body:
            lang === 'zh'
              ? '此操作将调用 vssadmin 删除系统盘最旧的卷影复制还原点以释放磁盘空间，删除后该还原点不可恢复。确定继续吗？'
              : 'This runs vssadmin to permanently delete the oldest VSS shadow copy restore point on the system drive. Continue?',
        };
      case 'driver_cleanup':
        return {
          title: lang === 'zh' ? '确认卸载过期重复驱动包？' : 'Confirm Outdated Driver Removal',
          body:
            lang === 'zh'
              ? '将调用 pnputil 安全移除已被同厂商新版本（且硬件 ID 完全覆盖）取代的旧版驱动包；当前正在使用的驱动保持不变。确定继续吗？'
              : 'Safely delete superseded duplicate driver packages via pnputil while keeping active drivers?',
        };
      default:
        return { title: '', body: '' };
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
        <MessageBar intent={failedCount > 0 || (!lastReport.dry_run && lastReport.total_skipped_files > 0) ? 'warning' : lastReport.dry_run ? 'info' : 'success'}>
          <MessageBarBody>
            <MessageBarTitle>
              {lastReport.dry_run
                ? (lang === 'zh' ? '模拟结果' : 'Simulation results')
                : (lang === 'zh' ? '清理结果' : 'Cleanup results')}
            </MessageBarTitle>
            {lang === 'zh'
              ? `${lastReport.dry_run ? '预计处理' : '已处理'} ${lastReport.total_deleted_files} 项 · 跳过 ${lastReport.total_skipped_files} 项 · 失败 ${failedCount} 项`
              : `${lastReport.total_deleted_files} ${lastReport.dry_run ? 'planned' : 'processed'} · ${lastReport.total_skipped_files} skipped · ${failedCount} failed`}
            <Button size="small" appearance="subtle" onClick={() => setReportOpen(true)}>
              {lang === 'zh' ? '查看明细' : 'View Details'}
            </Button>
          </MessageBarBody>
        </MessageBar>
      )}

      <div className={styles.toolbar}>
        <div className={styles.toolbarLeft}>
          <Button
            appearance="primary"
            icon={scanning ? <Spinner size="tiny" /> : <Broom24Regular />}
            disabled={scanning || cleanupPhase !== null}
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
          <Button
            size="small"
            appearance="outline"
            icon={<Add20Regular />}
            onClick={() => {
              setCustomPreview(null);
              setCustomError(null);
              setCustomModalOpen(true);
            }}
          >
            {lang === 'zh' ? '+ 自定义规则' : '+ Custom Rule'}
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
                        {r.source === 'custom' && (
                          <Badge size="small" appearance="outline" color="informative">
                            {lang === 'zh' ? '自定义' : 'Custom'}
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
              <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', gap: 8 }}>
                <Text size={400} weight="bold">
                  {pickText(activeEntry.name, lang)}
                </Text>
                <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                  {activeEntry.source === 'custom' && (
                    <Button
                      size="small"
                      appearance="subtle"
                      onClick={async () => {
                        try {
                          const res = await api.deleteCustomRule(activeEntry.id);
                          onEntriesUpdated?.(res.items);
                          setActiveId(res.items.length > 0 ? res.items[0].rule.id : null);
                          setPreviewItem(null);
                          setTaskNotice(
                            lang === 'zh'
                              ? `已删除自定义规则：${pickText(activeEntry.name, lang)}`
                              : `Deleted custom rule: ${pickText(activeEntry.name, lang)}`
                          );
                        } catch (err) {
                          setErrorMsg(String(err));
                        }
                      }}
                    >
                      {lang === 'zh' ? '删除自定义规则' : 'Delete Custom Rule'}
                    </Button>
                  )}
                  {levelBadge(activeEntry.level)}
                </div>
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
                        disabled={Boolean(activeMaintenance)}
                        onClick={() => setPendingSysTask('component_cleanup')}
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
                          disabled={Boolean(activeMaintenance)}
                          onClick={() => setPendingSysTask('compact_os_enable')}
                        >
                          {lang === 'zh' ? '启用 CompactOS 系统压缩' : 'Enable CompactOS'}
                        </Button>
                        <Button
                          size="small"
                          appearance="outline"
                          disabled={Boolean(activeMaintenance)}
                          onClick={() => setPendingSysTask('compact_os_disable')}
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
                        disabled={Boolean(activeMaintenance)}
                        onClick={() => setPendingSysTask('restore_point_cleanup')}
                      >
                        {lang === 'zh' ? '删除最旧系统还原点 (VSS)' : 'Delete Oldest Restore Point'}
                      </Button>
                    )}
                    {activeEntry.system_task_proc === 'BuiltinDriverCleanup' && (
                      <Button
                        size="small"
                        appearance="primary"
                        icon={taskRunning === 'driver_cleanup' ? <Spinner size="tiny" /> : <PlayRegular />}
                        disabled={Boolean(activeMaintenance)}
                        onClick={() => setPendingSysTask('driver_cleanup')}
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
                      {f.status === 'Locked' && (
                        <div
                          style={{
                            display: 'flex',
                            alignItems: 'center',
                            gap: '6px',
                            flexWrap: 'wrap',
                            marginTop: '3px',
                          }}
                        >
                          {(!f.locking_processes || f.locking_processes.length === 0) && (
                            <Button
                              size="small"
                              appearance="subtle"
                              disabled={lockBusyKey === f.path}
                              onClick={() => void handleInspectLocks(f.path)}
                            >
                              {lockBusyKey === f.path
                                ? lang === 'zh'
                                  ? '检测占用中...'
                                  : 'Inspecting...'
                                : lang === 'zh'
                                ? '溯源占用进程'
                                : 'Inspect Lock PID'}
                            </Button>
                          )}
                          {f.locking_processes?.map((lp) => (
                            <div
                              key={lp.pid}
                              style={{ display: 'flex', alignItems: 'center', gap: 4 }}
                            >
                              <Badge size="small" appearance="tint" color={lp.is_critical ? 'warning' : 'brand'}>
                                {lp.app_name} (PID {lp.pid})
                              </Badge>
                              {!lp.is_critical && (
                                <Button
                                  size="small"
                                  appearance="outline"
                                  disabled={lockBusyKey === `pid:${lp.pid}`}
                                  onClick={() => void handleKillLockPid(f.path, lp.pid, lp.process_start_time, lp.app_name)}
                                >
                                  {lockBusyKey === `pid:${lp.pid}`
                                    ? '...'
                                    : lang === 'zh'
                                    ? '结束进程'
                                    : 'Kill'}
                                </Button>
                              )}
                            </div>
                          ))}
                        </div>
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
              ? `可处理目标大小 · ${totalScanLabel}`
              : `Target size · ${totalScanLabel}`}
          </Text>
          <Text size={300} weight="bold" style={{ color: tokens.colorBrandForeground1 }}>
            {lang === 'zh'
              ? `已勾选 ${selectedEntries.length} 项：${selectedLabel}`
              : `Selected ${selectedEntries.length}: ${selectedLabel}`}
          </Text>
        </div>
        <div style={{ display: 'flex', gap: '8px' }}>
          <Button
            disabled={scanning || cleanupPhase !== null || selectedEntries.length === 0}
            onClick={() => void runCleanup('simulate')}
          >
            {cleanupPhase === 'simulate'
              ? (lang === 'zh' ? '正在模拟…' : 'Simulating…')
              : (lang === 'zh' ? '模拟清理 (Dry Run)' : 'Simulate (Dry Run)')}
          </Button>
          <Button
            appearance="primary"
            icon={<Delete24Regular />}
            disabled={scanning || cleanupPhase !== null || selectedEntries.length === 0}
            onClick={() => void runCleanup('preview')}
          >
            {cleanupPhase === 'preview'
              ? (lang === 'zh' ? '正在准备预览…' : 'Preparing review…')
              : (lang === 'zh' ? '核对并清理选中项' : 'Review and Clean Selected')}
          </Button>
        </div>
      </div>

      <Dialog
        open={visible && confirmation !== null}
        onOpenChange={(_, d) => {
          if (!d.open && !cleanupPhase) dismissConfirmation();
        }}
      >
        <DialogSurface style={{ maxWidth: 720 }}>
          <DialogBody>
            <DialogTitle>
              {lang === 'zh' ? '核对本次清理' : 'Review Cleanup'}
            </DialogTitle>
            <DialogContent>
              <Text>
                {lang === 'zh'
                  ? '仅处理本次核对时记录的目标。执行前会复核目标，已变更、移走或被占用的项目会跳过。计划有效期为 5 分钟。'
                  : 'Only targets recorded for this review are eligible. Changed, missing, or locked targets are skipped. This plan expires after 5 minutes.'}
              </Text>
              {confirmation && <CleanupDetails lang={lang} report={confirmation.report} confirming />}
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" disabled={cleanupPhase !== null} onClick={dismissConfirmation}>
                {lang === 'zh' ? '取消' : 'Cancel'}
              </Button>
              <Button
                appearance="primary"
                disabled={scanning || cleanupPhase !== null}
                icon={cleanupPhase === 'execute' ? <Spinner size="tiny" /> : undefined}
                onClick={() => void runCleanup('execute')}
              >
                {cleanupPhase === 'execute'
                  ? (lang === 'zh' ? '正在清理…' : 'Cleaning…')
                  : (lang === 'zh' ? '确认执行清理' : 'Confirm Cleanup')}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>

      <Dialog open={visible && reportOpen} onOpenChange={(_, d) => setReportOpen(d.open)}>
        <DialogSurface style={{ maxWidth: 720 }}>
          <DialogBody>
            <DialogTitle>
              {lastReport?.dry_run
                ? (lang === 'zh' ? '模拟清理明细' : 'Simulation Details')
                : (lang === 'zh' ? '清理结果明细' : 'Cleanup Details')}
            </DialogTitle>
            <DialogContent>
              {lastReport && <CleanupDetails lang={lang} report={lastReport} />}
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={() => setReportOpen(false)}>
                {lang === 'zh' ? '关闭' : 'Close'}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>

      <Dialog
        open={visible && Boolean(pendingSysTask)}
        onOpenChange={(_, d) => {
          if (!d.open) setPendingSysTask(null);
        }}
      >
        <DialogSurface>
          <DialogBody>
            <DialogTitle>{sysTaskDescription(pendingSysTask).title}</DialogTitle>
            <DialogContent>
              <Text>{sysTaskDescription(pendingSysTask).body}</Text>
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={() => setPendingSysTask(null)}>
                {lang === 'zh' ? '取消' : 'Cancel'}
              </Button>
              <Button
                appearance="primary"
                onClick={() => pendingSysTask && handleRunSystemTask(pendingSysTask)}
              >
                {lang === 'zh' ? '确认执行任务' : 'Confirm Task'}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>

      <Dialog open={visible && customModalOpen} onOpenChange={(_, d) => setCustomModalOpen(d.open)}>
        <DialogSurface style={{ maxWidth: 640 }}>
          <DialogBody>
            <DialogTitle>
              {lang === 'zh' ? '新建自定义清理规则' : 'Create Custom Cleanup Rule'}
            </DialogTitle>
            <DialogContent>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
                {customError && (
                  <MessageBar intent="error">
                    <MessageBarBody>{customError}</MessageBarBody>
                  </MessageBar>
                )}
                <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                  <Text size={200} weight="semibold">
                    {lang === 'zh' ? '规则名称 *' : 'Rule Name *'}
                  </Text>
                  <Input
                    placeholder={lang === 'zh' ? '例如：我的构建缓存日志' : 'e.g. Build Temp Logs'}
                    value={customName}
                    onChange={(_, d) => setCustomName(d.value)}
                  />
                </div>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                  <Text size={200} weight="semibold">
                    {lang === 'zh' ? '目标根目录 *（支持 %LOCALAPPDATA% 等环境变量）' : 'Target Root Path *'}
                  </Text>
                  <Input
                    placeholder={lang === 'zh' ? '例如：%LOCALAPPDATA%\\MyApp\\Logs 或 D:\\Builds\\Logs' : 'e.g. %LOCALAPPDATA%\\MyApp\\Logs'}
                    value={customRoot}
                    onChange={(_, d) => setCustomRoot(d.value)}
                  />
                </div>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                  <Text size={200} weight="semibold">
                    {lang === 'zh' ? '匹配文件通配符（逗号分隔，留空则清理整个目标目录内容）' : 'File Queries (comma-separated, optional)'}
                  </Text>
                  <Input
                    placeholder="*.log, *.tmp, *.dmp"
                    value={customQueries}
                    onChange={(_, d) => setCustomQueries(d.value)}
                  />
                </div>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                  <Text size={200} weight="semibold">
                    {lang === 'zh' ? '排除项（逗号分隔，可选）' : 'Excluded Patterns (optional)'}
                  </Text>
                  <Input
                    placeholder="latest.log, config.json"
                    value={customExcluded}
                    onChange={(_, d) => setCustomExcluded(d.value)}
                  />
                </div>
                <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <Text size={200} weight="semibold">
                    {lang === 'zh' ? '安全等级：' : 'Level:'}
                  </Text>
                  {[3, 2, 1].map((lv) => (
                    <Button
                      key={lv}
                      size="small"
                      appearance={customLevel === lv ? 'primary' : 'outline'}
                      onClick={() => setCustomLevel(lv)}
                    >
                      {lv === 3 ? 'L3 推荐' : lv === 2 ? 'L2 标准' : 'L1 谨慎'}
                    </Button>
                  ))}
                </div>

                {customPreview && (
                  <div
                    style={{
                      padding: 10,
                      borderRadius: tokens.borderRadiusMedium,
                      backgroundColor: tokens.colorNeutralBackground3,
                      display: 'flex',
                      flexDirection: 'column',
                      gap: 6,
                      maxHeight: 200,
                      overflowY: 'auto',
                    }}
                  >
                    <Text size={200} weight="semibold">
                      {lang === 'zh'
                        ? `Dry-Run 测试命中：${customPreview.cleanable_count} 个可清理文件 (${formatBytes(customPreview.cleanable_bytes)}) · 跳过占用 ${customPreview.skipped_locked_count} 个`
                        : `Dry-Run Matched: ${customPreview.cleanable_count} ready (${formatBytes(customPreview.cleanable_bytes)}), ${customPreview.skipped_locked_count} locked`}
                    </Text>
                    {customPreview.preview_files.slice(0, 15).map((pf) => (
                      <Text
                        key={pf.path}
                        size={100}
                        truncate
                        style={{ fontFamily: 'Consolas, monospace', color: tokens.colorNeutralForeground2 }}
                      >
                        [{pf.status}] {formatBytes(pf.size_bytes)} - {pf.path}
                      </Text>
                    ))}
                  </div>
                )}
              </div>
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={() => setCustomModalOpen(false)}>
                {lang === 'zh' ? '取消' : 'Cancel'}
              </Button>
              <Button
                appearance="outline"
                disabled={customTesting || !customName.trim() || !customRoot.trim()}
                onClick={async () => {
                  setCustomTesting(true);
                  setCustomError(null);
                  try {
                    const splitList = (s: string) =>
                      s
                        .split(',')
                        .map((x) => x.trim())
                        .filter(Boolean);
                    const res = await api.testCustomRule({
                      name_zh: customName,
                      root_path: customRoot,
                      queries: splitList(customQueries),
                      excluded: splitList(customExcluded),
                      level: customLevel,
                    });
                    setCustomPreview(res);
                  } catch (err) {
                    setCustomError(String(err));
                  } finally {
                    setCustomTesting(false);
                  }
                }}
              >
                {customTesting
                  ? lang === 'zh'
                    ? '测试中...'
                    : 'Testing...'
                  : lang === 'zh'
                  ? 'Dry-Run 测试扫描'
                  : 'Test (Dry-Run)'}
              </Button>
              <Button
                appearance="primary"
                disabled={customSaving || !customName.trim() || !customRoot.trim()}
                onClick={async () => {
                  setCustomSaving(true);
                  setCustomError(null);
                  try {
                    const splitList = (s: string) =>
                      s
                        .split(',')
                        .map((x) => x.trim())
                        .filter(Boolean);
                    const res = await api.saveCustomRule({
                      name_zh: customName,
                      root_path: customRoot,
                      queries: splitList(customQueries),
                      excluded: splitList(customExcluded),
                      level: customLevel,
                    });
                    onEntriesUpdated?.(res.items);
                    setActiveId(res.saved.id);
                    setPreviewItem(res.saved);
                    setCustomModalOpen(false);
                    setCustomName('');
                    setCustomRoot('');
                  } catch (err) {
                    setCustomError(String(err));
                  } finally {
                    setCustomSaving(false);
                  }
                }}
              >
                {customSaving
                  ? lang === 'zh'
                    ? '保存中...'
                    : 'Saving...'
                  : lang === 'zh'
                  ? '保存并启用规则'
                  : 'Save Rule'}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </div>
  );
};
