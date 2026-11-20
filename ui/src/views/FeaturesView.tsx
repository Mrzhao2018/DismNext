import { useMaintenance } from '../MaintenanceJobs';
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
  PuzzlePieceRegular,
  Search20Regular,
  Warning16Regular,
} from '@fluentui/react-icons';
import { FeatureCategory, Lang, WindowsFeatureInfo, api, pickText } from '../api';

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

export const FeaturesView: React.FC<Props> = ({ lang, isAdmin }) => {
  const { active: activeMaintenance, run: runMaintenance } = useMaintenance();
  const styles = useStyles();
  const [features, setFeatures] = useState<WindowsFeatureInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [catFilter, setCatFilter] = useState<'all' | FeatureCategory>('all');
  const [onlyEnabled, setOnlyEnabled] = useState(false);
  const [search, setSearch] = useState('');
  const deferredSearch = React.useDeferredValue(search);
  const [busyName, setBusyName] = useState<string | null>(null);
  const [pendingToggle, setPendingToggle] = useState<{
    feat: WindowsFeatureInfo;
    nextEnabled: boolean;
  } | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const load = async () => {
    setLoading(true);
    setErrorMsg(null);
    try {
      const res = await api.getFeatures();
      setFeatures(res.features);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, []);

  const counts = useMemo(() => {
    const map: Record<string, number> = {
      all: features.length,
      virtualization: 0,
      developer: 0,
      security_ai: 0,
      network_smb: 0,
      print_media: 0,
      iis_web: 0,
    };
    features.forEach((f) => {
      map[f.category] = (map[f.category] || 0) + 1;
    });
    return map;
  }, [features]);

  const filtered = useMemo(() => {
    const q = deferredSearch.trim().toLowerCase();
    return features.filter((f) => {
      if (catFilter !== 'all' && f.category !== catFilter) return false;
      if (onlyEnabled && !f.enabled) return false;
      if (!q) return true;
      return (
        f.name.toLowerCase().includes(q) ||
        f.display_name.zh.toLowerCase().includes(q) ||
        f.display_name.en.toLowerCase().includes(q) ||
        f.description.zh.toLowerCase().includes(q)
      );
    });
  }, [features, catFilter, onlyEnabled, deferredSearch]);

  const handleToggle = async (feat: WindowsFeatureInfo, nextEnabled: boolean) => {
    setPendingToggle(null);
    setBusyName(feat.name);
    setErrorMsg(null);
    setNotice(null);
    try {
      const res = await runMaintenance({ kind: 'feature', name: feat.name, enabled: nextEnabled }, pickText(feat.display_name, lang));
      if (!res) return;
      setFeatures(res.features);
      setNotice(
        lang === 'zh'
          ? `${pickText(feat.display_name, lang)} 已设置为${nextEnabled ? '开启' : '关闭'}${res.outcome.restart_required ? '（需要重启 Windows 后完全生效）' : ''}。`
          : `${feat.name} ${nextEnabled ? 'enabled' : 'disabled'}${res.outcome.restart_required ? ' (restart required)' : ''}.`
      );
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setBusyName(null);
    }
  };

  const requestToggle = (feat: WindowsFeatureInfo, nextEnabled: boolean) => {
    if (feat.high_impact) {
      setPendingToggle({ feat, nextEnabled });
    } else {
      void handleToggle(feat, nextEnabled);
    }
  };

  return (
    <div className={styles.root}>
      {!isAdmin && (
        <MessageBar intent="warning">
          <MessageBarBody>
            <Warning16Regular style={{ verticalAlign: 'middle', marginRight: 6 }} />
            {lang === 'zh'
              ? '当前为标准用户模式，可查看全部系统可选功能状态；启用或关闭 WSL、Hyper-V、.NET 3.5 等组件需要点击顶部“一键提权为管理员”。'
              : 'Standard user mode is read-only for feature toggles. Click "Elevate to Admin" in the top bar before changing Windows features.'}
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
          <MessageBarBody>{notice}</MessageBarBody>
        </MessageBar>
      )}

      <div className={styles.toolbar}>
        <TabList
          size="small"
          selectedValue={catFilter}
          onTabSelect={(_, d) => setCatFilter(d.value as 'all' | FeatureCategory)}
        >
          <Tab value="all">
            {lang === 'zh' ? `全部 (${counts.all})` : `All (${counts.all})`}
          </Tab>
          <Tab value="virtualization">
            {lang === 'zh' ? `虚拟化 / WSL (${counts.virtualization})` : `Virtualization (${counts.virtualization})`}
          </Tab>
          <Tab value="developer">
            {lang === 'zh' ? `开发组件 (${counts.developer})` : `Developer (${counts.developer})`}
          </Tab>
          <Tab value="security_ai">
            {lang === 'zh' ? `安全 / AI (${counts.security_ai})` : `Security (${counts.security_ai})`}
          </Tab>
          <Tab value="network_smb">
            {lang === 'zh' ? `网络共享 (${counts.network_smb})` : `Network (${counts.network_smb})`}
          </Tab>
          <Tab value="print_media">
            {lang === 'zh' ? `打印与传统 (${counts.print_media})` : `Print/Legacy (${counts.print_media})`}
          </Tab>
          <Tab value="iis_web">
            {lang === 'zh' ? `IIS / WCF (${counts.iis_web})` : `IIS/Web (${counts.iis_web})`}
          </Tab>
        </TabList>

        <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap' }}>
          <Button
            size="small"
            appearance={onlyEnabled ? 'primary' : 'outline'}
            onClick={() => setOnlyEnabled((v) => !v)}
          >
            {lang === 'zh' ? '仅看已启用' : 'Enabled Only'}
          </Button>
          <Input
            contentBefore={<Search20Regular />}
            placeholder={lang === 'zh' ? '搜索 WSL、NetFx3、SMB1...' : 'Search features...'}
            value={search}
            onChange={(_, d) => setSearch(d.value)}
            style={{ width: 220 }}
          />
          <Button size="small" icon={<ArrowClockwise20Regular />} onClick={load} disabled={loading}>
            {lang === 'zh' ? '刷新' : 'Refresh'}
          </Button>
        </div>
      </div>

      <div className={styles.listCard}>
        {loading ? (
          <div style={{ display: 'flex', justifyContent: 'center', paddingTop: 48 }}>
            <Spinner label={lang === 'zh' ? '正在读取 Windows 可选功能状态...' : 'Loading Windows Optional Features...'} />
          </div>
        ) : filtered.length === 0 ? (
          <Text size={300} style={{ color: tokens.colorNeutralForeground3, padding: 20 }}>
            {lang === 'zh' ? '当前筛选下没有匹配的 Windows 可选功能。' : 'No matching Windows features.'}
          </Text>
        ) : (
          filtered.map((feat) => (
            <div key={feat.name} className={styles.row}>
              <div style={{ display: 'flex', alignItems: 'center', gap: 10, minWidth: 0, flex: 1 }}>
                <PuzzlePieceRegular
                  style={{
                    fontSize: 20,
                    color: feat.enabled
                      ? tokens.colorBrandForeground1
                      : tokens.colorNeutralForeground3,
                    flexShrink: 0,
                  }}
                />
                <div style={{ display: 'flex', flexDirection: 'column', gap: 2, minWidth: 0, flex: 1 }}>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 6, flexWrap: 'wrap' }}>
                    <Text weight="semibold">{pickText(feat.display_name, lang)}</Text>
                    <Badge size="small" appearance="outline">
                      {feat.name}
                    </Badge>
                    {feat.high_impact && (
                      <Badge size="small" appearance="tint" color="warning">
                        {lang === 'zh' ? '高影响/隐私' : 'Notice'}
                      </Badge>
                    )}
                    <Badge
                      size="small"
                      appearance="tint"
                      color={feat.enabled ? 'success' : 'subtle'}
                    >
                      {feat.enabled
                        ? lang === 'zh'
                          ? '已开启'
                          : 'Enabled'
                        : lang === 'zh'
                        ? '已关闭'
                        : 'Disabled'}
                    </Badge>
                  </div>
                  <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                    {pickText(feat.description, lang)}
                  </Text>
                </div>
              </div>

              <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexShrink: 0 }}>
                {busyName === feat.name && <Spinner size="tiny" />}
                <Switch
                  checked={feat.enabled}
                  disabled={busyName !== null || Boolean(activeMaintenance)}
                  onChange={(_, d) => requestToggle(feat, d.checked)}
                />
              </div>
            </div>
          ))
        )}
      </div>

      <Dialog
        open={Boolean(pendingToggle)}
        onOpenChange={(_, d) => {
          if (!d.open) setPendingToggle(null);
        }}
      >
        <DialogSurface>
          <DialogBody>
            <DialogTitle>
              {lang === 'zh' ? '确认更改关键 Windows 可选功能？' : 'Confirm Critical Windows Feature Change'}
            </DialogTitle>
            <DialogContent>
              {pendingToggle && (
                <Text>
                  {lang === 'zh'
                    ? `即将${pendingToggle.nextEnabled ? '开启' : '关闭'}「${pickText(pendingToggle.feat.display_name, lang)}」(${pendingToggle.feat.name})。\n${pickText(pendingToggle.feat.description, lang)}\n${!pendingToggle.nextEnabled ? '注意：关闭虚拟化或运行时组件后，依赖它的 WSL2、Docker、模拟器或传统桌面软件将无法启动，且操作后可能需要重启电脑。' : '部分功能启用后需要重启 Windows 生效。'}`
                    : `${pendingToggle.nextEnabled ? 'Enable' : 'Disable'} ${pendingToggle.feat.name}? ${pickText(pendingToggle.feat.description, lang)}`}
                </Text>
              )}
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={() => setPendingToggle(null)}>
                {lang === 'zh' ? '取消' : 'Cancel'}
              </Button>
              <Button
                appearance="primary"
                onClick={() =>
                  pendingToggle && void handleToggle(pendingToggle.feat, pendingToggle.nextEnabled)
                }
              >
                {lang === 'zh' ? '确认修改功能' : 'Confirm Change'}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </div>
  );
};
