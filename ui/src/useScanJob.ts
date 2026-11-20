import { useEffect, useRef, useState } from 'react';
import { api, CleanupScanPayload, DiskAnalysisReport, DuplicateScanReport, PathRecyclePayload, PathRecyclePreview, ScanProgress, ScanRequest } from './api';

interface ScanResults {
  disk_analysis: DiskAnalysisReport;
  duplicate_files: DuplicateScanReport;
  cleanup_rules: CleanupScanPayload;
  recycle_review: PathRecyclePreview;
  recycle_path: PathRecyclePayload;
}

interface Session {
  id: number | null;
  cancelRequested: boolean;
  detached: boolean;
}

export interface ActiveScan {
  kind: ScanRequest['kind'];
  progress: ScanProgress | null;
  cancelling: boolean;
}

export function createScanStatusStore() {
  let current: ActiveScan | null = null;
  const listeners = new Set<() => void>();
  return {
    getSnapshot: () => current,
    subscribe(listener: () => void) {
      listeners.add(listener);
      return () => { listeners.delete(listener); };
    },
    publish(next: ActiveScan | null) {
      const previous = current;
      if (previous?.kind === next?.kind && previous?.cancelling === next?.cancelling
        && previous?.progress?.phase === next?.progress?.phase
        && previous?.progress?.processed_files === next?.progress?.processed_files
        && previous?.progress?.total_files === next?.progress?.total_files
        && previous?.progress?.bytes_read === next?.progress?.bytes_read
        && previous?.progress?.can_cancel === next?.progress?.can_cancel) return;
      current = next;
      listeners.forEach((listener) => listener());
    },
  };
}

export type ScanStatusStore = ReturnType<typeof createScanStatusStore>;

export function useScanJob() {
  const current = useRef<Session | null>(null);
  const [active, setActive] = useState<{ kind: ScanRequest['kind'] } | null>(null);
  const [cancelled, setCancelled] = useState(false);
  const [statusStore] = useState(createScanStatusStore);

  useEffect(() => () => {
    const session = current.current;
    if (session) {
      session.detached = true;
      session.cancelRequested = true;
    }
  }, []);

  const cancel = () => {
    if (current.current) {
      current.current.cancelRequested = true;
      const scan = statusStore.getSnapshot();
      if (scan) statusStore.publish({ ...scan, cancelling: true });
    }
  };

  async function run<T extends ScanRequest>(request: T): Promise<ScanResults[T['kind']] | undefined> {
    if (current.current) return;
    const session: Session = { id: null, cancelRequested: false, detached: false };
    current.current = session;
    setCancelled(false);
    setActive({ kind: request.kind });
    statusStore.publish({ kind: request.kind, progress: null, cancelling: false });
    try {
      const { id } = await api.startScan(request);
      session.id = id;
      let cancelSent = false;
      while (true) {
        if (session.cancelRequested && !cancelSent) {
          await api.cancelScan(id);
          cancelSent = true;
        }
        const status = await api.getScanStatus<ScanResults[T['kind']]>(id);
        if (status.state === 'cancelled'
          || (status.state === 'completed' && session.cancelRequested && request.kind !== 'recycle_path')) {
          if (!session.detached) setCancelled(true);
          return;
        }
        if (status.state === 'completed') return status.result;
        if (status.state === 'failed') throw new Error(status.error);
        if (!session.detached) {
          statusStore.publish({
            kind: request.kind,
            progress: status.progress,
            cancelling: status.state === 'cancelling' || (session.cancelRequested && status.progress.can_cancel),
          });
        }
        await new Promise((resolve) => setTimeout(resolve, 200));
      }
    } catch (error) {
      if (session.id !== null) void api.cancelScan(session.id).catch(() => {});
      if (!session.detached) throw error;
    } finally {
      current.current = null;
      if (!session.detached) {
        statusStore.publish(null);
        setActive(null);
      }
    }
  }

  return { active, statusStore, cancelled, cancel, run };
}
