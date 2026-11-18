import React, { useEffect, useState } from 'react';
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
  Text,
  makeStyles,
  tokens,
} from '@fluentui/react-components';
import {
  ArrowClockwise20Regular,
  History24Regular,
  PlayRegular,
  ShieldCheckmark24Regular,
  ToolboxRegular,
  Warning16Regular,
} from '@fluentui/react-icons';
import { Lang, RestorePointInfo, api } from '../api';

interface Props {
  lang: Lang;
  isAdmin: boolean;
}

interface RescueCardDef {
  id: string;
  zhTitle: string;
  enTitle: string;
  zhDesc: string;
  enDesc: string;
  requiresAdmin: boolean;
  heavyConfirm: boolean;
}

const RESCUE_CARDS: RescueCardDef[] = [
  {
    id: 'rebuild_icon_cache',
    zhTitle: '重建图标与缩略图缓存',
    enTitle: 'Rebuild Icon & Thumbnail Cache',
    zhDesc: '解决桌面或任务栏图标变白块、预览图错乱的问题，并立即通知资源管理器刷新。',
    enDesc: 'Clears unlocked icon/thumb cache databases and notifies Windows Shell to refresh icons.',
    requiresAdmin: false,
    heavyConfirm: false,
  },
  {
    id: 'flush_dns_network',
    zhTitle: 'DNS 缓存清理与网络栈重置',
    enTitle: 'Flush DNS & Reset Winsock',
    zhDesc: '执行 ipconfig /flushdns 与 netsh winsock reset，修复代理断开后的断网/无法解析域名问题。',
    enDesc: 'Flushes DNS resolver cache and resets Winsock catalog (Admin for full Winsock reset).',
    requiresAdmin: false,
    heavyConfirm: true,
  },
  {
    id: 'reset_windows_update',
    zhTitle: '重置 Windows Update 下载缓存',
    enTitle: 'Reset Windows Update Cache',
    zhDesc: '自动停止 wuauserv 与 BITS 服务，清空卡死的 SoftwareDistribution\\Download 后重新启动更新服务。',
    enDesc: 'Restarts wuauserv & BITS services and purges stuck SoftwareDistribution\\Download files.',
    requiresAdmin: true,
    heavyConfirm: true,
  },
  {
    id: 'clear_print_spooler',
    zhTitle: '清空卡死的打印队列',
    enTitle: 'Clear Stuck Print Spooler',
    zhDesc: '停止 Spooler 打印服务并清理阻塞的打印作业缓存文件，恢复打印机响应。',
    enDesc: 'Restarts Print Spooler service and purges stuck jobs under System32\\spool\\PRINTERS.',
    requiresAdmin: true,
    heavyConfirm: true,
  },
  {
    id: 'dism_check_health',
    zhTitle: 'DISM 系统映像健康检查',
    enTitle: 'DISM /CheckHealth',
    zhDesc: '快速查询 Windows 组件存储是否存在已标记损坏，耗时极短，不下载修复文件。',
    enDesc: 'Quickly checks the component store health flag via DISM /Online /CheckHealth.',
    requiresAdmin: true,
    heavyConfirm: false,
  },
  {
    id: 'dism_restore_health',
    zhTitle: 'DISM 系统映像在线修复',
    enTitle: 'DISM /RestoreHealth',
    zhDesc: '从微软更新服务器拉取健康系统组件修复受损的 Windows 映像（耗时数分钟）。',
    enDesc: 'Scans and repairs corrupted Windows component store files via DISM /RestoreHealth.',
    requiresAdmin: true,
    heavyConfirm: true,
  },
  {
    id: 'sfc_scannow',
    zhTitle: 'SFC 系统核心文件校验与修复',
    enTitle: 'SFC /scannow',
    zhDesc: '全盘验证受保护 Windows 核心系统文件完整性并自动替换被篡改或损坏的文件。',
    enDesc: 'Scans integrity of all protected Windows system files and repairs mismatches.',
    requiresAdmin: true,
    heavyConfirm: true,
  },
];

