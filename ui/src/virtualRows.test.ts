import { expect, it } from 'vitest';
import { visibleRowRange } from './virtualRows';

it('only displays the viewport and buffer even for a large expanded tree', () => {
  expect(visibleRowRange(50_000, 28, 28_000, 280, 6)).toEqual({ start: 994, end: 1016 });
});

it('clamps to the remaining rows if a branch is collapsed at the bottom', () => {
  expect(visibleRowRange(30, 28, 28_000, 280, 6)).toEqual({ start: 14, end: 30 });
  expect(visibleRowRange(0, 28, 500, 280, 6)).toEqual({ start: 0, end: 0 });
});
