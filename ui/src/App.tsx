import React, { Suspense, lazy, useCallback, useEffect, useRef, useState } from 'react';
import {
  Badge,
  Button,
  FluentProvider,
  Spinner,
  Text,
  makeStyles,
  tokens,
  webDarkTheme,
  webLightTheme,
} from '@fluentui/react-components';
import {
  Apps24Regular,
  Broom24Regular,
  DataPie24Regular,
  DeveloperBoardRegular,
  History24Regular,
  HomeRegular,
  PuzzlePieceRegular,
  Rocket24Regular,
  Settings24Regular,
  ShieldCheckmark24Regular,
  ToolboxRegular,
  WeatherMoon24Regular,
  WeatherSunny24Regular,
} from '@fluentui/react-icons';
import {
  Lang,
  api,
  formatBytes,
} from './api';
import { DashboardTargetTab, DashboardView } from './views/DashboardView';
import { ScanStatusBar } from './ScanStatusBar';
import { MaintenanceProvider, MaintenanceStatusBar } from './MaintenanceJobs';
import { useScanJob } from './useScanJob';

const SpaceRecycleView = lazy(() =>
  import('./views/SpaceRecycleView').then((m) => ({ default: m.SpaceRecycleView }))
);
const OptimizationView = lazy(() =>
  import('./views/OptimizationView').then((m) => ({ default: m.OptimizationView }))
);
const SnapshotView = lazy(() =>
  import('./views/SnapshotView').then((m) => ({ default: m.SnapshotView }))
);
const DiskAnalyzerView = lazy(() =>
  import('./views/DiskAnalyzerView').then((m) => ({ default: m.DiskAnalyzerView }))
);
const StartupView = lazy(() =>
  import('./views/StartupView').then((m) => ({ default: m.StartupView }))
);
const AppxView = lazy(() =>
  import('./views/AppxView').then((m) => ({ default: m.AppxView }))
);
const DriverView = lazy(() =>
  import('./views/DriverView').then((m) => ({ default: m.DriverView }))
);
const FeaturesView = lazy(() =>
  import('./views/FeaturesView').then((m) => ({ default: m.FeaturesView }))
);
const RescueView = lazy(() =>
  import('./views/RescueView').then((m) => ({ default: m.RescueView }))
);

type NavTab = DashboardTargetTab;

const useStyles = makeStyles({
  shell: {
    display: 'flex',
    height: '100vh',
    width: '100vw',
    backgroundColor: tokens.colorNeutralBackground1,
    color: tokens.colorNeutralForeground1,
    overflow: 'hidden',
  },
  sidebar: {
    width: '220px',
    padding: '16px 12px',
    display: 'flex',
    flexDirection: 'column',
    justifyContent: 'space-between',
    backgroundColor: tokens.colorNeutralBackground2,
    borderRight: `1px solid ${tokens.colorNeutralStroke2}`,
    flexShrink: 0,
  },
  brand: {
    display: 'flex',
    flexDirection: 'column',
    gap: '4px',
    padding: '4px 8px 16px 8px',
  },
  navStack: {
    display: 'flex',
    flexDirection: 'column',
    gap: '6px',
  },
  navBtn: {
    justifyContent: 'flex-start',
    height: '40px',
  },
  mainCol: {
    flex: 1,
    display: 'flex',
    flexDirection: 'column',
    minWidth: 0,
    padding: '16px',
    gap: '12px',
    overflow: 'hidden',
  },
  topBanner: {
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'space-between',
    gap: '16px',
    padding: '12px 18px',
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    flexShrink: 0,
  },
  diskBarTrack: {
    width: '280px',
    height: '10px',
    borderRadius: '999px',
    backgroundColor: tokens.colorNeutralBackground4,
    overflow: 'hidden',
    display: 'flex',
  },
  contentArea: {
    flex: 1,
    minHeight: 0,
    overflow: 'hidden',
  },
});

const LANGUAGE_KEY = 'dismnext-language';
const THEME_KEY = 'dismnext-theme';

