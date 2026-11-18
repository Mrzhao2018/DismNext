import React, { useEffect, useMemo, useState } from 'react';
import {
  Badge,
  Button,
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
  FolderOpen16Regular,
  Rocket24Regular,
  Search20Regular,
  Warning16Regular,
} from '@fluentui/react-icons';
import { Lang, StartupItem, api } from '../api';

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

export const StartupView: React.FC<Props> = ({ lang, isAdmin }) => {
  const styles = useStyles();
  const [items, setItems] = useState<StartupItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [scopeFilter, setScopeFilter] = useState<'all' | 'user' | 'system'>('all');
  const [onlyEnabled, setOnlyEnabled] = useState(false);
  const [search, setSearch] = useState('');
  const [busyId, setBusyId] = useState<string | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const load = async () => {
    setLoading(true);
    setErrorMsg(null);
    try {
      const res = await api.getStartupItems();
      setItems(res.items);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, []);

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
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
  }, [items, scopeFilter, onlyEnabled, search]);

  const enabledCount = useMemo(() => items.filter((i) => i.enabled).length, [items]);

  const handleToggle = async (it: StartupItem, nextEnabled: boolean) => {
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

  return (
    <div className={styles.root}>
      {errorMsg && (
        <MessageBar intent="error">
          <MessageBarBody>{errorMsg}</MessageBarBody>
        </MessageBar>
      )}

      <div className={styles.toolbar}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap' }}>
          <Rocket24Regular style={{ color: tokens.colorBrandForeground1 }} />
          <TabList
            size="small"
            selectedValue={scopeFilter}
            onTabSelect={(_, d) => setScopeFilter(d.value as 'all' | 'user' | 'system')}
          >
            <Tab value="all">
              {lang === 'zh' ? `全部 (${items.length})` : `All (${items.length})`}
            </Tab>
            <Tab value="user">
              {lang === 'zh'
                ? `当前用户 (${items.filter((i) => i.scope === 'user').length})`
                : `User (${items.filter((i) => i.scope === 'user').length})`}
            </Tab>
            <Tab value="system">
              {lang === 'zh'
                ? `系统全局 (${items.filter((i) => i.scope === 'system').length})`
                : `System (${items.filter((i) => i.scope === 'system').length})`}
            </Tab>
          </TabList>
          <Badge appearance="tint" color="success">
            {lang === 'zh' ? `已启用 ${enabledCount}` : `${enabledCount} Enabled`}
          </Badge>
        </div>

        <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap' }}>
          <Switch
            checked={onlyEnabled}
            onChange={(_, d) => setOnlyEnabled(d.checked)}
            label={lang === 'zh' ? '仅看已启用' : 'Enabled only'}
          />
          <Input
            size="small"
            contentBefore={<Search20Regular />}
            placeholder={lang === 'zh' ? '搜索启动项或命令...' : 'Search startup items...'}
            value={search}
            onChange={(_, d) => setSearch(d.value)}
            style={{ width: 210 }}
          />
          <Button size="small" icon={<ArrowClockwise20Regular />} onClick={load}>
            {lang === 'zh' ? '刷新' : 'Refresh'}
          </Button>
        </div>
      </div>

      <div className={styles.listCard}>
        {loading ? (
          <div style={{ padding: 40, display: 'flex', justifyContent: 'center' }}>
            <Spinner label={lang === 'zh' ? '正在枚举开机启动项...' : 'Loading startup items...'} />
          </div>
        ) : filtered.length === 0 ? (
          <Text style={{ padding: 20, color: tokens.colorNeutralForeground3 }}>
            {lang === 'zh' ? '当前筛选下没有启动项。' : 'No matching startup items found.'}
          </Text>
        ) : (
          filtered.map((it) => {
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
                        {lang === 'zh' ? '目标文件缺失' : 'Target Missing'}
                      </Badge>
                    )}
                    {lockedByPriv && (
                      <Badge size="small" appearance="outline" color="warning">
                        {lang === 'zh' ? '修改需管理员权限' : 'Admin required to modify'}
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
                  {it.target_path && it.target_exists && (
                    <Button
                      size="small"
                      appearance="subtle"
                      icon={<FolderOpen16Regular />}
                      onClick={() => api.revealInExplorer(it.target_path!)}
                    >
                      {lang === 'zh' ? '定位' : 'Reveal'}
                    </Button>
                  )}
                  <Switch
                    checked={it.enabled}
                    disabled={busyId === it.id || lockedByPriv}
                    onChange={(_, d) => handleToggle(it, d.checked)}
                  />
                </div>
              </div>
            );
          })
        )}
      </div>
    </div>
  );
};