const useStyles = makeStyles({
  root: {
    display: 'flex',
    flexDirection: 'column',
    height: '100%',
    gap: '12px',
    overflowY: 'auto',
    paddingRight: '4px',
  },
  restoreCard: {
    padding: '14px 18px',
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    display: 'flex',
    flexDirection: 'column',
    gap: '10px',
    flexShrink: 0,
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
    minHeight: '155px',
  },
});

export const RescueView: React.FC<Props> = ({ lang, isAdmin }) => {
  const styles = useStyles();
  const [points, setPoints] = useState<RestorePointInfo[]>([]);
  const [loadingPoints, setLoadingPoints] = useState(true);
  const [desc, setDesc] = useState(() =>
    lang === 'zh' ? 'DismNext 维护前安全还原点' : 'DismNext Pre-Maintenance Checkpoint'
  );
  const [creatingPoint, setCreatingPoint] = useState(false);
  const [runningId, setRunningId] = useState<string | null>(null);
  const [pendingCard, setPendingCard] = useState<RescueCardDef | null>(null);
  const [outputLog, setOutputLog] = useState<string | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const loadPoints = async () => {
    setLoadingPoints(true);
    try {
      const res = await api.getRestorePoints();
      setPoints(res.points);
    } catch {
      // Non-blocking if restore points cannot be enumerated
    } finally {
      setLoadingPoints(false);
    }
  };

  useEffect(() => {
    void loadPoints();
  }, []);

  const handleCreatePoint = async () => {
    setCreatingPoint(true);
    setErrorMsg(null);
    setOutputLog(null);
    try {
      const res = await api.createRestorePoint(desc);
      setPoints(res.points);
      setOutputLog(res.message);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setCreatingPoint(false);
    }
  };

  const handleRunAction = async (card: RescueCardDef) => {
    setPendingCard(null);
    setRunningId(card.id);
    setErrorMsg(null);
    setOutputLog(null);
    try {
      const res = await api.runRescueAction(card.id);
      setOutputLog(
        `${res.output}${res.requires_restart ? (lang === 'zh' ? '\n（建议稍后重启电脑以完全生效）' : '\n(Restart recommended)') : ''}`
      );
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setRunningId(null);
    }
  };

  return (
    <div className={styles.root}>
      {!isAdmin && (
        <MessageBar intent="warning">
          <MessageBarBody>
            <Warning16Regular style={{ verticalAlign: 'middle', marginRight: 6 }} />
            {lang === 'zh'
              ? '当前为标准用户模式，“重建图标缓存”和“刷新 DNS”可直接运行；创建系统还原点、DISM 修复、SFC 扫描、重置更新服务需先点击顶部“一键提权为管理员”。'
              : 'Icon cache & DNS flush work in standard mode; Restore Point creation, DISM, SFC, and service resets require Administrator privileges.'}
          </MessageBarBody>
        </MessageBar>
      )}
      {errorMsg && (
        <MessageBar intent="error">
          <MessageBarBody>{errorMsg}</MessageBarBody>
        </MessageBar>
      )}
      {outputLog && (
        <MessageBar intent="success">
          <MessageBarBody>
            <Text style={{ fontFamily: 'Consolas, monospace', whiteSpace: 'pre-wrap' }} size={200}>
              {outputLog}
            </Text>
          </MessageBarBody>
        </MessageBar>
      )}

      <div className={styles.restoreCard}>
        <div
          style={{
            display: 'flex',
            justifyContent: 'space-between',
            alignItems: 'center',
            flexWrap: 'wrap',
            gap: 10,
          }}
        >
          <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <History24Regular style={{ color: tokens.colorBrandForeground1 }} />
            <Text size={400} weight="semibold">
              {lang === 'zh' ? 'Windows 系统还原点管理' : 'Windows System Restore Points'}
            </Text>
            <Badge appearance="tint" color="informative">
              {points.length} {lang === 'zh' ? '个可用还原点' : 'available'}
            </Badge>
          </div>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
            <Input
              size="small"
              value={desc}
              onChange={(_, d) => setDesc(d.value)}
              style={{ width: 260 }}
              placeholder={lang === 'zh' ? '还原点备注说明...' : 'Restore point description...'}
            />
            <Button
              size="small"
              appearance="primary"
              icon={creatingPoint ? <Spinner size="tiny" /> : <ShieldCheckmark24Regular />}
              disabled={creatingPoint || Boolean(runningId)}
              onClick={handleCreatePoint}
            >
              {creatingPoint
                ? lang === 'zh'
                  ? '正在创建...'
                  : 'Creating...'
                : lang === 'zh'
                ? '一键创建系统还原点'
                : 'Create Restore Point'}
            </Button>
            <Button
              size="small"
              icon={<ArrowClockwise20Regular />}
              disabled={loadingPoints}
              onClick={loadPoints}
            >
              {lang === 'zh' ? '刷新' : 'Refresh'}
            </Button>
          </div>
        </div>

        {loadingPoints ? (
          <Spinner size="tiny" label={lang === 'zh' ? '正在查询系统还原点...' : 'Querying restore points...'} />
        ) : points.length === 0 ? (
          <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
            {lang === 'zh'
              ? '当前未检出系统还原点（可能未开启系统保护或需要管理员权限查看）。'
              : 'No system restore points found (ensure System Protection is enabled or elevate).'}
          </Text>
        ) : (
          <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8 }}>
            {points.slice(0, 6).map((pt) => (
              <Badge key={pt.sequence_number} appearance="outline" size="large">
                #{pt.sequence_number} · {pt.description} ({pt.creation_time})
              </Badge>
            ))}
          </div>
        )}
      </div>

      <div className={styles.grid}>
        {RESCUE_CARDS.map((card) => (
          <div key={card.id} className={styles.card}>
            <div>
              <div
                style={{
                  display: 'flex',
                  justifyContent: 'space-between',
                  alignItems: 'center',
                  gap: 8,
                }}
              >
                <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <ToolboxRegular style={{ fontSize: 20, color: tokens.colorBrandForeground1 }} />
                  <Text size={400} weight="semibold">
                    {lang === 'zh' ? card.zhTitle : card.enTitle}
                  </Text>
                </div>
                {card.requiresAdmin && (
                  <Badge size="small" appearance="tint" color={isAdmin ? 'success' : 'warning'}>
                    Admin
                  </Badge>
                )}
              </div>
              <Text
                size={200}
                block
                style={{ marginTop: 8, color: tokens.colorNeutralForeground3 }}
              >
                {lang === 'zh' ? card.zhDesc : card.enDesc}
              </Text>
            </div>
            <Button
              appearance="outline"
              icon={runningId === card.id ? <Spinner size="tiny" /> : <PlayRegular />}
              disabled={Boolean(runningId) || creatingPoint}
              onClick={() => {
                if (card.heavyConfirm) setPendingCard(card);
                else void handleRunAction(card);
              }}
            >
              {runningId === card.id
                ? lang === 'zh'
                  ? '正在执行...'
                  : 'Running...'
                : lang === 'zh'
                ? '立即执行'
                : 'Run Tool'}
            </Button>
          </div>
        ))}
      </div>

      <Dialog
        open={Boolean(pendingCard)}
        onOpenChange={(_, d) => {
          if (!d.open) setPendingCard(null);
        }}
      >
        <DialogSurface>
          <DialogBody>
            <DialogTitle>
              {pendingCard
                ? lang === 'zh'
                  ? `确认执行：${pendingCard.zhTitle}？`
                  : `Confirm: ${pendingCard.enTitle}?`
                : ''}
            </DialogTitle>
            <DialogContent>
              <Text>
                {pendingCard ? (lang === 'zh' ? pendingCard.zhDesc : pendingCard.enDesc) : ''}
              </Text>
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={() => setPendingCard(null)}>
                {lang === 'zh' ? '取消' : 'Cancel'}
              </Button>
              <Button
                appearance="primary"
                onClick={() => pendingCard && void handleRunAction(pendingCard)}
              >
                {lang === 'zh' ? '确认执行' : 'Confirm Run'}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </div>
  );
};
