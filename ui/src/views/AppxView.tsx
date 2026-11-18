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
  FolderOpen16Regular,
  LockClosed16Regular,
  Search20Regular,
} from '@fluentui/react-icons';
import { AppxCategory, AppxPackageInfo, Lang, api, pickText } from '../api';

interface Props {
  lang: Lang;
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
});

export const AppxView: React.FC<Props> = ({ lang }) => {
  const styles = useStyles();
  const [packages, setPackages] = useState<AppxPackageInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [cat, setCat] = useState<'all' | AppxCategory>('bloatware');
  const [search, setSearch] = useState('');
  const [confirmPkg, setConfirmPkg] = useState<AppxPackageInfo | null>(null);
  const [removing, setRemoving] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const load = async () => {
    setLoading(true);
    setErrorMsg(null);
    try {
      const res = await api.getAppxPackages();
      setPackages(res.packages);
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

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
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
  }, [packages, cat, search]);

  const handleConfirmRemove = async () => {
    if (!confirmPkg) return;
    setRemoving(true);
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
      setRemoving(false);
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

      <div className={styles.toolbar}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap' }}>
          <Apps24Regular style={{ color: tokens.colorBrandForeground1 }} />
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

        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <Input
            size="small"
            contentBefore={<Search20Regular />}
            placeholder={lang === 'zh' ? '搜索应用名称或包名...' : 'Search packages...'}
            value={search}
            onChange={(_, d) => setSearch(d.value)}
            style={{ width: 220 }}
          />
          <Button size="small" icon={<ArrowClockwise20Regular />} onClick={load}>
            {lang === 'zh' ? '刷新' : 'Refresh'}
          </Button>
        </div>
      </div>

      <div className={styles.listCard}>
        {loading ? (
          <div style={{ padding: 40, display: 'flex', justifyContent: 'center' }}>
            <Spinner label={lang === 'zh' ? '正在读取已安装的现代应用...' : 'Loading packages...'} />
          </div>
        ) : filtered.length === 0 ? (
          <Text style={{ padding: 20, color: tokens.colorNeutralForeground3 }}>
            {lang === 'zh' ? '当前分类下没有匹配的应用。' : 'No matching packages found.'}
          </Text>
        ) : (
          filtered.map((pkg) => (
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
                    onClick={() => api.revealInExplorer(pkg.install_location)}
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
          ))
        )}
      </div>

      <Dialog
        open={Boolean(confirmPkg)}
        onOpenChange={(_, d) => {
          if (!d.open && !removing) setConfirmPkg(null);
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
              <Button appearance="secondary" disabled={removing} onClick={() => setConfirmPkg(null)}>
                {lang === 'zh' ? '取消' : 'Cancel'}
              </Button>
              <Button appearance="primary" disabled={removing} onClick={handleConfirmRemove}>
                {removing
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
