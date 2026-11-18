import React, { useEffect, useState } from 'react';
import {
  Badge,
  Button,
  Spinner,
  Text,
  makeStyles,
  tokens,
} from '@fluentui/react-components';
import {
  Apps24Regular,
  ArrowClockwise20Regular,
  Broom24Regular,
  DataPie24Regular,
  DeveloperBoardRegular,
  HardDriveRegular,
  History24Regular,
  PlayRegular,
  PuzzlePieceRegular,
  Rocket24Regular,
  Settings24Regular,
  ToolboxRegular,
} from '@fluentui/react-icons';
import {
  AppxPackageInfo,
  CleanupRuleEntry,
  DriverListPayload,
  Lang,
  OptimizationGroupStatus,
  RegistrySnapshot,
  StartupItem,
  SysInfoPayload,
  SystemInfo,
  SystemTaskStatus,
  WindowsFeatureInfo,
  api,
  formatBytes,
} from '../api';

export type DashboardTargetTab =
  | 'dashboard'
  | 'cleanup'
  | 'diskmap'
  | 'drivers'
  | 'features'
  | 'optimize'
  | 'startup'
  | 'appx'
  | 'snapshots';

interface Props {
  lang: Lang;
  sysPayload: SysInfoPayload | null;
  cleanupEntries: CleanupRuleEntry[];
  optGroups: OptimizationGroupStatus[];
  snapshots: RegistrySnapshot[];
  scanning: boolean;
  onScanCleanup: () => Promise<void>;
  onNavigate: (tab: DashboardTargetTab) => void;
  onSystemUpdated: (sys: SystemInfo) => void;
}

const useStyles = makeStyles({
  root: {
    display: 'flex',
    flexDirection: 'column',
    height: '100%',
    gap: '12px',
    overflowY: 'auto',
    paddingRight: '4px',
  },
  heroCard: {
    padding: '18px 20px',
    borderRadius: tokens.borderRadiusXLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    display: 'flex',
    flexDirection: 'column',
    gap: '12px',
    flexShrink: 0,
  },
  heroTop: {
    display: 'flex',
    justifyContent: 'space-between',
    alignItems: 'center',
    flexWrap: 'wrap',
    gap: '12px',
  },
  gaugeTrack: {
    width: '100%',
    height: '12px',
    borderRadius: '999px',
    backgroundColor: tokens.colorNeutralBackground4,
    overflow: 'hidden',
    display: 'flex',
  },
  grid: {
    display: 'grid',
    gridTemplateColumns: 'repeat(3, minmax(0, 1fr))',
    gap: '12px',
  },
  card: {
    padding: '16px',
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    display: 'flex',
    flexDirection: 'column',
    justifyContent: 'space-between',
    gap: '12px',
    minHeight: '170px',
  },
  cardHeader: {
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'space-between',
    gap: '8px',
  },
  cardTitleWrap: {
    display: 'flex',
    alignItems: 'center',
    gap: '8px',
  },
  pillsWrap: {
    display: 'flex',
    flexWrap: 'wrap',
    gap: '6px',
  },
});

const KEY_FEATURES = [
  { id: 'Microsoft-Windows-Subsystem-Linux', label: 'WSL' },
  { id: 'VirtualMachinePlatform', label: 'VM Platform' },
  { id: 'HypervisorPlatform', label: 'Hypervisor' },
  { id: 'NetFx3', label: '.NET 3.5' },
  { id: 'Recall', label: 'Recall' },
  { id: 'SMB1Protocol', label: 'SMB 1.0' },
];

