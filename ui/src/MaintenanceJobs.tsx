import { createContext, useCallback, useContext, useMemo, useRef, useState, type ReactNode } from 'react';
import { Button, Spinner, Text } from '@fluentui/react-components';
import { api, type Lang, type MaintenanceProgress, type MaintenanceRequest, type MaintenanceResults } from './api';

interface ActiveJob {
  title: string;
  progress: MaintenanceProgress | null;
  cancelling: boolean;
}

interface JobStatus {
  active: ActiveJob | null;
  notice: { title: string; state: 'completed' | 'cancelled' | 'failed'; error?: string } | null;
  dismiss: () => void;
  cancel: () => void;
}

interface JobActions {
  active: boolean;
  run: <T extends MaintenanceRequest>(request: T, title: string) => Promise<MaintenanceResults[T['kind']] | undefined>;
}

const MaintenanceContext = createContext<JobActions | null>(null);
const MaintenanceStatusContext = createContext<JobStatus | null>(null);

export function MaintenanceProvider({ children }: { children: ReactNode }) {
  const job = useRef<{ cancelRequested: boolean } | null>(null);
  const [active, setActive] = useState<ActiveJob | null>(null);
  const [notice, setNotice] = useState<JobStatus['notice']>(null);

  const cancel = () => {
    if (!job.current) return;
    job.current.cancelRequested = true;
    setActive((current) => current && { ...current, cancelling: true });
  };

  const run = useCallback(async <T extends MaintenanceRequest>(request: T, title: string): Promise<MaintenanceResults[T['kind']] | undefined> => {
    if (job.current) throw new Error('Another maintenance task is already running');
    const session = { cancelRequested: false };
    job.current = session;
    setNotice(null);
    setActive({ title, progress: null, cancelling: false });
    try {
      const { id } = await api.startMaintenance(request);
      let cancelSent = false;
      for (;;) {
        if (session.cancelRequested && !cancelSent) {
          await api.cancelMaintenance(id);
          cancelSent = true;
        }
        const status = await api.getMaintenanceStatus<MaintenanceResults[T['kind']]>(id);
        if (status.state === 'completed') {
          setNotice({ title, state: 'completed' });
          return status.result;
        }
        if (status.state === 'cancelled') {
          setNotice({ title, state: 'cancelled' });
          return;
        }
        if (status.state === 'failed') throw new Error(status.error);
        setActive({ title, progress: status.progress, cancelling: session.cancelRequested || status.state === 'cancelling' });
        await new Promise((resolve) => setTimeout(resolve, 400));
      }
    } catch (error) {
      setNotice({ title, state: 'failed', error: String(error) });
      throw error;
    } finally {
      job.current = null;
      setActive(null);
    }
  }, []);
  const busy = active !== null;
  const actions = useMemo(() => ({ active: busy, run }), [busy, run]);

  return <MaintenanceContext.Provider value={actions}>
    <MaintenanceStatusContext.Provider value={{ active, notice, dismiss: () => setNotice(null), cancel }}>
      {children}
    </MaintenanceStatusContext.Provider>
  </MaintenanceContext.Provider>;
}

export function useMaintenance() {
  const context = useContext(MaintenanceContext);
  if (!context) throw new Error('MaintenanceProvider is missing');
  return context;
}

export function MaintenanceStatusBar({ lang }: { lang: Lang }) {
  const context = useContext(MaintenanceStatusContext);
  if (!context) throw new Error('MaintenanceProvider is missing');
  const { active, notice, cancel, dismiss } = context;
  const zh = lang === 'zh';
  if (!active) {
    if (!notice) return null;
    const status = {
      completed: zh ? '已完成' : 'Completed',
      cancelled: zh ? '已取消，已完成的更改会保留' : 'Cancelled. Changes already applied remain in effect.',
      failed: zh ? '失败' : 'Failed',
    }[notice.state];
    return <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
      <Text role="status" size={200}>{notice.title} · {status}{notice.error ? ` · ${notice.error}` : ''}</Text>
      <Button size="small" onClick={dismiss}>{zh ? '关闭' : 'Dismiss'}</Button>
    </div>;
  }

  const percent = active.progress?.percent;
  const status = active.cancelling ? (zh ? '正在取消…' : 'Cancelling…') : (zh ? '正在运行…' : 'Running…');
  return <div>
    <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
      {percent == null ? <Spinner size="tiny" /> : <progress max={100} value={percent} aria-label={active.title} />}
      <Text role="status" size={200}>{active.title} · {status}{percent != null ? ` ${percent}%` : ''}</Text>
      <Button size="small" disabled={active.cancelling} onClick={cancel}>{zh ? '取消任务' : 'Cancel task'}</Button>
    </div>
    {active.progress?.output && <details>
      <summary>{zh ? '实时输出' : 'Live output'}{active.progress.program ? ` · ${active.progress.program}` : ''}</summary>
      <pre style={{ whiteSpace: 'pre-wrap', overflow: 'auto', maxHeight: 180 }}>{active.progress.output.replace(/\r(?!\n)/g, '\n')}</pre>
    </details>}
  </div>;
}
