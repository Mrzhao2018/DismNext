import { VirtualList } from './VirtualList';
import React, { useEffect, useMemo, useState } from 'react';
import {
  Badge,
  Button,
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
  Switch,
  Tab,
  TabList,
  Text,
  makeStyles,
  tokens,
} from '@fluentui/react-components';
import {
  ArrowClockwise20Regular,
  Copy16Regular,
  FolderOpen16Regular,
  Rocket24Regular,
  Search20Regular,
  Warning16Regular,
} from '@fluentui/react-icons';
import {
  Lang,
  ScheduledTaskItem,
  StartupInspection,
  StartupItem,
  StartupSignature,
  ThirdPartyServiceItem,
  api,
} from '../api';

interface Props {
  lang: Lang;
  isAdmin: boolean;
}

type StartupMode = 'run' | 'services' | 'tasks';

const signatureLabels: Record<StartupSignature, Record<Lang, string>> = {
  valid: { zh: '签名有效', en: 'Valid signature' },
  unsigned: { zh: '未签名', en: 'Unsigned' },
  untrusted: { zh: '签名不受信任', en: 'Untrusted signature' },
  hash_mismatch: { zh: '签名与文件内容不匹配', en: 'Signature does not match file contents' },
  unsupported: { zh: '此文件类型不支持签名验证', en: 'Signature verification unsupported for this file type' },
  missing_target: { zh: '找不到目标文件', en: 'Target file missing' },
  unresolved_target: { zh: '无法确定本地目标文件', en: 'Local target could not be determined' },
  unavailable: { zh: '暂时无法验证签名', en: 'Signature could not be verified' },
};

interface InspectionRequest {
  name: string;
  details?: StartupInspection;
  error?: string;
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
  inspection: {
    display: 'flex',
    flexDirection: 'column',
    gap: '12px',
  },
  inspectionSection: {
    display: 'flex',
    flexDirection: 'column',
    alignItems: 'flex-start',
    gap: '4px',
  },
  sourcePath: {
    color: tokens.colorNeutralForeground2,
    fontFamily: 'Consolas, monospace',
    overflowWrap: 'anywhere',
  },
});

