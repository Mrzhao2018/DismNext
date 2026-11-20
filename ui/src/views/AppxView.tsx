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
  Tab,
  TabList,
  Text,
  makeStyles,
  tokens,
} from '@fluentui/react-components';
import {
  Apps24Regular,
  ArrowClockwise20Regular,
  Delete16Regular,
  Desktop24Regular,
  FolderOpen16Regular,
  LockClosed16Regular,
  Search20Regular,
} from '@fluentui/react-icons';
import {
  AppLeftoverItem,
  AppxCategory,
  AppxPackageInfo,
  Lang,
  LeftoverRecycleResult,
  LeftoverSkipReason,
  Win32AppInfo,
  Win32UninstallStatus,
  api,
  formatBytes,
  pickText,
} from '../api';

interface Props {
  lang: Lang;
}

const leftoverSkipLabels: Record<LeftoverSkipReason, { zh: string; en: string }> = {
  changed: { zh: '目录或内容已变更，请重新扫描。', en: 'The folder or its contents changed. Scan again.' },
  missing: { zh: '原路径下的目录已不存在。', en: 'The folder is no longer at its original path.' },
  protected: { zh: '路径受保护或包含重定向。', en: 'The path is protected or contains a redirect.' },
  unverifiable: { zh: '无法完整核对目录，请检查权限或占用状态后重新扫描。', en: 'Could not verify the whole folder. Check permissions or file locks and scan again.' },
};

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
  leftoverRow: {
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'space-between',
    gap: '10px',
    padding: '8px 10px',
    borderRadius: tokens.borderRadiusMedium,
    backgroundColor: tokens.colorNeutralBackground3,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
  },
});

