import { useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react';

function findRow(offsets: number[], position: number) {
  let low = 0;
  let high = offsets.length - 1;
  while (low < high) {
    const middle = Math.floor((low + high + 1) / 2);
    if (offsets[middle] <= position) low = middle;
    else high = middle - 1;
  }
  return Math.min(low, offsets.length - 2);
}

interface Props<T> {
  rows: readonly T[];
  getKey: (row: T) => string;
  renderRow: (row: T) => ReactNode;
  label: string;
  estimate?: number;
  gap?: number;
}

export function VirtualList<T>({ rows, getKey, renderRow, label, estimate = 76, gap = 8 }: Props<T>) {
  const viewport = useRef<HTMLDivElement>(null);
  const widths = useRef(0);
  const measured = useRef(new Map<string, number>());
  const [revision, setRevision] = useState(0);
  const [frame, setFrame] = useState({ top: 0, height: 0 });
  const [focused, setFocused] = useState<string | null>(null);
  const keys = useMemo(() => rows.map(getKey), [rows, getKey]);
  const offsets = useMemo(() => {
    void revision;
    const cumulative = [0];
    for (const key of keys) {
      cumulative.push(cumulative[cumulative.length - 1] + (measured.current.get(key) ?? estimate + gap));
    }
    return cumulative;
  }, [keys, estimate, gap, revision]);
  const start = Math.max(0, findRow(offsets, frame.top) - 4);
  const end = Math.min(rows.length, findRow(offsets, frame.top + frame.height) + 5);
  const focusedIndex = focused === null ? -1 : keys.indexOf(focused);

  useLayoutEffect(() => {
    const element = viewport.current;
    if (!element) return;
    const resize = () => {
      if (element.clientWidth > 0 && element.clientWidth !== widths.current) {
        widths.current = element.clientWidth;
        measured.current.clear();
        setRevision((value) => value + 1);
      }
      setFrame({ top: element.scrollTop, height: element.clientHeight });
    };
    const observer = new ResizeObserver(resize);
    observer.observe(element);
    resize();
    return () => observer.disconnect();
  }, []);

  useLayoutEffect(() => {
    const element = viewport.current;
    if (!element) return;
    const observer = new ResizeObserver((entries) => {
      let changed = false;
      for (const entry of entries) {
        const child = entry.target as HTMLElement;
        const key = child.dataset.rowKey;
        const height = child.offsetHeight;
        if (key !== undefined && height > 0 && measured.current.get(key) !== height) {
          measured.current.set(key, height);
          changed = true;
        }
      }
      if (changed) setRevision((value) => value + 1);
    });
    for (const child of element.querySelectorAll('[data-virtual-row]')) observer.observe(child);
    return () => observer.disconnect();
  }, [keys, start, end, focusedIndex]);

  const render = (index: number) => (
    <div
      key={keys[index]}
      role="listitem"
      aria-posinset={index + 1}
      aria-setsize={rows.length}
      data-virtual-row=""
      data-row-key={keys[index]}
      onFocus={() => setFocused(keys[index])}
      style={{ position: 'absolute', top: offsets[index], insetInline: 0, paddingBottom: gap }}
    >
      {renderRow(rows[index])}
    </div>
  );

  return (
    <div
      ref={viewport}
      role="list"
      aria-label={label}
      tabIndex={0}
      style={{ flex: '1 1 0', minHeight: 0, overflowY: 'auto', overflowAnchor: 'none' }}
      onScroll={({ currentTarget }) => setFrame({
        top: currentTarget.scrollTop, height: currentTarget.clientHeight,
      })}
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) setFocused(null);
      }}
    >
      <div style={{ position: 'relative', height: offsets[offsets.length - 1] }}>
        {focusedIndex >= 0 && focusedIndex < start && render(focusedIndex)}
        {rows.slice(start, end).map((_, index) => render(start + index))}
        {focusedIndex >= end && focusedIndex >= 0 && render(focusedIndex)}
      </div>
    </div>
  );
}
