export function visibleRowRange(
  count: number, rowHeight: number, scrollTop: number, viewportHeight: number, overscan: number
) {
  const top = Math.max(0, Math.min(scrollTop, count * rowHeight - viewportHeight));
  return {
    start: Math.max(0, Math.floor(top / rowHeight) - overscan),
    end: Math.min(count, Math.ceil((top + viewportHeight) / rowHeight) + overscan),
  };
}