export const AppxView: React.FC<Props> = ({ lang }) => {
  const styles = useStyles();
  const [mode, setMode] = useState<'win32' | 'appx'>('win32');

  // Win32 / MSI desktop apps state
  const [win32Apps, setWin32Apps] = useState<Win32AppInfo[]>([]);
  const [loadingWin32, setLoadingWin32] = useState(true);
  const [win32Search, setWin32Search] = useState('');
  const deferredWin32Search = React.useDeferredValue(win32Search);
  const [confirmWin32, setConfirmWin32] = useState<Win32AppInfo | null>(null);
  const [launchingUninstall, setLaunchingUninstall] = useState(false);
  const [uninstall, setUninstall] = useState<Win32UninstallStatus | null | undefined>(undefined);
  const [checkingUninstall, setCheckingUninstall] = useState(false);

  // Leftover scanner modal state
  const [leftoverTitle, setLeftoverTitle] = useState('');
  const [leftoverLoading, setLeftoverLoading] = useState(false);
  const [leftoverError, setLeftoverError] = useState<string | null>(null);
  const [leftoverDialogOpen, setLeftoverDialogOpen] = useState(false);
  const [leftovers, setLeftovers] = useState<AppLeftoverItem[]>([]);
  const [selectedLeftovers, setSelectedLeftovers] = useState<Set<string>>(new Set());
  const [leftoverResults, setLeftoverResults] = useState<Map<string, LeftoverRecycleResult>>(new Map());
  const leftoverScan = React.useRef<(() => Promise<{ leftovers: AppLeftoverItem[] }>) | null>(null);
  const [recyclingLeftovers, setRecyclingLeftovers] = useState(false);

  // Store / AppX state
  const [packages, setPackages] = useState<AppxPackageInfo[]>([]);
  const [loadingAppx, setLoadingAppx] = useState(true);
  const [cat, setCat] = useState<'all' | AppxCategory>('bloatware');
  const [appxSearch, setAppxSearch] = useState('');
  const deferredAppxSearch = React.useDeferredValue(appxSearch);
  const [confirmPkg, setConfirmPkg] = useState<AppxPackageInfo | null>(null);
  const [removingAppx, setRemovingAppx] = useState(false);

  const [notice, setNotice] = useState<string | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const loadWin32 = useCallback(async () => {
    setLoadingWin32(true);
    setErrorMsg(null);
    try {
      const res = await api.getWin32Apps();
      setWin32Apps(res.apps);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setLoadingWin32(false);
    }
  }, []);

  const loadAppx = async () => {
    setLoadingAppx(true);
    setErrorMsg(null);
    try {
      const res = await api.getAppxPackages();
      setPackages(res.packages);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setLoadingAppx(false);
    }
  };

  useEffect(() => {
    void loadWin32();
    void loadAppx();
  }, [loadWin32]);

  const pollUninstall = uninstall === undefined || uninstall?.state === 'running';
  useEffect(() => {
    if (!pollUninstall) return;
    let disposed = false;
    let timer: number | undefined;
    let pollError: string | null = null;

    const check = async () => {
      try {
        const { uninstall: status } = await api.getWin32UninstallStatus();
        if (disposed) return;
        if (pollError !== null) {
          const resolvedError = pollError;
          setErrorMsg((message) => message === resolvedError ? null : message);
          pollError = null;
        }
        setUninstall(status);
        if (status?.state !== 'running') {
          if (status?.state === 'completed') void loadWin32();
          return;
        }
      } catch (e) {
        if (disposed) return;
        pollError = String(e);
        setErrorMsg(pollError);
      }
      timer = window.setTimeout(check, 1000);
    };

    void check();
    return () => {
      disposed = true;
      window.clearTimeout(timer);
    };
  }, [pollUninstall, loadWin32]);

  const filteredWin32 = useMemo(() => {
    const q = deferredWin32Search.trim().toLowerCase();
    if (!q) return win32Apps;
    return win32Apps.filter(
      (a) =>
        a.display_name.toLowerCase().includes(q) ||
        a.publisher.toLowerCase().includes(q) ||
        a.install_location.toLowerCase().includes(q)
    );
  }, [win32Apps, deferredWin32Search]);

  const counts = useMemo(() => {
    const map: Record<AppxCategory, number> = {
      bloatware: 0,
      user_app: 0,
      framework: 0,
      system_core: 0,
    };
    packages.forEach((p) => {
      map[p.category] += 1;
    });
    return map;
  }, [packages]);

  const filteredAppx = useMemo(() => {
    const q = deferredAppxSearch.trim().toLowerCase();
    return packages.filter((p) => {
      if (cat !== 'all' && p.category !== cat) return false;
      if (!q) return true;
      return (
        p.display_name.zh.toLowerCase().includes(q) ||
        p.display_name.en.toLowerCase().includes(q) ||
        p.name.toLowerCase().includes(q) ||
        p.description.zh.toLowerCase().includes(q)
      );
    });
  }, [packages, cat, deferredAppxSearch]);

  const reviewLeftovers = async (
    title: string,
    scan: () => Promise<{ leftovers: AppLeftoverItem[] }>,
  ) => {
    leftoverScan.current = scan;
    setLeftoverTitle(title);
    setLeftoverDialogOpen(true);
    setLeftoverLoading(true);
    setLeftoverError(null);
    setLeftovers([]);
    setSelectedLeftovers(new Set());
    setLeftoverResults(new Map());
    try {
      setLeftovers((await scan()).leftovers);
    } catch (e) {
      setLeftoverError(String(e));
    } finally {
      setLeftoverLoading(false);
    }
  };

  const handleLaunchWin32Uninstall = async (useQuiet: boolean) => {
    if (!confirmWin32) return;
    setLaunchingUninstall(true);
    setErrorMsg(null);
    setNotice(null);
    try {
      const status = await api.uninstallWin32App(confirmWin32.id, useQuiet);
      setConfirmWin32(null);
      setUninstall(status);
      if (status.state === 'completed') void loadWin32();
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setLaunchingUninstall(false);
    }
  };

  const handleUninstallStatus = async (dismiss: boolean) => {
    if (!uninstall) return;
    setCheckingUninstall(true);
    setErrorMsg(null);
    try {
      if (dismiss) {
        await api.dismissWin32Uninstall(uninstall.app_id);
        setUninstall(null);
      } else {
        const { uninstall: status } = await api.getWin32UninstallStatus();
        setUninstall(status);
        if (status?.state === 'completed') void loadWin32();
      }
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setCheckingUninstall(false);
    }
  };

  const handleRecycleSelectedLeftovers = async () => {
    const targets = leftovers.flatMap(({ path, review }) =>
      selectedLeftovers.has(path) && review?.status === 'ready' ? [{ path, stamp: review.stamp }] : []);
    if (recyclingLeftovers || targets.length === 0) return;
    setRecyclingLeftovers(true);
    setLeftoverError(null);
    try {
      const { items } = await api.recycleAppLeftovers(targets);
      const resolved = new Set(items.filter((item) => item.status !== 'failed').map((item) => item.path));
      setLeftovers((prev) => prev.filter(({ path }) => !resolved.has(path)));
      setSelectedLeftovers(new Set(items.filter((item) => item.status === 'failed').map((item) => item.path)));
      setLeftoverResults((prev) => {
        const next = new Map(prev);
        for (const item of items) next.set(item.path, item);
        return next;
      });
    } catch (e) {
      setLeftoverError(String(e));
    } finally {
      setRecyclingLeftovers(false);
    }
  };

  const handleConfirmRemoveAppx = async () => {
    if (!confirmPkg) return;
    setRemovingAppx(true);
    setErrorMsg(null);
    setNotice(null);
    try {
      const res = await api.removeAppxPackage(confirmPkg.package_full_name);
      setPackages(res.packages);
      setNotice(
        lang === 'zh'
          ? `已卸载应用：${pickText(confirmPkg.display_name, lang)}`
          : `Uninstalled package: ${pickText(confirmPkg.display_name, lang)}`
      );
      setConfirmPkg(null);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setRemovingAppx(false);
    }
  };

  const catBadge = (pkg: AppxPackageInfo) => {
    if (pkg.category === 'bloatware') {
      return (
        <Badge size="small" appearance="tint" color="warning">
          {lang === 'zh' ? '可精简预装' : 'Preinstalled'}
        </Badge>
      );
    }
    if (pkg.category === 'user_app') {
      return (
        <Badge size="small" appearance="tint" color="brand">
          {lang === 'zh' ? '用户 / 商店应用' : 'User App'}
        </Badge>
      );
    }
    if (pkg.category === 'framework') {
      return (
        <Badge size="small" appearance="tint" color="subtle">
          {lang === 'zh' ? '运行时框架' : 'Framework'}
        </Badge>
      );
    }
    return (
      <Badge size="small" appearance="tint" color="success">
        <LockClosed16Regular style={{ verticalAlign: 'middle', marginRight: 3 }} />
        {lang === 'zh' ? '系统核心组件' : 'System Core'}
      </Badge>
    );
  };

  const kindLabel = (kind: string) => {
    if (kind === 'install_dir') return lang === 'zh' ? '安装目录' : 'Install Dir';
    if (kind === 'appdata_local') return 'AppData\\Local';
    if (kind === 'appdata_roaming') return 'AppData\\Roaming';
    return 'ProgramData';
  };

  const matchLabel = {
    registered_install_location: lang === 'zh' ? '登记的安装路径' : 'Registered install path',
    application_name: lang === 'zh' ? '仅匹配应用名称' : 'Application name only',
    publisher_and_application_name: lang === 'zh' ? '匹配发布商及应用名称' : 'Publisher and application name',
  };
  const uninstallLabel = {
    running: lang === 'zh' ? '正在等待卸载程序退出。' : 'Waiting for the uninstaller to exit.',
    awaiting_verification: lang === 'zh'
      ? '卸载程序已退出，Windows 仍有安装记录。如果卸载窗口还开着，请完成操作后重新检查。'
      : 'The launcher exited and Windows still lists the app. If an uninstall window is open, finish it before checking again.',
    completed: lang === 'zh'
      ? '已确认卸载完成，可以扫描残留目录。'
      : 'Uninstallation verified. You can now scan for leftovers.',
    cancelled: lang === 'zh' ? '卸载已取消。' : 'Uninstallation was cancelled.',
    restart_required: lang === 'zh' ? 'Windows Installer 需要重启才能完成卸载。' : 'Windows Installer requires a restart to finish.',
    failed: lang === 'zh' ? '卸载程序返回错误。' : 'The uninstaller returned an error.',
  };
  const uninstallBusy = pollUninstall || uninstall?.state === 'awaiting_verification';

  return (
    <div className={styles.root}>
      {errorMsg && (
        <MessageBar intent="error">
          <MessageBarBody>{errorMsg}</MessageBarBody>
        </MessageBar>
      )}
      {notice && (
        <MessageBar intent="success">
          <MessageBarBody>{notice}</MessageBarBody>
        </MessageBar>
      )}
      {mode === 'win32' && uninstall && (
        <MessageBar intent={
          uninstall.state === 'completed' ? 'success'
            : uninstall.state === 'failed' ? 'error'
              : uninstall.state === 'restart_required' ? 'warning' : 'info'
        }>
          <MessageBarBody>
            <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
              <Text>
                <strong>{uninstall.display_name}</strong> · {uninstallLabel[uninstall.state]}
                {uninstall.state === 'failed' && uninstall.exit_code !== null && ` (${uninstall.exit_code})`}
              </Text>
              {uninstall.state === 'completed' && (
                <Button
                  size="small"
                  disabled={leftoverLoading || checkingUninstall || launchingUninstall}
                  onClick={() => void reviewLeftovers(
                    uninstall.display_name,
                    () => api.scanWin32UninstallLeftovers(uninstall.app_id),
                  )}
                >
                  {lang === 'zh' ? '扫描残留' : 'Scan Leftovers'}
                </Button>
              )}
              {uninstall.state === 'awaiting_verification' && (
                <Button size="small" disabled={checkingUninstall} onClick={() => void handleUninstallStatus(false)}>
                  {lang === 'zh' ? '重新检查' : 'Check Again'}
                </Button>
              )}
              {uninstall.state !== 'running' && (
                <Button
                  size="small"
                  appearance="subtle"
                  disabled={checkingUninstall || launchingUninstall}
                  onClick={() => void handleUninstallStatus(true)}
                >
                  {lang === 'zh' ? '关闭状态' : 'Dismiss'}
                </Button>
              )}
            </div>
          </MessageBarBody>
        </MessageBar>
      )}

      <div className={styles.toolbar}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap' }}>
          {mode === 'win32' ? (
            <Desktop24Regular style={{ color: tokens.colorBrandForeground1 }} />
          ) : (
            <Apps24Regular style={{ color: tokens.colorBrandForeground1 }} />
          )}
          <TabList
            size="small"
            selectedValue={mode}
            onTabSelect={(_, d) => setMode(d.value as 'win32' | 'appx')}
          >
            <Tab value="win32">
              {lang === 'zh'
                ? `桌面软件卸载 (Win32/MSI · ${win32Apps.length})`
                : `Desktop Software (${win32Apps.length})`}
            </Tab>
            <Tab value="appx">
              {lang === 'zh'
                ? `商店与预装应用 (AppX · ${packages.length})`
                : `Store / AppX (${packages.length})`}
            </Tab>
          </TabList>
        </div>

        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <Input
            size="small"
            contentBefore={<Search20Regular />}
            placeholder={
              mode === 'win32'
                ? lang === 'zh'
                  ? '搜索桌面软件或发布商...'
                  : 'Search software or publisher...'
                : lang === 'zh'
                ? '搜索 AppX 应用名称或包名...'
                : 'Search packages...'
            }
            value={mode === 'win32' ? win32Search : appxSearch}
            onChange={(_, d) =>
              mode === 'win32' ? setWin32Search(d.value) : setAppxSearch(d.value)
            }
            style={{ width: 240 }}
          />
          <Button
            size="small"
            icon={<ArrowClockwise20Regular />}
            onClick={() => (mode === 'win32' ? void loadWin32() : void loadAppx())}
          >
            {lang === 'zh' ? '刷新' : 'Refresh'}
          </Button>
        </div>
      </div>

      {mode === 'appx' && (
        <div className={styles.toolbar} style={{ padding: '6px 14px' }}>
          <TabList
            size="small"
            selectedValue={cat}
            onTabSelect={(_, d) => setCat(d.value as 'all' | AppxCategory)}
          >
            <Tab value="bloatware">
              {lang === 'zh'
                ? `可精简预装 (${counts.bloatware})`
                : `Preinstalled (${counts.bloatware})`}
            </Tab>
            <Tab value="user_app">
              {lang === 'zh'
                ? `商店与用户应用 (${counts.user_app})`
                : `User Apps (${counts.user_app})`}
            </Tab>
            <Tab value="framework">
              {lang === 'zh' ? `运行库 (${counts.framework})` : `Frameworks (${counts.framework})`}
            </Tab>
            <Tab value="system_core">
              {lang === 'zh'
                ? `系统核心 (${counts.system_core})`
                : `System Core (${counts.system_core})`}
            </Tab>
            <Tab value="all">
              {lang === 'zh' ? `全部 (${packages.length})` : `All (${packages.length})`}
            </Tab>
          </TabList>
        </div>
      )}

      <div className={styles.listCard}>
        {mode === 'win32' ? (
          loadingWin32 ? (
            <div style={{ padding: 40, display: 'flex', justifyContent: 'center' }}>
              <Spinner label={lang === 'zh' ? '正在枚举已安装的桌面软件...' : 'Loading desktop apps...'} />
            </div>
          ) : filteredWin32.length === 0 ? (
            <Text style={{ padding: 20, color: tokens.colorNeutralForeground3 }}>
              {lang === 'zh' ? '没有匹配的桌面软件。' : 'No matching desktop software found.'}
            </Text>
          ) : (
            <VirtualList rows={filteredWin32} getKey={(app) => app.id} label={lang === 'zh' ? '桌面应用' : 'Desktop apps'} renderRow={(app) => (
              <div key={app.id} className={styles.row}>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 3, minWidth: 0, flex: 1 }}>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 6, flexWrap: 'wrap' }}>
                    <Text weight="semibold">{app.display_name}</Text>
                    {app.display_version && (
                      <Badge size="small" appearance="outline">
                        v{app.display_version}
                      </Badge>
                    )}
                    <Badge size="small" appearance="tint" color={app.is_msi ? 'brand' : 'subtle'}>
                      {app.is_msi ? 'MSI' : app.scope === 'user' ? 'User' : app.scope === 'system_x86' ? 'x86' : 'x64'}
                    </Badge>
                    {app.estimated_size_bytes > 0 && (
                      <Badge size="small" appearance="tint" color="informative">
                        {formatBytes(app.estimated_size_bytes)}
                      </Badge>
                    )}
                  </div>
                  <Text size={200} style={{ color: tokens.colorNeutralForeground2 }}>
                    {app.publisher || (lang === 'zh' ? '未知发布商' : 'Unknown Publisher')}
                    {app.install_date ? ` · ${lang === 'zh' ? '安装于' : 'Installed'} ${app.install_date}` : ''}
                  </Text>
                  {app.install_location && (
                    <Text
                      size={100}
                      truncate
                      title={app.install_location}
                      style={{ color: tokens.colorNeutralForeground3, fontFamily: 'Consolas, monospace' }}
                    >
                      {app.install_location}
                    </Text>
                  )}
                </div>

                <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexShrink: 0 }}>
                  {app.install_location && (
                    <Button
                      size="small"
                      appearance="subtle"
                      icon={<FolderOpen16Regular />}
                      onClick={() => { void api.revealInExplorer(app.install_location).catch((error) => setErrorMsg(String(error))); }}
                    >
                      {lang === 'zh' ? '目录' : 'Folder'}
                    </Button>
                  )}
                  <Button
                    size="small"
                    appearance="outline"
                    disabled={leftoverLoading || uninstall?.app_id === app.id}
                    onClick={() => void reviewLeftovers(
                      app.display_name,
                      () => api.scanAppLeftovers(app.display_name, app.publisher, app.install_location),
                    )}
                  >
                    {lang === 'zh' ? '扫描残留' : 'Leftovers'}
                  </Button>
                  <Button
                    size="small"
                    appearance="primary"
                    icon={<Delete16Regular />}
                    disabled={uninstallBusy || checkingUninstall || launchingUninstall}
                    onClick={() => setConfirmWin32(app)}
                  >
                    {lang === 'zh' ? '卸载' : 'Uninstall'}
                  </Button>
                </div>
              </div>
            )} />
          )
        ) : loadingAppx ? (
          <div style={{ padding: 40, display: 'flex', justifyContent: 'center' }}>
            <Spinner label={lang === 'zh' ? '正在读取已安装的现代应用...' : 'Loading packages...'} />
          </div>
        ) : filteredAppx.length === 0 ? (
          <Text style={{ padding: 20, color: tokens.colorNeutralForeground3 }}>
            {lang === 'zh' ? '当前分类下没有匹配的应用。' : 'No matching packages found.'}
          </Text>
        ) : (
          <VirtualList rows={filteredAppx} getKey={(pkg) => pkg.package_full_name} label={lang === 'zh' ? '现代应用' : 'AppX packages'} renderRow={(pkg) => (
            <div key={pkg.package_full_name} className={styles.row}>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 3, minWidth: 0, flex: 1 }}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 6, flexWrap: 'wrap' }}>
                  <Text weight="semibold">{pickText(pkg.display_name, lang)}</Text>
                  {catBadge(pkg)}
                  <Badge size="small" appearance="outline">
                    {pkg.version} ({pkg.arch})
                  </Badge>
                </div>
                <Text size={200} style={{ color: tokens.colorNeutralForeground2 }}>
                  {pickText(pkg.description, lang)}
                </Text>
                <Text
                  size={100}
                  truncate
                  title={pkg.package_full_name}
                  style={{ color: tokens.colorNeutralForeground3, fontFamily: 'Consolas, monospace' }}
                >
                  {pkg.package_full_name}
                </Text>
              </div>

              <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexShrink: 0 }}>
                {pkg.install_location && (
                  <Button
                    size="small"
                    appearance="subtle"
                    icon={<FolderOpen16Regular />}
                    onClick={() => { void api.revealInExplorer(pkg.install_location).catch((error) => setErrorMsg(String(error))); }}
                  >
                    {lang === 'zh' ? '目录' : 'Folder'}
                  </Button>
                )}
                <Button
                  size="small"
                  appearance={pkg.protected ? 'outline' : 'primary'}
                  icon={pkg.protected ? <LockClosed16Regular /> : <Delete16Regular />}
                  disabled={pkg.protected}
                  onClick={() => setConfirmPkg(pkg)}
                >
                  {pkg.protected
                    ? lang === 'zh'
                      ? '受保护'
                      : 'Protected'
                    : lang === 'zh'
                    ? '卸载'
                    : 'Uninstall'}
                </Button>
              </div>
            </div>
          )} />
        )}
      </div>

      {/* Win32 Uninstall confirmation dialog */}
      <Dialog
        open={Boolean(confirmWin32)}
        onOpenChange={(_, d) => {
          if (!d.open && !launchingUninstall) setConfirmWin32(null);
        }}
      >
        <DialogSurface>
          <DialogBody>
            <DialogTitle>
              {lang === 'zh' ? '卸载桌面软件' : 'Uninstall Desktop Software'}
            </DialogTitle>
            <DialogContent>
              {confirmWin32 && (
                <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
                  <Text>
                    {lang === 'zh'
                      ? `将运行「${confirmWin32.display_name}」登记的卸载程序。确认卸载完成后，可以扫描残留目录。`
                      : `Run the registered uninstaller for "${confirmWin32.display_name}". You can scan for leftovers once removal is verified.`}
                  </Text>
                  <Text
                    size={100}
                    style={{
                      fontFamily: 'Consolas, monospace',
                      color: tokens.colorNeutralForeground3,
                      wordBreak: 'break-all',
                    }}
                  >
                    {confirmWin32.uninstall_string || confirmWin32.quiet_uninstall_string}
                  </Text>
                </div>
              )}
            </DialogContent>
            <DialogActions>
              <Button
                appearance="secondary"
                disabled={launchingUninstall}
                onClick={() => setConfirmWin32(null)}
              >
                {lang === 'zh' ? '取消' : 'Cancel'}
              </Button>
              {confirmWin32?.quiet_uninstall_string && (
                <Button
                  appearance="outline"
                  disabled={launchingUninstall}
                  onClick={() => void handleLaunchWin32Uninstall(true)}
                >
                  {lang === 'zh' ? '静默卸载' : 'Quiet Uninstall'}
                </Button>
              )}
              <Button
                appearance="primary"
                disabled={launchingUninstall}
                onClick={() => void handleLaunchWin32Uninstall(false)}
              >
                {launchingUninstall
                  ? lang === 'zh'
                    ? '启动中...'
                    : 'Launching...'
                  : lang === 'zh'
                  ? '启动标准卸载'
                  : 'Run Uninstaller'}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>

      {/* Leftover cleanup review dialog */}
      <Dialog
        open={leftoverDialogOpen}
        onOpenChange={(_, d) => {
          if (!d.open && !recyclingLeftovers) setLeftoverDialogOpen(false);
        }}
      >
        <DialogSurface style={{ maxWidth: 680 }}>
          <DialogBody>
            <DialogTitle>
              {lang === 'zh'
                ? `残留扫描 · ${leftoverTitle}`
                : `Leftover Scan: ${leftoverTitle}`}
            </DialogTitle>
            <DialogContent>
              {leftoverError && (
                <MessageBar intent="error"><MessageBarBody>{leftoverError}</MessageBarBody></MessageBar>
              )}
              {leftoverResults.size > 0 && (
                <ul style={{ maxHeight: 168, overflowY: 'auto', overflowWrap: 'anywhere', paddingInlineStart: 20 }}>
                  {[...leftoverResults.values()].map((item) => (
                    <li key={item.path}>
                      <Text size={200} weight="semibold">
                        {item.status === 'recycled'
                          ? (lang === 'zh' ? '已移入回收站' : 'Recycled')
                          : item.status === 'skipped' ? (lang === 'zh' ? '已跳过' : 'Skipped') : (lang === 'zh' ? '失败' : 'Failed')}
                      </Text>
                      <Text size={200}> · {item.path}</Text>
                      <div>
                        <Text size={200}>
                          {item.status === 'recycled' ? formatBytes(item.size_bytes)
                            : item.status === 'skipped' ? leftoverSkipLabels[item.reason][lang] : item.error}
                        </Text>
                      </div>
                    </li>
                  ))}
                </ul>
              )}
              {leftoverLoading ? (
                <div style={{ padding: 24, display: 'flex', justifyContent: 'center' }}>
                  <Spinner label={lang === 'zh' ? '正在扫描 AppData / ProgramData / 安装目录...' : 'Scanning leftovers...'} />
                </div>
              ) : leftovers.length === 0 ? (
                !leftoverError && leftoverResults.size === 0 && <Text style={{ color: tokens.colorNeutralForeground3 }}>
                  {lang === 'zh'
                    ? '未检测到明显关联残留目录。'
                    : 'No candidate leftover folders detected.'}
                </Text>
              ) : (
                <div style={{ display: 'flex', flexDirection: 'column', gap: 8, maxHeight: 360, overflowY: 'auto' }}>
                  <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                    {lang === 'zh'
                      ? '根据匹配依据核对目录，再勾选需要移入回收站的项目。'
                      : 'Review why each directory matched, then select what to move to the Recycle Bin.'}
                  </Text>
                  {leftovers.map((item) => {
                    const checked = selectedLeftovers.has(item.path);
                    const blocked = item.review?.status !== 'ready';
                    const reason = item.review?.status === 'blocked' ? item.review.reason : 'unverifiable';
                    return (
                      <div key={item.path} className={styles.leftoverRow}>
                        <div style={{ display: 'flex', alignItems: 'center', gap: 8, minWidth: 0, flex: 1 }}>
                          <Checkbox
                            checked={checked}
                            disabled={blocked || recyclingLeftovers}
                            aria-label={item.path}
                            onChange={() =>
                              setSelectedLeftovers((prev) => {
                                const next = new Set(prev);
                                if (next.has(item.path)) next.delete(item.path);
                                else next.add(item.path);
                                return next;
                              })
                            }
                          />
                          <div style={{ display: 'flex', flexDirection: 'column', minWidth: 0, flex: 1 }}>
                            <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                              <Badge size="small" appearance="tint" color={item.kind === 'install_dir' ? 'warning' : 'brand'}>
                                {kindLabel(item.kind)}
                              </Badge>
                              {!blocked && <Text size={200} weight="semibold">
                                {formatBytes(item.size_bytes)} · {item.file_count}{' '}
                                {lang === 'zh' ? '个文件' : 'files'}
                              </Text>}
                            </div>
                            <Text size={100} style={{ color: tokens.colorNeutralForeground3 }}>
                              {matchLabel[item.match_basis]}
                            </Text>
                            <Text
                              size={100}
                              truncate
                              title={item.path}
                              style={{ fontFamily: 'Consolas, monospace', color: tokens.colorNeutralForeground3 }}
                            >
                              {item.path}
                            </Text>
                            {blocked && <Text size={200}>{leftoverSkipLabels[reason][lang]}</Text>}
                          </div>
                        </div>
                        <Button
                          size="small"
                          appearance="subtle"
                          icon={<FolderOpen16Regular />}
                          onClick={() => { void api.revealInExplorer(item.path).catch((error) => setLeftoverError(String(error))); }}
                        />
                      </div>
                    );
                  })}
                </div>
              )}
            </DialogContent>
            <DialogActions>
              <Button
                appearance="secondary"
                disabled={leftoverLoading || recyclingLeftovers}
                onClick={() => {
                  if (leftoverScan.current) void reviewLeftovers(leftoverTitle, leftoverScan.current);
                }}
              >
                {lang === 'zh' ? '重新扫描' : 'Scan Again'}
              </Button>
              <Button
                appearance="secondary"
                disabled={recyclingLeftovers}
                onClick={() => setLeftoverDialogOpen(false)}
              >
                {lang === 'zh' ? '暂不清理 / 关闭' : 'Close'}
              </Button>
              {leftovers.length > 0 && (
                <Button
                  appearance="primary"
                  disabled={recyclingLeftovers || selectedLeftovers.size === 0}
                  onClick={() => void handleRecycleSelectedLeftovers()}
                >
                  {recyclingLeftovers
                    ? lang === 'zh'
                      ? '正在移入回收站...'
                      : 'Recycling...'
                    : lang === 'zh'
                    ? `移入回收站 (${selectedLeftovers.size})`
                    : `Move to Recycle Bin (${selectedLeftovers.size})`}
                </Button>
              )}
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>

      {/* AppX uninstall confirmation dialog */}
      <Dialog
        open={Boolean(confirmPkg)}
        onOpenChange={(_, d) => {
          if (!d.open && !removingAppx) setConfirmPkg(null);
        }}
      >
        <DialogSurface>
          <DialogBody>
            <DialogTitle>
              {lang === 'zh' ? '确认卸载现代应用？' : 'Confirm Uninstall App'}
            </DialogTitle>
            <DialogContent>
              {confirmPkg && (
                <>
                  <Text block>
                    {lang === 'zh'
                      ? `确定要卸载「${pickText(confirmPkg.display_name, lang)}」吗？之后如有需要可从应用商店重新获取。`
                      : `Uninstall "${pickText(confirmPkg.display_name, lang)}"? You can reinstall it later from the Store.`}
                  </Text>
                  <Text
                    block
                    size={200}
                    style={{
                      marginTop: 8,
                      fontFamily: 'Consolas, monospace',
                      color: tokens.colorNeutralForeground3,
                      wordBreak: 'break-all',
                    }}
                  >
                    {confirmPkg.package_full_name}
                  </Text>
                </>
              )}
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" disabled={removingAppx} onClick={() => setConfirmPkg(null)}>
                {lang === 'zh' ? '取消' : 'Cancel'}
              </Button>
              <Button appearance="primary" disabled={removingAppx} onClick={handleConfirmRemoveAppx}>
                {removingAppx
                  ? lang === 'zh'
                    ? '卸载中...'
                    : 'Uninstalling...'
                  : lang === 'zh'
                  ? '确认卸载'
                  : 'Uninstall'}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </div>
  );
};
