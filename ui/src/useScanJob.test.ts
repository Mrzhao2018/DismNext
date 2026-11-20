import { expect, it, vi } from 'vitest';
import { createScanStatusStore } from './useScanJob';

it('publishes changed progress without notifying on identical polling results', () => {
  const store = createScanStatusStore();
  const observe = vi.fn();
  const unsubscribe = store.subscribe(observe);
  const status = { kind: 'cleanup_rules', progress: null, cancelling: false } as const;
  store.publish(status);
  store.publish({ ...status });
  expect(observe).toHaveBeenCalledTimes(1);
  store.publish({
    ...status,
    progress: { phase: 'walking', processed_files: 2, total_files: null, bytes_read: 0, can_cancel: true },
  });
  expect(store.getSnapshot()?.progress?.processed_files).toBe(2);
  expect(observe).toHaveBeenCalledTimes(2);
  unsubscribe();
  store.publish(null);
  expect(observe).toHaveBeenCalledTimes(2);
});