export const StartupView: React.FC<Props> = ({ lang, isAdmin }) => {
  const styles = useStyles();
  const [mode, setMode] = useState<StartupMode>('run');

  // Run entries
  const [items, setItems] = useState<StartupItem[]>([]);
  const [loadingRun, setLoadingRun] = useState(true);
  const [scopeFilter, setScopeFilter] = useState<'all' | 'user' | 'system'>('all');
  const [onlyEnabled, setOnlyEnabled] = useState(false);

  // Services
  const [services, setServices] = useState<ThirdPartyServiceItem[]>([]);
  const [loadingServices, setLoadingServices] = useState(false);
  const [servicesLoaded, setServicesLoaded] = useState(false);

  // Scheduled tasks
  const [tasks, setTasks] = useState<ScheduledTaskItem[]>([]);
  const [loadingTasks, setLoadingTasks] = useState(false);
  const [tasksLoaded, setTasksLoaded] = useState(false);

  const [search, setSearch] = useState('');
  const deferredSearch = React.useDeferredValue(search);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [inspection, setInspection] = useState<InspectionRequest | null>(null);
  const [inspectionActionError, setInspectionActionError] = useState<string | null>(null);
  const [sourceCopied, setSourceCopied] = useState(false);
  const inspected = inspection?.details;
  const origin = inspected?.origin;

  const openInspection = async (item: StartupItem) => {
    const request = { name: item.name };
    setInspection(request);
    setInspectionActionError(null);
    setSourceCopied(false);
    try {
      const details = await api.inspectStartupItem(item.id);
      setInspection((previous) => previous === request ? { ...request, details } : previous);
    } catch (error) {
      setInspection((previous) => previous === request ? { ...request, error: String(error) } : previous);
    }
  };

  const copyRegistryPath = async (path: string) => {
    setInspectionActionError(null);
    try {
      await navigator.clipboard.writeText(path);
      setSourceCopied(true);
    } catch (error) {
      setInspectionActionError(String(error));
    }
  };

  const revealStartupFile = async (path: string) => {
    setInspectionActionError(null);
    try {
      await api.revealInExplorer(path);
    } catch (error) {
      setInspectionActionError(String(error));
    }
  };

  const loadRun = async () => {
    setLoadingRun(true);
    setErrorMsg(null);
    try {
      const res = await api.getStartupItems();
      setItems(res.items);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setLoadingRun(false);
    }
  };

  const loadServices = async () => {
    setLoadingServices(true);
    setErrorMsg(null);
    try {
      const res = await api.getServices();
      setServices(res.services);
      setServicesLoaded(true);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setLoadingServices(false);
    }
  };

  const loadTasks = async () => {
    setLoadingTasks(true);
    setErrorMsg(null);
    try {
      const res = await api.getScheduledTasks();
      setTasks(res.tasks);
      setTasksLoaded(true);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setLoadingTasks(false);
    }
  };

  useEffect(() => {
    void loadRun();
  }, []);

  useEffect(() => {
    if (mode === 'services' && !servicesLoaded) {
      void loadServices();
    } else if (mode === 'tasks' && !tasksLoaded) {
      void loadTasks();
    }
  }, [mode, servicesLoaded, tasksLoaded]);

  const filteredRun = useMemo(() => {
    const q = deferredSearch.trim().toLowerCase();
    return items.filter((it) => {
      if (scopeFilter !== 'all' && it.scope !== scopeFilter) return false;
      if (onlyEnabled && !it.enabled) return false;
      if (!q) return true;
      return (
        it.name.toLowerCase().includes(q) ||
        it.command.toLowerCase().includes(q) ||
        it.location.toLowerCase().includes(q)
      );
    });
  }, [items, scopeFilter, onlyEnabled, deferredSearch]);

  const filteredServices = useMemo(() => {
    const q = deferredSearch.trim().toLowerCase();
    return services.filter((s) => {
      if (onlyEnabled && s.start_mode !== 'auto') return false;
      if (!q) return true;
      return (
        s.name.toLowerCase().includes(q) ||
        s.display_name.toLowerCase().includes(q) ||
        s.image_path.toLowerCase().includes(q)
      );
    });
  }, [services, onlyEnabled, deferredSearch]);

  const filteredTasks = useMemo(() => {
    const q = deferredSearch.trim().toLowerCase();
    return tasks.filter((t) => {
      if (onlyEnabled && !t.enabled) return false;
      if (!q) return true;
      return (
        t.name.toLowerCase().includes(q) ||
        t.path.toLowerCase().includes(q) ||
        t.command.toLowerCase().includes(q)
      );
    });
  }, [tasks, onlyEnabled, deferredSearch]);

  const enabledRunCount = useMemo(() => items.filter((i) => i.enabled).length, [items]);
  const autoServiceCount = useMemo(
    () => services.filter((s) => s.start_mode === 'auto').length,
    [services]
  );
  const enabledTaskCount = useMemo(() => tasks.filter((t) => t.enabled).length, [tasks]);

  const handleToggleRun = async (it: StartupItem, nextEnabled: boolean) => {
    setBusyId(it.id);
    setErrorMsg(null);
    try {
      const res = await api.toggleStartup(it.id, nextEnabled);
      setItems(res.items);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setBusyId(null);
    }
  };

  const handleServiceMode = async (svcName: string, nextMode: 'auto' | 'manual' | 'disabled') => {
    setBusyId(svcName);
    setErrorMsg(null);
    try {
      const res = await api.setServiceMode(svcName, nextMode);
      setServices(res.services);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setBusyId(null);
    }
  };

  const handleToggleTask = async (taskPath: string, nextEnabled: boolean) => {
    setBusyId(taskPath);
    setErrorMsg(null);
    try {
      const res = await api.toggleScheduledTask(taskPath, nextEnabled);
      setTasks(res.tasks);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setBusyId(null);
    }
  };

  const refreshCurrent = () => {
    if (mode === 'run') void loadRun();
    else if (mode === 'services') void loadServices();
    else void loadTasks();
  };

  return (
    <div className={styles.root}>
      {errorMsg && (
        <MessageBar intent="error">
          <MessageBarBody>{errorMsg}</MessageBarBody>
        </MessageBar>
      )}

      {!isAdmin && mode !== 'run' && (
        <MessageBar intent="warning">
          <MessageBarBody>
            <Warning16Regular style={{ verticalAlign: 'middle', marginRight: 6 }} />
            {lang === 'zh'
              ? '当前为标准用户模式，可查看第三方服务与计划任务；修改服务启动类型或禁系统级任务请先点击顶部“一键提权为管理员”。'
              : 'Modifying Win32 service startup modes or machine scheduled tasks requires Administrator privileges.'}
          </MessageBarBody>
        </MessageBar>
      )}

      <div className={styles.toolbar}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap' }}>
          <Rocket24Regular style={{ color: tokens.colorBrandForeground1 }} />
          <TabList
            size="small"
            selectedValue={mode}
            onTabSelect={(_, d) => setMode(d.value as StartupMode)}
          >
            <Tab value="run">
              {lang === 'zh' ? `常规自启 (${items.length})` : `Startup Run (${items.length})`}
            </Tab>
            <Tab value="services">
              {lang === 'zh'
                ? `第三方服务${servicesLoaded ? ` (${services.length})` : ''}`
                : `3rd-Party Services${servicesLoaded ? ` (${services.length})` : ''}`}
            </Tab>
            <Tab value="tasks">
              {lang === 'zh'
                ? `计划任务${tasksLoaded ? ` (${tasks.length})` : ''}`
                : `Scheduled Tasks${tasksLoaded ? ` (${tasks.length})` : ''}`}
            </Tab>
          </TabList>

          {mode === 'run' && (
            <>
              <TabList
                size="small"
                selectedValue={scopeFilter}
                onTabSelect={(_, d) => setScopeFilter(d.value as 'all' | 'user' | 'system')}
              >
                <Tab value="all">{lang === 'zh' ? '全部' : 'All'}</Tab>
                <Tab value="user">{lang === 'zh' ? '用户' : 'User'}</Tab>
                <Tab value="system">{lang === 'zh' ? '全局' : 'System'}</Tab>
              </TabList>
              <Badge appearance="tint" color="success">
                {lang === 'zh' ? `已启用 ${enabledRunCount}` : `${enabledRunCount} Enabled`}
              </Badge>
            </>
          )}
          {mode === 'services' && servicesLoaded && (
            <Badge appearance="tint" color="warning">
              {lang === 'zh' ? `自启中 ${autoServiceCount}` : `${autoServiceCount} Auto`}
            </Badge>
          )}
          {mode === 'tasks' && tasksLoaded && (
            <Badge appearance="tint" color="success">
              {lang === 'zh' ? `已启用 ${enabledTaskCount}` : `${enabledTaskCount} Enabled`}
            </Badge>
          )}
        </div>

        <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap' }}>
          <Switch
            checked={onlyEnabled}
            onChange={(_, d) => setOnlyEnabled(d.checked)}
            label={
              mode === 'services'
                ? lang === 'zh'
                  ? '仅看开机自启'
                  : 'Auto only'
                : lang === 'zh'
                ? '仅看已启用'
                : 'Enabled only'
            }
          />
          <Input
            size="small"
            contentBefore={<Search20Regular />}
            placeholder={lang === 'zh' ? '搜索名称、路径或命令...' : 'Search name, path, command...'}
            value={search}
            onChange={(_, d) => setSearch(d.value)}
            style={{ width: 210 }}
          />
          <Button size="small" icon={<ArrowClockwise20Regular />} onClick={refreshCurrent}>
            {lang === 'zh' ? '刷新' : 'Refresh'}
          </Button>
        </div>
      </div>

      <div className={styles.listCard}>
        {mode === 'run' &&
          (loadingRun ? (
            <div style={{ padding: 40, display: 'flex', justifyContent: 'center' }}>
              <Spinner label={lang === 'zh' ? '正在枚举开机启动项...' : 'Loading startup items...'} />
            </div>
          ) : filteredRun.length === 0 ? (
            <Text style={{ padding: 20, color: tokens.colorNeutralForeground3 }}>
              {lang === 'zh' ? '当前筛选下没有启动项。' : 'No matching startup items found.'}
            </Text>
          ) : (
            <VirtualList rows={filteredRun} getKey={(it) => it.id} label={lang === 'zh' ? '启动项' : 'Startup items'} renderRow={(it) => {
              const lockedByPriv = it.scope === 'system' && !isAdmin;
              return (
                <div key={it.id} className={styles.row}>
                  <div style={{ display: 'flex', flexDirection: 'column', gap: 3, minWidth: 0, flex: 1 }}>
                    <div style={{ display: 'flex', alignItems: 'center', gap: 6, flexWrap: 'wrap' }}>
                      <Text weight="semibold">{it.name}</Text>
                      <Badge size="small" appearance="tint" color={it.scope === 'system' ? 'warning' : 'brand'}>
                        {it.location}
                      </Badge>
                      {!it.target_exists && (
                        <Badge size="small" appearance="tint" color="danger">
                          <Warning16Regular style={{ verticalAlign: 'middle', marginRight: 3 }} />
                          {lang === 'zh' ? '未定位目标' : 'Target not located'}
                        </Badge>
                      )}
                      {lockedByPriv && (
                        <Badge size="small" appearance="outline" color="warning">
                          {lang === 'zh' ? '修改需管理员权限' : 'Admin required'}
                        </Badge>
                      )}
                    </div>
                    <Text
                      size={100}
                      truncate
                      title={it.command}
                      style={{
                        color: tokens.colorNeutralForeground3,
                        fontFamily: 'Consolas, monospace',
                      }}
                    >
                      {it.command}
                    </Text>
                  </div>

                  <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexShrink: 0 }}>
                    <Button size="small" appearance="subtle" onClick={() => void openInspection(it)}>
                      {lang === 'zh' ? '来源与签名' : 'Source and Signature'}
                    </Button>
                    {it.target_path && it.target_exists && (
                      <Button
                        size="small"
                        appearance="subtle"
                        icon={<FolderOpen16Regular />}
                        onClick={() => { void api.revealInExplorer(it.target_path!).catch((error) => setErrorMsg(String(error))); }}
                      >
                        {lang === 'zh' ? '定位' : 'Reveal'}
                      </Button>
                    )}
                    <Switch
                      aria-label={it.name}
                      checked={it.enabled}
                      disabled={busyId === it.id || lockedByPriv}
                      onChange={(_, d) => handleToggleRun(it, d.checked)}
                    />
                  </div>
                </div>
              );
            }} />
          ))}

        {mode === 'services' &&
          (loadingServices ? (
            <div style={{ padding: 40, display: 'flex', justifyContent: 'center' }}>
              <Spinner label={lang === 'zh' ? '正在扫描第三方后台服务...' : 'Scanning third-party services...'} />
            </div>
          ) : filteredServices.length === 0 ? (
            <Text style={{ padding: 20, color: tokens.colorNeutralForeground3 }}>
              {lang === 'zh' ? '当前筛选下没有匹配的第三方后台服务。' : 'No matching third-party services.'}
            </Text>
          ) : (
            <VirtualList rows={filteredServices} getKey={(svc) => svc.name} label={lang === 'zh' ? '后台服务' : 'Services'} renderRow={(svc) => (
              <div key={svc.name} className={styles.row}>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 3, minWidth: 0, flex: 1 }}>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 6, flexWrap: 'wrap' }}>
                    <Text weight="semibold">{svc.display_name}</Text>
                    <Badge size="small" appearance="outline">
                      {svc.name}
                    </Badge>
                    <Badge
                      size="small"
                      appearance="tint"
                      color={
                        svc.start_mode === 'auto'
                          ? 'warning'
                          : svc.start_mode === 'manual'
                          ? 'brand'
                          : 'subtle'
                      }
                    >
                      {svc.start_mode === 'auto'
                        ? svc.delayed_auto
                          ? lang === 'zh'
                            ? '延迟自启'
                            : 'Auto (Delayed)'
                          : lang === 'zh'
                          ? '开机自启'
                          : 'Auto'
                        : svc.start_mode === 'manual'
                        ? lang === 'zh'
                          ? '手动按需'
                          : 'Manual'
                        : lang === 'zh'
                        ? '已禁用'
                        : 'Disabled'}
                    </Badge>
                    {!svc.target_exists && (
                      <Badge size="small" appearance="tint" color="danger">
                        {lang === 'zh' ? '执行文件缺失' : 'Binary Missing'}
                      </Badge>
                    )}
                  </div>
                  {svc.description && (
                    <Text size={200} style={{ color: tokens.colorNeutralForeground2 }} truncate>
                      {svc.description}
                    </Text>
                  )}
                  <Text
                    size={100}
                    truncate
                    title={svc.image_path}
                    style={{
                      color: tokens.colorNeutralForeground3,
                      fontFamily: 'Consolas, monospace',
                    }}
                  >
                    {svc.image_path}
                  </Text>
                </div>

                <div style={{ display: 'flex', alignItems: 'center', gap: 6, flexShrink: 0 }}>
                  {svc.target_path && svc.target_exists && (
                    <Button
                      size="small"
                      appearance="subtle"
                      icon={<FolderOpen16Regular />}
                      onClick={() => { void api.revealInExplorer(svc.target_path!).catch((error) => setErrorMsg(String(error))); }}
                    />
                  )}
                  <Button
                    size="small"
                    appearance={svc.start_mode === 'auto' ? 'primary' : 'outline'}
                    disabled={busyId === svc.name}
                    onClick={() => handleServiceMode(svc.name, 'auto')}
                  >
                    {lang === 'zh' ? '自启' : 'Auto'}
                  </Button>
                  <Button
                    size="small"
                    appearance={svc.start_mode === 'manual' ? 'primary' : 'outline'}
                    disabled={busyId === svc.name}
                    onClick={() => handleServiceMode(svc.name, 'manual')}
                  >
                    {lang === 'zh' ? '手动' : 'Manual'}
                  </Button>
                  <Button
                    size="small"
                    appearance={svc.start_mode === 'disabled' ? 'primary' : 'outline'}
                    disabled={busyId === svc.name}
                    onClick={() => handleServiceMode(svc.name, 'disabled')}
                  >
                    {lang === 'zh' ? '禁用' : 'Disable'}
                  </Button>
                </div>
              </div>
            )} />
          ))}

        {mode === 'tasks' &&
          (loadingTasks ? (
            <div style={{ padding: 40, display: 'flex', justifyContent: 'center' }}>
              <Spinner label={lang === 'zh' ? '正在枚举第三方计划任务...' : 'Scanning scheduled tasks...'} />
            </div>
          ) : filteredTasks.length === 0 ? (
            <Text style={{ padding: 20, color: tokens.colorNeutralForeground3 }}>
              {lang === 'zh' ? '当前筛选下没有匹配的第三方计划任务。' : 'No matching scheduled tasks.'}
            </Text>
          ) : (
            <VirtualList rows={filteredTasks} getKey={(t) => t.path} label={lang === 'zh' ? '计划任务' : 'Scheduled tasks'} renderRow={(t) => (
              <div key={t.path} className={styles.row}>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 3, minWidth: 0, flex: 1 }}>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 6, flexWrap: 'wrap' }}>
                    <Text weight="semibold">{t.name}</Text>
                    <Badge size="small" appearance="outline">
                      {t.path}
                    </Badge>
                    <Badge
                      size="small"
                      appearance="tint"
                      color={t.enabled ? 'success' : 'subtle'}
                    >
                      {t.state}
                    </Badge>
                  </div>
                  <Text
                    size={100}
                    truncate
                    title={t.command}
                    style={{
                      color: tokens.colorNeutralForeground3,
                      fontFamily: 'Consolas, monospace',
                    }}
                  >
                    {t.command || '(Trigger / COM handler)'}
                  </Text>
                </div>

                <Switch
                  aria-label={t.name}
                  checked={t.enabled}
                  disabled={busyId === t.path}
                  onChange={(_, d) => handleToggleTask(t.path, d.checked)}
                />
              </div>
            )} />
          ))}
      </div>
      <Dialog
        open={inspection !== null}
        onOpenChange={(_, data) => { if (!data.open) setInspection(null); }}
      >
        <DialogSurface style={{ maxWidth: 600 }}>
          <DialogBody>
            <DialogTitle>{inspection?.name}</DialogTitle>
            <DialogContent className={styles.inspection}>
              {inspection?.error ? (
                <MessageBar intent="error"><MessageBarBody>{inspection.error}</MessageBarBody></MessageBar>
              ) : inspected ? (
                <>
                  <section className={styles.inspectionSection}>
                    <Text weight="semibold">{lang === 'zh' ? '启动来源' : 'Startup source'}</Text>
                    {origin?.kind === 'registry' ? (
                      <>
                        <Text className={styles.sourcePath}>{origin.key_path}</Text>
                        <Text size={200}>
                          {lang === 'zh' ? '注册表值名' : 'Registry value'} · {origin.value_name}
                        </Text>
                        <Button
                          size="small"
                          icon={<Copy16Regular />}
                          onClick={() => void copyRegistryPath(origin.key_path)}
                        >
                          {sourceCopied
                            ? (lang === 'zh' ? '已复制' : 'Copied')
                            : (lang === 'zh' ? '复制注册表路径' : 'Copy registry path')}
                        </Button>
                      </>
                    ) : origin?.kind === 'startup_folder' && (
                      <>
                        <Text className={styles.sourcePath}>{origin.path}</Text>
                        <Button
                          size="small"
                          icon={<FolderOpen16Regular />}
                          onClick={() => void revealStartupFile(origin.path)}
                        >
                          {lang === 'zh' ? '定位启动文件' : 'Reveal Startup File'}
                        </Button>
                      </>
                    )}
                  </section>
                  <section className={styles.inspectionSection}>
                    <Text weight="semibold">{lang === 'zh' ? '启动目标签名' : 'Launch file signature'}</Text>
                    {inspected.target.path && (
                      <Text className={styles.sourcePath}>{inspected.target.path}</Text>
                    )}
                    <Badge
                      appearance="tint"
                      color={inspected.target.signature === 'valid' ? 'success' : 'subtle'}
                    >
                      {signatureLabels[inspected.target.signature][lang]}
                    </Badge>
                    {inspected.target.signer && (
                      <Text size={200}>
                        {lang === 'zh' ? '证书签名者' : 'Certificate signer'} · {inspected.target.signer}
                      </Text>
                    )}
                    {inspected.target.company && (
                      <Text size={200}>
                        {lang === 'zh' ? '文件声明的公司' : 'Company from file metadata'} · {inspected.target.company}
                      </Text>
                    )}
                    <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                      {lang === 'zh'
                        ? '核对启动命令指定的文件或快捷方式目标。命令参数可能另外引用文件。'
                        : 'Checks the file named by the startup command or the shortcut target. Arguments may reference other files.'}
                    </Text>
                  </section>
                  {inspectionActionError && (
                    <MessageBar intent="error"><MessageBarBody>{inspectionActionError}</MessageBarBody></MessageBar>
                  )}
                </>
              ) : (
                <Spinner label={lang === 'zh' ? '正在读取来源并验证签名…' : 'Checking source and signature…'} />
              )}
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={() => setInspection(null)}>
                {lang === 'zh' ? '关闭' : 'Close'}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </div>
  );
};