export const DashboardView: React.FC<Props> = ({
  lang,
  sysPayload,
  cleanupEntries,
  optGroups,
  snapshots,
  scanning,
  onScanCleanup,
  onNavigate,
  onSystemUpdated,
}) => {
  const styles = useStyles();
  const [loadingStats, setLoadingStats] = useState(true);
  const [driverPayload, setDriverPayload] = useState<DriverListPayload | null>(null);
  const [features, setFeatures] = useState<WindowsFeatureInfo[]>([]);
  const [startupItems, setStartupItems] = useState<StartupItem[]>([]);
  const [appxPackages, setAppxPackages] = useState<AppxPackageInfo[]>([]);
  const [sysTaskStatus, setSysTaskStatus] = useState<SystemTaskStatus | null>(null);
  const [taskRunning, setTaskRunning] = useState(false);
  const [taskFeedback, setTaskFeedback] = useState<string | null>(null);

  const loadMetrics = async () => {
    setLoadingStats(true);
    try {
      const [drvRes, featRes, stRes, appxRes, taskRes] = await Promise.allSettled([
        api.getDrivers(),
        api.getFeatures(),
        api.getStartupItems(),
        api.getAppxPackages(),
        api.getSysTaskStatus(),
      ]);
      if (drvRes.status === 'fulfilled') setDriverPayload(drvRes.value);
      if (featRes.status === 'fulfilled') setFeatures(featRes.value.features);
      if (stRes.status === 'fulfilled') setStartupItems(stRes.value.items);
      if (appxRes.status === 'fulfilled') setAppxPackages(appxRes.value.packages);
      if (taskRes.status === 'fulfilled') setSysTaskStatus(taskRes.value);
    } finally {
      setLoadingStats(false);
    }
  };

  useEffect(() => {
    loadMetrics();
  }, []);

  const handleRunComponentCleanup = async () => {
    setTaskRunning(true);
    setTaskFeedback(null);
    try {
      const { outcome, system } = await api.runSysTask('component_cleanup');
      setSysTaskStatus(outcome.status);
      onSystemUpdated(system);
      setTaskFeedback(outcome.output);
    } catch (e) {
      setTaskFeedback(String(e));
    } finally {
      setTaskRunning(false);
    }
  };

  const disk = sysPayload?.system.system_disk;
  const scannedCount = cleanupEntries.filter((e) => e.scanned).length;
  const reclaimableBytes = cleanupEntries.reduce((a, e) => a + e.rule.cleanable_bytes, 0);
  const usedPct = disk && disk.total_bytes > 0 ? (disk.used_bytes / disk.total_bytes) * 100 : 0;
  const reclaimPct =
    disk && disk.total_bytes > 0
      ? Math.min(usedPct, (reclaimableBytes / disk.total_bytes) * 100)
      : 0;

  const enabledStartups = startupItems.filter((s) => s.enabled).length;
  const bloatwareCount = appxPackages.filter((p) => p.category === 'bloatware').length;
  const totalOptItems = optGroups.reduce((a, g) => a + g.items.length, 0);
  const featureByName = new Map(features.map((f) => [f.name, f]));

  return (
    <div className={styles.root}>
      <div className={styles.heroCard}>
        <div className={styles.heroTop}>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
            <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
              <HardDriveRegular style={{ fontSize: 24, color: tokens.colorBrandForeground1 }} />
              <Text size={500} weight="bold">
                {lang === 'zh' ? '系统健康总览仪表盘' : 'System Maintenance Dashboard'}
              </Text>
              {loadingStats && <Spinner size="tiny" />}
            </div>
            <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
              {disk
                ? lang === 'zh'
                  ? `系统盘 ${disk.root} · 总容量 ${formatBytes(disk.total_bytes)} · 已用 ${formatBytes(disk.used_bytes)} (${usedPct.toFixed(1)}%) · 剩余 ${formatBytes(disk.free_bytes)}`
                  : `System Drive ${disk.root} · ${formatBytes(disk.used_bytes)} used (${usedPct.toFixed(1)}%) · ${formatBytes(disk.free_bytes)} free of ${formatBytes(disk.total_bytes)}`
                : 'Loading storage info...'}
            </Text>
          </div>

          <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
            <Button
              appearance="primary"
              icon={scanning ? <Spinner size="tiny" /> : <Broom24Regular />}
              disabled={scanning}
              onClick={async () => {
                await onScanCleanup();
                onNavigate('cleanup');
              }}
            >
              {scanning
                ? lang === 'zh'
                  ? '正在扫描...'
                  : 'Scanning...'
                : lang === 'zh'
                ? '一键全盘体检扫描'
                : 'Scan Reclaimable Space'}
            </Button>
            <Button
              appearance="outline"
              icon={<DataPie24Regular />}
              onClick={() => onNavigate('diskmap')}
            >
              {lang === 'zh' ? '空间分布分析' : 'Space Analyzer'}
            </Button>
            <Button
              appearance="subtle"
              icon={<ArrowClockwise20Regular />}
              disabled={loadingStats}
              onClick={loadMetrics}
            >
              {lang === 'zh' ? '刷新指标' : 'Refresh'}
            </Button>
          </div>
        </div>

        {disk && (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
            <div className={styles.gaugeTrack}>
              <div
                style={{
                  width: `${Math.max(0, usedPct - reclaimPct)}%`,
                  backgroundColor: tokens.colorBrandBackground,
                }}
              />
              <div
                style={{
                  width: `${reclaimPct}%`,
                  backgroundColor: tokens.colorPaletteGreenBackground3,
                }}
              />
            </div>
            <div style={{ display: 'flex', justifyContent: 'space-between' }}>
              <Text size={100} style={{ color: tokens.colorNeutralForeground3 }}>
                {lang === 'zh'
                  ? `蓝色：系统及用户数据 · 绿色：已扫描可安全回收空间 (${formatBytes(reclaimableBytes)})`
                  : `Blue: Used Data · Green: Scanned Reclaimable (${formatBytes(reclaimableBytes)})`}
              </Text>
              <Text size={100} style={{ color: tokens.colorNeutralForeground3 }}>
                {sysPayload?.system.product_name} {sysPayload?.system.display_version}
              </Text>
            </div>
          </div>
        )}
      </div>

      <div className={styles.grid}>
        {/* Card 1: Space Recycle */}
        <div className={styles.card}>
          <div>
            <div className={styles.cardHeader}>
              <div className={styles.cardTitleWrap}>
                <Broom24Regular style={{ color: tokens.colorBrandForeground1 }} />
                <Text size={400} weight="semibold">
                  {lang === 'zh' ? '空间回收' : 'Space Recycle'}
                </Text>
              </div>
              <Badge appearance="tint" color={reclaimableBytes > 0 ? 'success' : 'subtle'}>
                {scannedCount > 0
                  ? formatBytes(reclaimableBytes)
                  : lang === 'zh'
                  ? '待扫描'
                  : 'Pending'}
              </Badge>
            </div>
            <div style={{ marginTop: 10, display: 'flex', flexDirection: 'column', gap: 4 }}>
              <Text size={600} weight="bold">
                {cleanupEntries.length}{' '}
                <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                  {lang === 'zh' ? '条适用规则' : 'applicable rules'}
                </Text>
              </Text>
              <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                {sysTaskStatus
                  ? lang === 'zh'
                    ? `Installer 孤立安装包：${sysTaskStatus.orphaned_installer_count} 个 (${formatBytes(sysTaskStatus.orphaned_installer_bytes)})`
                    : `Orphaned MSI/MSP caches: ${sysTaskStatus.orphaned_installer_count} (${formatBytes(sysTaskStatus.orphaned_installer_bytes)})`
                  : lang === 'zh'
                  ? '支持 Dry-Run 模拟与只读锁定预判'
                  : 'Supports Dry-Run diagnostics'}
              </Text>
            </div>
          </div>
          <Button appearance="outline" onClick={() => onNavigate('cleanup')}>
            {lang === 'zh' ? '进入空间回收' : 'Open Space Recycle'}
          </Button>
        </div>

        {/* Card 2: Driver Manager & Backup */}
        <div className={styles.card}>
          <div>
            <div className={styles.cardHeader}>
              <div className={styles.cardTitleWrap}>
                <DeveloperBoardRegular style={{ fontSize: 22, color: tokens.colorBrandForeground1 }} />
                <Text size={400} weight="semibold">
                  {lang === 'zh' ? '驱动管理与备份' : 'Driver Manager'}
                </Text>
              </div>
              {driverPayload && driverPayload.outdated_count > 0 && (
                <Badge appearance="tint" color="warning">
                  {driverPayload.outdated_count}{' '}
                  {lang === 'zh' ? '个可清理旧驱动' : 'outdated'}
                </Badge>
              )}
            </div>
            <div style={{ marginTop: 10, display: 'flex', flexDirection: 'column', gap: 4 }}>
              <Text size={600} weight="bold">
                {driverPayload ? driverPayload.drivers.length : '—'}{' '}
                <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                  {driverPayload
                    ? `${lang === 'zh' ? '个第三方驱动' : 'oem drivers'} (${formatBytes(driverPayload.total_bytes)})`
                    : ''}
                </Text>
              </Text>
              <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                {driverPayload
                  ? lang === 'zh'
                    ? `检测到 ${driverPayload.outdated_count} 个历史旧版本 (${formatBytes(driverPayload.outdated_bytes)})，支持一键备份整包`
                    : `${driverPayload.outdated_count} outdated duplicates (${formatBytes(driverPayload.outdated_bytes)})`
                  : 'Loading DriverStore...'}
              </Text>
            </div>
          </div>
          <Button appearance="outline" onClick={() => onNavigate('drivers')}>
            {lang === 'zh' ? '驱动备份与去重清理' : 'Manage & Backup Drivers'}
          </Button>
        </div>

        {/* Card 3: Startup & AppX */}
        <div className={styles.card}>
          <div>
            <div className={styles.cardHeader}>
              <div className={styles.cardTitleWrap}>
                <Rocket24Regular style={{ color: tokens.colorBrandForeground1 }} />
                <Text size={400} weight="semibold">
                  {lang === 'zh' ? '启动项与预装应用' : 'Startup & AppX'}
                </Text>
              </div>
              <Badge appearance="tint" color={bloatwareCount > 0 ? 'warning' : 'success'}>
                {bloatwareCount} {lang === 'zh' ? '个可精简预装' : 'bloatware'}
              </Badge>
            </div>
            <div style={{ marginTop: 10, display: 'flex', flexDirection: 'column', gap: 4 }}>
              <Text size={600} weight="bold">
                {enabledStartups}/{startupItems.length}{' '}
                <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                  {lang === 'zh' ? '开机自启项启用中' : 'startup items enabled'}
                </Text>
              </Text>
              <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                {lang === 'zh'
                  ? `已识别 ${appxPackages.length} 个 UWP/MSIX 应用（含系统核心保护锁）`
                  : `${appxPackages.length} AppX packages detected`}
              </Text>
            </div>
          </div>
          <div style={{ display: 'flex', gap: 8 }}>
            <Button style={{ flex: 1 }} appearance="outline" onClick={() => onNavigate('startup')}>
              {lang === 'zh' ? '启动项管理' : 'Startup'}
            </Button>
            <Button
              style={{ flex: 1 }}
              appearance="outline"
              icon={<Apps24Regular />}
              onClick={() => onNavigate('appx')}
            >
              {lang === 'zh' ? '预装应用' : 'AppX'}
            </Button>
          </div>
        </div>

        {/* Card 4: Windows Optional Features */}
        <div className={styles.card}>
          <div>
            <div className={styles.cardHeader}>
              <div className={styles.cardTitleWrap}>
                <PuzzlePieceRegular style={{ fontSize: 22, color: tokens.colorBrandForeground1 }} />
                <Text size={400} weight="semibold">
                  {lang === 'zh' ? 'Windows 可选功能' : 'Optional Features'}
                </Text>
              </div>
              <Badge appearance="tint" color="brand">
                {features.filter((f) => f.enabled).length}/{features.length}
              </Badge>
            </div>
            <div className={styles.pillsWrap} style={{ marginTop: 10 }}>
              {KEY_FEATURES.map((kf) => {
                const info = featureByName.get(kf.id);
                const enabled = Boolean(info?.enabled);
                return (
                  <Badge
                    key={kf.id}
                    size="small"
                    appearance={enabled ? 'filled' : 'outline'}
                    color={enabled ? 'success' : 'subtle'}
                  >
                    {kf.label}: {enabled ? 'ON' : 'OFF'}
                  </Badge>
                );
              })}
            </div>
          </div>
          <Button appearance="outline" onClick={() => onNavigate('features')}>
            {lang === 'zh' ? '开关 WSL / Hyper-V / .NET 3.5' : 'Configure Windows Features'}
          </Button>
        </div>

        {/* Card 5: System Tasks & CompactOS */}
        <div className={styles.card}>
          <div>
            <div className={styles.cardHeader}>
              <div className={styles.cardTitleWrap}>
                <ToolboxRegular style={{ fontSize: 22, color: tokens.colorBrandForeground1 }} />
                <Text size={400} weight="semibold">
                  {lang === 'zh' ? '系统级重型维护' : 'System Tasks & CompactOS'}
                </Text>
              </div>
              {sysTaskStatus && (
                <Badge
                  appearance="tint"
                  color={sysTaskStatus.compact_os_enabled ? 'success' : 'subtle'}
                >
                  CompactOS:{' '}
                  {sysTaskStatus.compact_os_enabled
                    ? lang === 'zh'
                      ? '已压缩'
                      : 'Active'
                    : lang === 'zh'
                    ? '未开启'
                    : 'Off'}
                </Badge>
              )}
            </div>
            <div style={{ marginTop: 10, display: 'flex', flexDirection: 'column', gap: 6 }}>
              <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                {lang === 'zh'
                  ? '直接调用 Windows 原生 DISM 组件清理、CompactOS 与 VSS 卷影复制清理。'
                  : 'Run native DISM component store cleanup, CompactOS, or VSS shadow copy cleanup.'}
              </Text>
              {taskFeedback && (
                <Text
                  size={100}
                  style={{ fontFamily: 'Consolas, monospace', color: tokens.colorBrandForeground1 }}
                >
                  {taskFeedback}
                </Text>
              )}
            </div>
          </div>
          <Button
            appearance="outline"
            icon={taskRunning ? <Spinner size="tiny" /> : <PlayRegular />}
            disabled={taskRunning}
            onClick={handleRunComponentCleanup}
          >
            {lang === 'zh' ? '执行 WinSxS 组件深度清理' : 'Run DISM /StartComponentCleanup'}
          </Button>
        </div>

        {/* Card 6: Optimization & Snapshot Rollback */}
        <div className={styles.card}>
          <div>
            <div className={styles.cardHeader}>
              <div className={styles.cardTitleWrap}>
                <Settings24Regular style={{ color: tokens.colorBrandForeground1 }} />
                <Text size={400} weight="semibold">
                  {lang === 'zh' ? '系统优化与快照回滚' : 'Tweaks & Snapshots'}
                </Text>
              </div>
              <Badge appearance="tint" color="informative">
                {snapshots.length} {lang === 'zh' ? '份快照' : 'snapshots'}
              </Badge>
            </div>
            <div style={{ marginTop: 10, display: 'flex', flexDirection: 'column', gap: 4 }}>
              <Text size={600} weight="bold">
                {totalOptItems}{' '}
                <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                  {lang === 'zh' ? '项系统优化开关' : 'optimization controls'}
                </Text>
              </Text>
              <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                {lang === 'zh'
                  ? '包含 Win11 26H2/25H2 现代特性；每次修改前自动落盘注册表原始快照。'
                  : 'Includes Win11 modern tweaks with automatic pre-edit registry snapshots.'}
              </Text>
            </div>
          </div>
          <div style={{ display: 'flex', gap: 8 }}>
            <Button style={{ flex: 1 }} appearance="outline" onClick={() => onNavigate('optimize')}>
              {lang === 'zh' ? '系统优化' : 'Optimization'}
            </Button>
            <Button
              style={{ flex: 1 }}
              appearance="outline"
              icon={<History24Regular />}
              onClick={() => onNavigate('snapshots')}
            >
              {lang === 'zh' ? '快照回滚' : 'Snapshots'}
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
};
