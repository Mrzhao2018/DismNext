import { expect, it } from 'vitest';
import { calculateSelectedBytes } from './cleanupSelection';

const summary = {
  cleanable_bytes: 210,
  overlaps: [{ rule_ids: ['a', 'b', 'c'], cleanable_bytes: 45 }],
};
const entries = ['a', 'b', 'c'].map((id) => ({ rule: { id, cleanable_bytes: 100 } }));

it('deducts shared bytes once for each additional selected rule', () => {
  expect(calculateSelectedBytes(entries.slice(0, 1), summary)).toBe(100);
  expect(calculateSelectedBytes(entries.slice(0, 2), summary)).toBe(155);
  expect(calculateSelectedBytes(entries, summary)).toBe(210);
});

it('distinguishes an empty selection from a pending scan', () => {
  expect(calculateSelectedBytes([], null)).toBe(0);
  expect(calculateSelectedBytes(entries, null)).toBeNull();
});
