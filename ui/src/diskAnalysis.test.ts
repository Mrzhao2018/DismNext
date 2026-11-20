import { describe, expect, it } from 'vitest';
import type { DiskTreeNode } from './api';
import { removeNodeFromTree, sameOrBelow, squarifyRects } from './diskAnalysis';

function node(path: string, size: number, fields: Partial<DiskTreeNode> = {}): DiskTreeNode {
  return {
    name: path.slice(path.lastIndexOf('/') + 1),
    path,
    is_dir: false,
    is_aggregate: false,
    is_cache_hint: false,
    aggregate_count: 0,
    size_bytes: size,
    file_count: 1,
    dir_count: 0,
    ext: null,
    children: [],
    ...fields,
  };
}

describe('treemap geometry', () => {
  it('assigns proportional areas inside the original bounds', () => {
    const nodes = [node('large', 50), node('medium', 25), node('small', 25), node('empty', 0)];
    const rects = squarifyRects(nodes, 4, 7, 80, 50);
    expect(rects.map(({ node }) => node.path)).toEqual(['large', 'medium', 'small']);
    for (const rect of rects) {
      expect(rect.x).toBeGreaterThanOrEqual(4);
      expect(rect.y).toBeGreaterThanOrEqual(7);
      expect(rect.x + rect.w).toBeLessThanOrEqual(84.000001);
      expect(rect.y + rect.h).toBeLessThanOrEqual(57.000001);
      expect(rect.w * rect.h).toBeCloseTo(rect.node.size_bytes * 40);
    }
    expect(squarifyRects(nodes, 0, 0, 0, 30)).toEqual([]);
  });
});

describe('removing recycled nodes', () => {
  it('normalizes Windows paths while requiring a directory boundary', () => {
    expect(sameOrBelow('c:/Cache/tmp.log', 'C:\\CACHE\\')).toBe(true);
    expect(sameOrBelow('c:/Cache', 'C:\\Cache\\')).toBe(true);
    expect(sameOrBelow('C:/CacheBackup/file', 'C:/Cache')).toBe(false);
  });

  it('updates ancestor totals and preserves aggregate entries', () => {
    const file = node('C:/root/cache/file', 20);
    const aggregate = node('C:/root/cache/file', 15, { is_aggregate: true, file_count: 2 });
    const dir = node('C:/root/cache', 35, {
      is_dir: true,
      children: [file, aggregate],
      file_count: 3,
    });
    const root = node('C:/root', 35, {
      is_dir: true,
      children: [dir],
      file_count: 3,
      dir_count: 1,
    });

    const updated = removeNodeFromTree(root, 'c:\\ROOT\\cache\\file');
    expect(updated).toMatchObject({ removedBytes: 20, removedFiles: 1, removedDirs: 0 });
    expect(updated.updated).toMatchObject({ size_bytes: 15, file_count: 2, dir_count: 1 });
    expect(updated.updated.children[0].children).toEqual([aggregate]);
    expect(root.children[0].children).toEqual([file, aggregate]);

    const withoutDir = removeNodeFromTree(updated.updated, dir.path);
    expect(withoutDir).toMatchObject({ removedBytes: 15, removedFiles: 2, removedDirs: 1 });
    expect(withoutDir.updated).toMatchObject({ size_bytes: 0, file_count: 0, dir_count: 0 });
  });
});