function storedPreference(key: string): string | null {
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

function savePreference(key: string, value: string) {
  try {
    window.localStorage.setItem(key, value);
  } catch {
    // The preference still applies for the current window.
  }
}

const fetchOptimizerGroups = async () => (await api.getOptimizerGroups()).groups;
const fetchSnapshots = async () => (await api.getSnapshots()).snapshots;

function useResource<T>(fetchData: () => Promise<T>, enabled = true) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const pending = useRef<Promise<void> | null>(null);

  const reload = useCallback(() => {
    if (pending.current) return pending.current;
    setError(null);
    const request = fetchData()
      .then(setData)
      .catch((failure: unknown) => setError(String(failure)))
      .finally(() => { pending.current = null; });
    pending.current = request;
    return request;
  }, [fetchData]);

  useEffect(() => {
    if (enabled) void reload();
  }, [enabled, reload]);

  return { data, setData, error, reload };
}

const AppShell: React.FC<{
  dark: boolean;
  onToggleTheme: () => void;
}> = ({ dark, onToggleTheme }) => {
  const styles = useStyles();
  const [lang, setLang] = useState<Lang>(() => storedPreference(LANGUAGE_KEY) === 'en' ? 'en' : 'zh');
  const [tab, setTab] = useState<NavTab>('dashboard');
  const [visitedCleanup, setVisitedCleanup] = useState(false);
  const [visitedDiskmap, setVisitedDiskmap] = useState(false);
  const navigate = useCallback((next: NavTab) => {
    if (next === 'cleanup') setVisitedCleanup(true);
    if (next === 'diskmap') setVisitedDiskmap(true);
    setTab(next);
  }, []);
  const toggleLanguage = () => {
    const next = lang === 'zh' ? 'en' : 'zh';
    savePreference(LANGUAGE_KEY, next);
    setLang(next);
  };
  const system = useResource(api.getSysInfo);
  const cleanup = useResource(api.getCleanupRules);
  const optimizer = useResource(fetchOptimizerGroups, tab === 'optimize');
  const snapshotList = useResource(fetchSnapshots);
  const { data: sysPayload, setData: setSysPayload } = system;
  const { data: cleanupData, setData: setCleanupData } = cleanup;
  const cleanupEntries = cleanupData?.items ?? null;
  const cleanupSummary = cleanupData?.summary ?? null;
  const { data: optGroups, setData: setOptGroups } = optimizer;
  const { data: snapshots, setData: setSnapshots, reload: refreshSnapshots } = snapshotList;
  const cleanupScan = useScanJob();
  const scanning = Boolean(cleanupScan.active);
  const [scanError, setScanError] = useState<string | null>(null);
  const [elevating, setElevating] = useState(false);
  const [elevationRelaunching, setElevationRelaunching] = useState(false);
  const [elevateErr, setElevateErr] = useState<string | null>(null);

  const handleElevate = async () => {
    setElevating(true);
    setElevateErr(null);
    try {
      const res = await api.elevate();
      if (res.relaunching) {
        setElevationRelaunching(true);
        return;
      }
      setSysPayload(await api.getSysInfo());
    } catch (e) {
      setElevateErr(String(e));
    } finally {
      setElevating(false);
    }
  };

  const handleScan = async () => {
    if (scanning) return;
    setScanError(null);
    try {
      const res = await cleanupScan.run({ kind: 'cleanup_rules' });
      if (res) setCleanupData({ items: res.items.map((rule) => ({ scanned: true, rule })), summary: res.summary });
    } catch (error) {
      setScanError(String(error));
    }
  };

  const reclaimableBytes = cleanupSummary?.cleanable_bytes ?? 0;

  const disk = sysPayload?.system.system_disk;
  const usedPct = disk && disk.total_bytes > 0 ? (disk.used_bytes / disk.total_bytes) * 100 : 0;
  const reclaimPct =
    disk && disk.total_bytes > 0
      ? Math.min(usedPct, (reclaimableBytes / disk.total_bytes) * 100)
      : 0;

  const resourceFeedback = (label: string, error: string | null, retry: () => Promise<void>) => (
    error ? (
      <div role="alert" style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
        <Text style={{ color: tokens.colorPaletteRedForeground1 }}>{label} · {error}</Text>
        <Button size="small" onClick={() => void retry()}>{lang === 'zh' ? '重试' : 'Retry'}</Button>
      </div>
    ) : <Spinner size="small" label={label} />
  );

  const cleanupFeedback = (error: string | null) => resourceFeedback(
    lang === 'zh' ? '加载清理规则' : 'Load cleanup rules', error, cleanup.reload,
  );
  const snapshotFeedback = (error: string | null) => resourceFeedback(
    lang === 'zh' ? '加载快照' : 'Load snapshots', error, refreshSnapshots,
  );

  const viewLoading = (
    <div style={{ display: 'flex', justifyContent: 'center', paddingTop: 60 }}>
      <Spinner label={lang === 'zh' ? '正在加载视图...' : 'Loading view...'} />
    </div>
  );

  return (
    <div className={styles.shell}>
      <aside className={styles.sidebar}>
        <div>
          <div className={styles.brand}>
            <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <ShieldCheckmark24Regular style={{ color: tokens.colorBrandForeground1 }} />
              <Text size={500} weight="bold">
                DismNext
              </Text>
            </div>
            <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
              {lang === 'zh' ? '现代 Windows 清理与优化' : 'Modern System Maintenance'}
            </Text>
          </div>

          <div className={styles.navStack}>
            <Button
              className={styles.navBtn}
              appearance={tab === 'dashboard' ? 'primary' : 'subtle'}
              icon={<HomeRegular style={{ fontSize: 22 }} />}
              onClick={() => navigate('dashboard')}
            >
              {lang === 'zh' ? '系统概览' : 'Dashboard'}
            </Button>
            <Button
              className={styles.navBtn}
              appearance={tab === 'cleanup' ? 'primary' : 'subtle'}
              icon={<Broom24Regular />}
              onClick={() => navigate('cleanup')}
            >
              {lang === 'zh' ? '空间回收' : 'Space Recycle'}
            </Button>
            <Button
              className={styles.navBtn}
              appearance={tab === 'diskmap' ? 'primary' : 'subtle'}
              icon={<DataPie24Regular />}
              onClick={() => navigate('diskmap')}
            >
              {lang === 'zh' ? '空间分析' : 'Space Analyzer'}
            </Button>
            <Button
              className={styles.navBtn}
              appearance={tab === 'drivers' ? 'primary' : 'subtle'}
              icon={<DeveloperBoardRegular style={{ fontSize: 22 }} />}
              onClick={() => navigate('drivers')}
            >
              {lang === 'zh' ? '驱动管理' : 'Driver Manager'}
            </Button>
            <Button
              className={styles.navBtn}
              appearance={tab === 'features' ? 'primary' : 'subtle'}
              icon={<PuzzlePieceRegular style={{ fontSize: 22 }} />}
              onClick={() => navigate('features')}
            >
              {lang === 'zh' ? '可选功能' : 'Optional Features'}
            </Button>
            <Button
              className={styles.navBtn}
              appearance={tab === 'optimize' ? 'primary' : 'subtle'}
              icon={<Settings24Regular />}
              onClick={() => navigate('optimize')}
            >
              {lang === 'zh' ? '系统优化' : 'Optimization'}
            </Button>
            <Button
              className={styles.navBtn}
              appearance={tab === 'startup' ? 'primary' : 'subtle'}
              icon={<Rocket24Regular />}
              onClick={() => navigate('startup')}
            >
              {lang === 'zh' ? '开机启动项' : 'Startup'}
            </Button>
            <Button
              className={styles.navBtn}
              appearance={tab === 'appx' ? 'primary' : 'subtle'}
              icon={<Apps24Regular />}
              onClick={() => navigate('appx')}
            >
              {lang === 'zh' ? '应用卸载管理' : 'App Uninstaller'}
            </Button>
            <Button
              className={styles.navBtn}
              appearance={tab === 'rescue' ? 'primary' : 'subtle'}
              icon={<ToolboxRegular style={{ fontSize: 22 }} />}
              onClick={() => navigate('rescue')}
            >
              {lang === 'zh' ? '系统急救箱' : 'Rescue Center'}
            </Button>
            <Button
              className={styles.navBtn}
              appearance={tab === 'snapshots' ? 'primary' : 'subtle'}
              icon={<History24Regular />}
              onClick={() => navigate('snapshots')}
            >
              {lang === 'zh' ? '快照回滚' : 'Snapshots'}{snapshots && ` (${snapshots.length})`}
            </Button>
          </div>
        </div>

        <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
          <Button
            appearance="outline"
            icon={dark ? <WeatherSunny24Regular /> : <WeatherMoon24Regular />}
            onClick={onToggleTheme}
          >
            {dark
              ? lang === 'zh'
                ? '浅色主题'
                : 'Light Mode'
              : lang === 'zh'
              ? '深色主题'
              : 'Dark Mode'}
          </Button>
          <Button
            appearance="outline"
            onClick={toggleLanguage}
          >
            {lang === 'zh' ? 'English UI' : '简体中文界面'}
          </Button>
        </div>
      </aside>

      <main className={styles.mainCol}>
        <header className={styles.topBanner}>
          {sysPayload ? (
            <>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
                  <Text size={400} weight="bold">
                    {sysPayload.system.product_name} {sysPayload.system.display_version}
                  </Text>
                  <Badge appearance="tint" color="brand">
                    Build {sysPayload.system.build}.{sysPayload.system.ubr}
                  </Badge>
                  <Badge appearance="outline">{sysPayload.system.arch}</Badge>
                  <Badge
                    appearance="tint"
                    color={sysPayload.system.is_admin ? 'success' : 'warning'}
                  >
                    {sysPayload.system.is_admin
                      ? lang === 'zh'
                        ? '管理员权限'
                        : 'Administrator'
                      : lang === 'zh'
                      ? '标准用户（部分系统项只读）'
                      : 'Standard User'}
                  </Badge>
                  {!sysPayload.system.is_admin && (
                    <Button
                      size="small"
                      appearance="primary"
                      disabled={elevating || elevationRelaunching}
                      onClick={handleElevate}
                    >
                      {elevating
                        ? lang === 'zh'
                          ? '正在提权重启...'
                          : 'Elevating...'
                        : lang === 'zh'
                        ? '一键提权为管理员'
                        : 'Elevate to Admin'}
                    </Button>
                  )}
                  {elevationRelaunching && (
                    <Text size={100}>
                      {lang === 'zh' ? '请在新打开的管理员窗口中继续。' : 'Continue in the new administrator window.'}
                    </Text>
                  )}
                  {elevateErr && (
                    <Text size={100} style={{ color: tokens.colorPaletteRedForeground1 }}>
                      {elevateErr}
                    </Text>
                  )}
                </div>
                <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                  {lang === 'zh'
                    ? `规则库基线 ${sysPayload.catalog.base_version} · 已启用 ${cleanupEntries?.length ?? '—'} 条空间规则 · ${sysPayload.catalog.optimization_items_total} 项系统优化`
                    : `Catalog ${sysPayload.catalog.base_version} · ${cleanupEntries?.length ?? '—'} cleanup rules · ${sysPayload.catalog.optimization_items_total} tweaks`}
                </Text>
              </div>

              {disk && (
                <div style={{ display: 'flex', flexDirection: 'column', gap: 4, alignItems: 'flex-end' }}>
                  <Text size={200} weight="semibold">
                    {disk.root} {formatBytes(disk.free_bytes)}{' '}
                    {lang === 'zh' ? '可用 / 共' : 'free of'} {formatBytes(disk.total_bytes)}
                    {reclaimableBytes > 0 &&
                      ` (${lang === 'zh' ? '可释放' : 'reclaimable'} ${formatBytes(reclaimableBytes)})`}
                  </Text>
                  <div className={styles.diskBarTrack}>
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
                </div>
              )}
            </>
          ) : (
            resourceFeedback(lang === 'zh' ? '加载系统信息' : 'Load system info', system.error, system.reload)
          )}
        </header>

        <section className={styles.contentArea}>
          <MaintenanceStatusBar lang={lang} />
          {(scanning || tab === 'dashboard' || tab === 'cleanup') && (
            <ScanStatusBar active={cleanupScan.active} cancelled={cleanupScan.cancelled} cancel={cleanupScan.cancel} lang={lang} />
          )}
          {(tab === 'dashboard' || tab === 'cleanup') && scanError && resourceFeedback(
            lang === 'zh' ? '扫描清理规则' : 'Scan cleanup rules', scanError, handleScan,
          )}
          {(tab === 'dashboard' || (tab === 'cleanup' && cleanupEntries !== null)) && cleanup.error && cleanupFeedback(cleanup.error)}
          {(tab === 'dashboard' || (tab === 'snapshots' && snapshots !== null)) && snapshotList.error && snapshotFeedback(snapshotList.error)}
          {tab === 'optimize' && optGroups !== null && optimizer.error && resourceFeedback(
            lang === 'zh' ? '加载系统优化项' : 'Load optimization controls', optimizer.error, optimizer.reload,
          )}
          {visitedCleanup && cleanupEntries !== null && (
            <div hidden={tab !== 'cleanup'} style={{ height: '100%' }}>
              <Suspense fallback={viewLoading}>
              <SpaceRecycleView
                visible={tab === 'cleanup'}
                lang={lang}
                entries={cleanupEntries}
                summary={cleanupSummary}
                scanning={scanning}
                onScan={handleScan}
                onEntriesUpdated={(items) => setCleanupData({ items, summary: null })}
                onExecuted={async (sys) => {
                  setSysPayload((prev) => (prev ? { ...prev, system: sys } : prev));
                  setCleanupData((prev) => prev && { ...prev, summary: null });
                  await handleScan();
                }}
                onSystemTaskDone={async (sys) => {
                  setSysPayload((prev) => (prev ? { ...prev, system: sys } : prev));
                  setCleanupData((prev) => prev && { ...prev, summary: null });
                  await handleScan();
                }}
              />
              </Suspense>
            </div>
          )}
          {visitedDiskmap && (
            <div hidden={tab !== 'diskmap'} style={{ height: '100%' }}>
              <Suspense fallback={viewLoading}>
              <DiskAnalyzerView
                visible={tab === 'diskmap'}
                lang={lang}
                onSystemUpdated={(sys) =>
                  setSysPayload((prev) => (prev ? { ...prev, system: sys } : prev))
                }
              />
              </Suspense>
            </div>
          )}
          <Suspense fallback={viewLoading}>
            {tab === 'dashboard' ? (
              <DashboardView
                lang={lang}
                sysPayload={sysPayload}
                cleanupEntries={cleanupEntries}
                reclaimableBytes={cleanupSummary?.cleanable_bytes ?? null}
                snapshotCount={snapshots?.length ?? null}
                scanning={scanning}
                onScanCleanup={handleScan}
                onNavigate={navigate}
                onSystemUpdated={(sys) =>
                  setSysPayload((prev) => (prev ? { ...prev, system: sys } : prev))
                }
              />
            ) : tab === 'cleanup' ? (cleanupEntries === null ? cleanupFeedback(cleanup.error) : null
            ) : tab === 'diskmap' ? null : tab === 'drivers' ? (
              <DriverView lang={lang} isAdmin={Boolean(sysPayload?.system.is_admin)} />
            ) : tab === 'features' ? (
              <FeaturesView lang={lang} isAdmin={Boolean(sysPayload?.system.is_admin)} />
            ) : tab === 'startup' ? (
              <StartupView lang={lang} isAdmin={Boolean(sysPayload?.system.is_admin)} />
            ) : tab === 'appx' ? (
              <AppxView lang={lang} />
            ) : tab === 'rescue' ? (
              <RescueView lang={lang} isAdmin={Boolean(sysPayload?.system.is_admin)} />
            ) : tab === 'optimize' ? optGroups === null ? resourceFeedback(
              lang === 'zh' ? '加载系统优化项' : 'Load optimization controls', optimizer.error, optimizer.reload,
            ) : (
              <OptimizationView
                lang={lang}
                groups={optGroups}
                onGroupsUpdated={(next) => {
                  setOptGroups(next);
                  void refreshSnapshots();
                }}
                onSnapshotCreated={(snap) => setSnapshots((prev) => prev && [snap, ...prev.filter((item) => item.id !== snap.id)])}
                onJumpToSnapshots={() => navigate('snapshots')}
              />
            ) : snapshots === null ? snapshotFeedback(snapshotList.error) : (
              <SnapshotView
                lang={lang}
                snapshots={snapshots}
                onRefresh={refreshSnapshots}
                onGroupsUpdated={setOptGroups}
              />
            )}
          </Suspense>
        </section>
      </main>
    </div>
  );
};

export default function App() {
  const [theme, setTheme] = useState<'dark' | 'light' | null>(() => {
    const saved = storedPreference(THEME_KEY);
    return saved === 'dark' || saved === 'light' ? saved : null;
  });
  const [systemDark, setSystemDark] = useState(() => window.matchMedia('(prefers-color-scheme: dark)').matches);
  useEffect(() => {
    const query = window.matchMedia('(prefers-color-scheme: dark)');
    const onChange = (event: MediaQueryListEvent) => setSystemDark(event.matches);
    query.addEventListener('change', onChange);
    return () => query.removeEventListener('change', onChange);
  }, []);
  const dark = theme === null ? systemDark : theme === 'dark';
  const toggleTheme = () => {
    const next = dark ? 'light' : 'dark';
    savePreference(THEME_KEY, next);
    setTheme(next);
  };
  return (
    <FluentProvider theme={dark ? webDarkTheme : webLightTheme}>
      <MaintenanceProvider>
        <AppShell dark={dark} onToggleTheme={toggleTheme} />
      </MaintenanceProvider>
    </FluentProvider>
  );
}
