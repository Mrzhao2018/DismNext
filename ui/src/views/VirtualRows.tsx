import { useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { visibleRowRange } from '../virtualRows';

interface Props<T> {
  rows: readonly T[];
  rowHeight: number;
  className: string;
  revealIndex: number;
  onReveal: () => void;
  renderRow: (row: T) => ReactNode;
}

export function VirtualRows<T>({ rows, rowHeight, className, revealIndex, onReveal, renderRow }: Props<T>) {
  const container = useRef<HTMLDivElement>(null);
  const [viewport, setViewport] = useState({ top: 0, height: 0 });

  useLayoutEffect(() => {
    const element = container.current;
    if (!element) return;
    const measure = () => setViewport({ top: element.scrollTop, height: element.clientHeight });
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    measure();
    return () => observer.disconnect();
  }, []);

  useLayoutEffect(() => {
    const element = container.current;
    if (!element || revealIndex < 0 || element.clientHeight === 0) return;
    const top = revealIndex * rowHeight;
    if (top < element.scrollTop) {
      element.scrollTop = top;
    } else if (top + rowHeight > element.scrollTop + element.clientHeight) {
      element.scrollTop = top + rowHeight - element.clientHeight;
    }
    setViewport({ top: element.scrollTop, height: element.clientHeight });
    onReveal();
  }, [onReveal, revealIndex, rowHeight, viewport.height]);

  const { start, end } = visibleRowRange(rows.length, rowHeight, viewport.top, viewport.height, 6);
  return (
    <div
      ref={container}
      className={className}
      style={{ overflowAnchor: 'none' }}
      onScroll={({ currentTarget }) => setViewport({ top: currentTarget.scrollTop, height: currentTarget.clientHeight })}
    >
      <div aria-hidden="true" style={{ height: start * rowHeight }} />
      {rows.slice(start, end).map(renderRow)}
      <div aria-hidden="true" style={{ height: (rows.length - end) * rowHeight }} />
    </div>
  );
}
