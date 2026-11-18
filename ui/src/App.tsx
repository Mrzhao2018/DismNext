import React, { useEffect, useMemo, useState } from 'react';
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
  Broom24Regular,
  History24Regular,
  Settings24Regular,
  ShieldCheckmark24Regular,
  WeatherMoon24Regular,
  WeatherSunny24Regular,
} from '@fluentui/react-icons';
import {
  CleanupRuleEntry,
  Lang,
  OptimizationGroupStatus,
  RegistrySnapshot,
  SysInfoPayload,
  api,
  formatBytes,
} from './api';
import { SpaceRecycleView } from './views/SpaceRecycleView';
import { OptimizationView } from './views/OptimizationView';
import { SnapshotView } from './views/SnapshotView';

type NavTab = 'cleanup' | 'optimize' | 'snapshots';

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

const AppShell: React.FC<{
  dark: boolean;
  onToggleTheme: () => void;
}> = ({ dark, onToggleTheme }) => {
  const styles = useStyles();
  const [lang, setLang] = useState<Lang>('zh');
  const [tab, setTab] = useState<NavTab>('cleanup');
  const [loading, setLoading] = useState(true);
  const [sysPayload, setSysPayload] = useState<SysInfoPayload | null>(null);
  const [cleanupEntries, setCleanupEntries] = useState<CleanupRuleEntry[]>([]);
  const [scanning, setScanning] = useState(false);
  const [optGroups, setOptGroups] = useState<OptimizationGroupStatus[]>([]);
  const [snapshots, setSnapshots] = useState<RegistrySnapshot[]>([]);
  const [elevating, setElevating] = useState(false);
  const [elevateErr, setElevateErr] = useState<string | null>(null);

  const loadInitialData = async () => {
    const [sys, rulesRes, optRes, snapRes] = await Promise.all([
      api.getSysInfo(),
      api.getCleanupRules(),
      api.getOptimizerGroups(),
      api.getSnapshots(),
    ]);
    setSysPayload(sys);
    setCleanupEntries(rulesRes.items);
    setOptGroups(optRes.groups);
    setSnapshots(snapRes.snapshots);
  };

  useEffect(() => {
    (async () => {
      try {
        await loadInitialData();
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  const handleElevate = async () => {
    setElevating(true);
    setElevateErr(null);
    try {
      const res = await api.elevate();
      if (res.relaunching) {
        // Poll until elevated backend binds the port and responds with is_admin=true
        for (let i = 0; i < 25; i++) {
          await new Promise((r) => setTimeout(r, 300));
          try {
            const sys = await api.getSysInfo();
            if (sys.system.is_admin) {
              await loadInitialData();
              setElevating(false);
              return;
            }
          } catch {
            // Backend is restarting during UAC handoff
          }
        }
      }
      await loadInitialData();
    } catch (e) {
      setElevateErr(String(e));
    } finally {
      setElevating(false);
    }
  };

  const handleScan = async () => {
    setScanning(true);
    try {
      const res = await api.scanCleanup();
      setCleanupEntries(res.items.map((rule) => ({ scanned: true, rule })));
    } finally {
      setScanning(false);
    }
  };

  const refreshSnapshots = async () => {
    const res = await api.getSnapshots();
    setSnapshots(res.snapshots);
  };

  const reclaimableBytes = useMemo(
    () => cleanupEntries.reduce((acc, e) => acc + e.rule.cleanable_bytes, 0),
    [cleanupEntries]
  );

  const disk = sysPayload?.system.system_disk;
  const usedPct = disk && disk.total_bytes > 0 ? (disk.used_bytes / disk.total_bytes) * 100 : 0;
  const reclaimPct =
    disk && disk.total_bytes > 0
      ? Math.min(usedPct, (reclaimableBytes / disk.total_bytes) * 100)
      : 0;

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
              appearance={tab === 'cleanup' ? 'primary' : 'subtle'}
              icon={<Broom24Regular />}
              onClick={() => setTab('cleanup')}
            >
              {lang === 'zh' ? '空间回收' : 'Space Recycle'}
            </Button>
            <Button
              className={styles.navBtn}
              appearance={tab === 'optimize' ? 'primary' : 'subtle'}
              icon={<Settings24Regular />}
              onClick={() => setTab('optimize')}
            >
              {lang === 'zh' ? '系统优化' : 'Optimization'}
            </Button>
            <Button
              className={styles.navBtn}
              appearance={tab === 'snapshots' ? 'primary' : 'subtle'}
              icon={<History24Regular />}
              onClick={() => setTab('snapshots')}
            >
              {lang === 'zh' ? `快照回滚 (${snapshots.length})` : `Snapshots (${snapshots.length})`}
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
            onClick={() => setLang((prev) => (prev === 'zh' ? 'en' : 'zh'))}
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
                      disabled={elevating}
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
                  {elevateErr && (
                    <Text size={100} style={{ color: tokens.colorPaletteRedForeground1 }}>
                      {elevateErr}
                    </Text>
                  )}
                </div>
                <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                  {lang === 'zh'
                    ? `规则库基线 ${sysPayload.catalog.base_version} · 已启用 ${cleanupEntries.length} 条空间规则 · ${optGroups.reduce((a, g) => a + g.items.length, 0)} 项系统优化`
                    : `Catalog ${sysPayload.catalog.base_version} · ${cleanupEntries.length} cleanup rules · ${optGroups.reduce((a, g) => a + g.items.length, 0)} tweaks`}
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
            <Spinner size="small" label="Loading system info..." />
          )}
        </header>

        <section className={styles.contentArea}>
          {loading ? (
            <div style={{ display: 'flex', justifyContent: 'center', paddingTop: 60 }}>
              <Spinner label={lang === 'zh' ? '正在加载规则引擎...' : 'Loading engine...'} />
            </div>
          ) : tab === 'cleanup' ? (
            <SpaceRecycleView
              lang={lang}
              entries={cleanupEntries}
              scanning={scanning}
              onScan={handleScan}
              onExecuted={async () => {
                await handleScan();
                const sys = await api.getSysInfo();
                setSysPayload(sys);
              }}
            />
          ) : tab === 'optimize' ? (
            <OptimizationView
              lang={lang}
              groups={optGroups}
              onGroupsUpdated={setOptGroups}
              onSnapshotCreated={(snap) => setSnapshots((prev) => [snap, ...prev])}
              onJumpToSnapshots={() => setTab('snapshots')}
            />
          ) : (
            <SnapshotView
              lang={lang}
              snapshots={snapshots}
              onRefresh={refreshSnapshots}
              onGroupsUpdated={setOptGroups}
            />
          )}
        </section>
      </main>
    </div>
  );
};

export default function App() {
  const [dark, setDark] = useState(false);
  return (
    <FluentProvider theme={dark ? webDarkTheme : webLightTheme}>
      <AppShell dark={dark} onToggleTheme={() => setDark((d) => !d)} />
    </FluentProvider>
  );
}
