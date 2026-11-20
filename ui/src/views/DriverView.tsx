import { VirtualList } from './VirtualList';
import React, { useCallback, useEffect, useMemo, useState } from 'react';
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
  Spinner,
  Text,
  makeStyles,
  tokens,
} from '@fluentui/react-components';
import {
  ArrowClockwise20Regular,
  ArrowDownloadRegular,
  Delete16Regular,
  DeveloperBoardRegular,
  FolderOpen16Regular,
  Search20Regular,
  Warning16Regular,
} from '@fluentui/react-icons';
import { DriverBackupReport, DriverPackageInfo, Lang, api, formatBytes } from '../api';

interface Props {
  lang: Lang;
  isAdmin: boolean;
}

const useStyles = makeStyles({
  root: {
    display: 'flex',
    flexDirection: 'column',
    height: '100%',
    gap: '12px',
    overflow: 'hidden',
  },
  summaryGrid: {
    display: 'grid',
    gridTemplateColumns: 'repeat(3, minmax(0, 1fr))',
    gap: '10px',
    flexShrink: 0,
  },
  statCard: {
    padding: '12px 16px',
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    display: 'flex',
    flexDirection: 'column',
    gap: '4px',
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
    flexShrink: 0,
  },
  pills: {
    display: 'flex',
    alignItems: 'center',
    gap: '6px',
    flexWrap: 'wrap',
    flexShrink: 0,
  },
  listCard: {
    flex: 1,
    minHeight: 0,
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    padding: '10px',
    overflowY: 'auto',
    display: 'flex',
    flexDirection: 'column',
    gap: '8px',
  },
  row: {
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

export const DriverView: React.FC<Props> = ({ lang, isAdmin }) => {
  const styles = useStyles();
  const [drivers, setDrivers] = useState<DriverPackageInfo[]>([]);
  const [totalBytes, setTotalBytes] = useState(0);
  const [outdatedCount, setOutdatedCount] = useState(0);
  const [outdatedBytes, setOutdatedBytes] = useState(0);
  const [backupDir, setBackupDir] = useState('');
  const [loading, setLoading] = useState(true);
  const [filterMode, setFilterMode] = useState<'all' | 'outdated' | string>('all');
  const [search, setSearch] = useState('');
  const deferredSearch = React.useDeferredValue(search);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [backupReport, setBackupReport] = useState<DriverBackupReport | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [confirmCleanOutdated, setConfirmCleanOutdated] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    setErrorMsg(null);
    try {
      const res = await api.getDrivers();
      setDrivers(res.drivers);
      setTotalBytes(res.total_bytes);
      setOutdatedCount(res.outdated_count);
      setOutdatedBytes(res.outdated_bytes);
      setBackupDir((previous) => previous || res.default_backup_dir);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const topClasses = useMemo(() => {
    const counts = new Map<string, number>();
    drivers.forEach((d) => {
      counts.set(d.class_name, (counts.get(d.class_name) || 0) + 1);
    });
    return Array.from(counts.entries())
      .sort((a, b) => b[1] - a[1])
      .slice(0, 6);
  }, [drivers]);

  const filtered = useMemo(() => {
    const q = deferredSearch.trim().toLowerCase();
    return drivers.filter((d) => {
      if (filterMode === 'outdated' && !d.is_outdated_duplicate) return false;
      if (filterMode !== 'all' && filterMode !== 'outdated' && d.class_name !== filterMode) {
        return false;
      }
      if (!q) return true;
      return (
        d.published_name.toLowerCase().includes(q) ||
        d.original_name.toLowerCase().includes(q) ||
        d.provider.toLowerCase().includes(q) ||
        d.class_name.toLowerCase().includes(q) ||
        d.version.toLowerCase().includes(q) ||
        d.signer_name.toLowerCase().includes(q)
      );
    });
  }, [drivers, filterMode, deferredSearch]);

  const toggleSelect = (pubName: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(pubName)) next.delete(pubName);
      else next.add(pubName);
      return next;
    });
  };

  const handleExport = async (selectedOnly: boolean) => {
    if (!backupDir.trim()) {
      setErrorMsg(lang === 'zh' ? '请填写有效的备份目标路径' : 'Please enter a valid backup path');
      return;
    }
    const targetList = selectedOnly ? Array.from(selected) : null;
    if (selectedOnly && (!targetList || targetList.length === 0)) return;
    setBusyAction('export');
    setErrorMsg(null);
    setNotice(null);
    try {
      const rep = await api.exportDrivers(backupDir.trim(), targetList);
      setBackupReport(rep);
      setNotice(
        lang === 'zh'
          ? `已成功导出 ${rep.exported_count} 个驱动程序（${formatBytes(rep.total_bytes)}）至 ${rep.target_dir}，并生成一键还原脚本 Restore-Drivers.cmd。`
          : `Exported ${rep.exported_count} driver package(s) (${formatBytes(rep.total_bytes)}) to ${rep.target_dir}.`
      );
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setBusyAction(null);
    }
  };

  const handleDeleteDrivers = async (names: string[]) => {
    if (names.length === 0) return;
    setBusyAction('delete');
    setErrorMsg(null);
    setNotice(null);
    try {
      const res = await api.deleteDrivers(names);
      setDrivers(res.drivers);
      setTotalBytes(res.total_bytes);
      setOutdatedCount(res.outdated_count);
      setOutdatedBytes(res.outdated_bytes);
      setSelected((prev) => {
        const next = new Set(prev);
        names.forEach((n) => next.delete(n));
        return next;
      });
      const okCount = res.deleted?.length ?? 0;
      const errCount = res.errors?.length ?? 0;
      setNotice(
        lang === 'zh'
          ? `已安全移除 ${okCount} 个旧版本驱动包${errCount > 0 ? `（跳过 ${errCount} 个仍被硬件占用或受保护的驱动）` : ''}。`
          : `Safely deleted ${okCount} driver package(s)${errCount > 0 ? ` (${errCount} skipped while in use)` : ''}.`
      );
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setBusyAction(null);
      setConfirmCleanOutdated(false);
    }
  };

  const outdatedNames = useMemo(
    () => drivers.filter((d) => d.is_outdated_duplicate).map((d) => d.published_name),
    [drivers]
  );

  return (
    <div className={styles.root}>
      {!isAdmin && outdatedCount > 0 && (
        <MessageBar intent="warning">
          <MessageBarBody>
            <Warning16Regular style={{ verticalAlign: 'middle', marginRight: 6 }} />
            {lang === 'zh'
              ? '提示：当前可无需提权直接扫描并一键备份驱动至本地目录；若需卸载旧驱动包，请先点击顶部“一键提权为管理员”。'
              : 'Tip: Scanning and driver backup work in standard mode; deleting DriverStore packages requires Administrator privileges.'}
          </MessageBarBody>
        </MessageBar>
      )}
      {errorMsg && (
        <MessageBar intent="error">
          <MessageBarBody>{errorMsg}</MessageBarBody>
        </MessageBar>
      )}
      {notice && (
        <MessageBar intent="success">
          <MessageBarBody>
            <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap' }}>
              <span>{notice}</span>
              {backupReport && (
                <Button
                  size="small"
                  appearance="outline"
                  icon={<FolderOpen16Regular />}
                  onClick={() => { void api.revealInExplorer(backupReport.manifest_path).catch((error) => setErrorMsg(String(error))); }}
                >
                  {lang === 'zh' ? '打开备份目录' : 'Open Backup Folder'}
                </Button>
              )}
            </div>
          </MessageBarBody>
        </MessageBar>
      )}

      <div className={styles.summaryGrid}>
        <div className={styles.statCard}>
          <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
            {lang === 'zh' ? '第三方驱动包 (oem*.inf)' : 'Third-Party Drivers'}
          </Text>
          <Text size={600} weight="bold">
            {drivers.length} <Text size={300}>({formatBytes(totalBytes)})</Text>
          </Text>
        </div>
        <div className={styles.statCard}>
          <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
            {lang === 'zh' ? '可清理重复旧版本' : 'Outdated Duplicates'}
          </Text>
          <Text
            size={600}
            weight="bold"
            style={{
              color:
                outdatedCount > 0
                  ? tokens.colorPaletteDarkOrangeForeground1
                  : tokens.colorPaletteGreenForeground1,
            }}
          >
            {outdatedCount} <Text size={300}>({formatBytes(outdatedBytes)})</Text>
          </Text>
        </div>
        <div className={styles.statCard}>
          <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
            {lang === 'zh' ? '驱动备份目标路径' : 'Backup Destination'}
          </Text>
          <Input
            size="small"
            value={backupDir}
            onChange={(_, d) => setBackupDir(d.value)}
            placeholder="C:\DismNext_DriverBackup"
          />
        </div>
      </div>

      <div className={styles.toolbar}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
          <Button
            size="small"
            appearance="primary"
            icon={busyAction === 'export' ? <Spinner size="tiny" /> : <ArrowDownloadRegular />}
            disabled={loading || Boolean(busyAction) || drivers.length === 0}
            onClick={() => handleExport(false)}
          >
            {lang === 'zh' ? '一键备份全部驱动' : 'Backup All Drivers'}
          </Button>
          <Button
            size="small"
            appearance="outline"
            icon={<ArrowDownloadRegular />}
            disabled={loading || Boolean(busyAction) || selected.size === 0}
            onClick={() => handleExport(true)}
          >
            {lang === 'zh' ? `备份勾选 (${selected.size})` : `Backup Selected (${selected.size})`}
          </Button>
          {outdatedCount > 0 && (
            <Button
              size="small"
              appearance="outline"
              icon={<Delete16Regular />}
              disabled={loading || Boolean(busyAction)}
              onClick={() => setConfirmCleanOutdated(true)}
            >
              {lang === 'zh'
                ? `一键清理过期驱动 (${outdatedCount} · ${formatBytes(outdatedBytes)})`
                : `Clean Outdated (${outdatedCount})`}
            </Button>
          )}
          <Button
            size="small"
            icon={<ArrowClockwise20Regular />}
            disabled={loading}
            onClick={load}
          >
            {lang === 'zh' ? '刷新' : 'Refresh'}
          </Button>
        </div>
        <Input
          contentBefore={<Search20Regular />}
          placeholder={lang === 'zh' ? '搜索 INF、厂商、版本号...' : 'Search INF, vendor, version...'}
          value={search}
          onChange={(_, d) => setSearch(d.value)}
          style={{ width: 240 }}
        />
      </div>

      <div className={styles.pills}>
        <Button
          size="small"
          appearance={filterMode === 'all' ? 'primary' : 'outline'}
          onClick={() => setFilterMode('all')}
        >
          {lang === 'zh' ? `全部 (${drivers.length})` : `All (${drivers.length})`}
        </Button>
        <Button
          size="small"
          appearance={filterMode === 'outdated' ? 'primary' : 'outline'}
          onClick={() => setFilterMode('outdated')}
        >
          {lang === 'zh' ? `重复旧版本 (${outdatedCount})` : `Outdated (${outdatedCount})`}
        </Button>
        {topClasses.map(([cls, count]) => (
          <Button
            key={cls}
            size="small"
            appearance={filterMode === cls ? 'primary' : 'outline'}
            onClick={() => setFilterMode(cls)}
          >
            {cls} ({count})
          </Button>
        ))}
      </div>

      <div className={styles.listCard}>
        {loading ? (
          <div style={{ display: 'flex', justifyContent: 'center', paddingTop: 48 }}>
            <Spinner label={lang === 'zh' ? '正在读取 DriverDatabase 驱动仓库...' : 'Loading DriverStore...'} />
          </div>
        ) : filtered.length === 0 ? (
          <Text size={300} style={{ color: tokens.colorNeutralForeground3, padding: 20 }}>
            {lang === 'zh' ? '当前筛选条件下没有匹配的驱动包。' : 'No matching driver packages.'}
          </Text>
        ) : (
          <VirtualList rows={filtered} getKey={(drv) => drv.published_name} label={lang === 'zh' ? '驱动包' : 'Driver packages'} renderRow={(drv) => (
            <div key={drv.repo_folder} className={styles.row}>
              <div style={{ display: 'flex', alignItems: 'center', gap: 10, minWidth: 0, flex: 1 }}>
                <Checkbox
                  aria-label={drv.published_name}
                  checked={selected.has(drv.published_name)}
                  onChange={() => toggleSelect(drv.published_name)}
                />
                <DeveloperBoardRegular style={{ fontSize: 20, color: tokens.colorBrandForeground1, flexShrink: 0 }} />
                <div style={{ display: 'flex', flexDirection: 'column', gap: 2, minWidth: 0, flex: 1 }}>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 6, flexWrap: 'wrap' }}>
                    <Text weight="semibold">{drv.original_name}</Text>
                    <Badge size="small" appearance="tint" color="brand">
                      {drv.published_name}
                    </Badge>
                    <Badge size="small" appearance="outline">
                      {drv.class_name}
                    </Badge>
                    <Badge size="small" appearance="tint" color="informative">
                      v{drv.version}
                    </Badge>
                    {drv.is_outdated_duplicate && (
                      <Badge size="small" appearance="filled" color="warning">
                        {lang === 'zh'
                          ? `旧版本 · 已被 ${drv.newer_version} 取代`
                          : `Superseded by ${drv.newer_version}`}
                      </Badge>
                    )}
                    {drv.cleanup_status === 'in_use' && (
                      <Badge size="small" appearance="outline">
                        {lang === 'zh' ? '设备正在使用' : 'In use by a device'}
                      </Badge>
                    )}
                    {drv.cleanup_status === 'unknown' && (
                      <Badge size="small" appearance="outline">
                        {lang === 'zh' ? '无法判断是否可清理' : 'Cleanup eligibility unknown'}
                      </Badge>
                    )}
                  </div>
                  <Text size={200} style={{ color: tokens.colorNeutralForeground3 }} truncate>
                    {drv.provider}
                    {drv.date ? ` · ${drv.date}` : ''}
                    {drv.signer_name ? ` · ${drv.signer_name}` : ''}
                  </Text>
                </div>
              </div>

              <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexShrink: 0 }}>
                <Text weight="semibold" style={{ minWidth: 74, textAlign: 'right' }}>
                  {formatBytes(drv.size_bytes)}
                </Text>
                <Button
                  size="small"
                  appearance="subtle"
                  icon={<FolderOpen16Regular />}
                  onClick={() => { void api.revealInExplorer(drv.repo_path).catch((error) => setErrorMsg(String(error))); }}
                  title={drv.repo_path}
                >
                  {lang === 'zh' ? '目录' : 'Folder'}
                </Button>
                {drv.is_outdated_duplicate && (
                  <Button
                    size="small"
                    appearance="outline"
                    icon={<Delete16Regular />}
                    disabled={Boolean(busyAction)}
                    onClick={() => handleDeleteDrivers([drv.published_name])}
                  >
                    {lang === 'zh' ? '卸载旧包' : 'Remove'}
                  </Button>
                )}
              </div>
            </div>
          )} />
        )}
      </div>

      <Dialog open={confirmCleanOutdated} onOpenChange={(_, d) => setConfirmCleanOutdated(d.open)}>
        <DialogSurface>
          <DialogBody>
            <DialogTitle>
              {lang === 'zh' ? '确认清理重复旧版本驱动？' : 'Confirm Outdated Driver Cleanup'}
            </DialogTitle>
            <DialogContent>
              <Text>
                {lang === 'zh'
                  ? `将通过 Windows 原生 pnputil /delete-driver 移除 ${outdatedCount} 个已被更新版本取代的历史驱动包（预计释放 ${formatBytes(outdatedBytes)}），当前正常使用的最新驱动不受影响。确定继续吗？`
                  : `Remove ${outdatedCount} superseded driver package(s) (${formatBytes(outdatedBytes)}) via pnputil?`}
              </Text>
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={() => setConfirmCleanOutdated(false)}>
                {lang === 'zh' ? '取消' : 'Cancel'}
              </Button>
              <Button
                appearance="primary"
                disabled={busyAction === 'delete'}
                onClick={() => handleDeleteDrivers(outdatedNames)}
              >
                {busyAction === 'delete'
                  ? lang === 'zh'
                    ? '清理中...'
                    : 'Removing...'
                  : lang === 'zh'
                  ? '确认清理旧驱动'
                  : 'Confirm Cleanup'}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </div>
  );
};
