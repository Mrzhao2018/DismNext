import type { DiskTreeNode } from './api';

export function squarifyRects(
  nodes: DiskTreeNode[],
  x: number,
  y: number,
  w: number,
  h: number
): { node: DiskTreeNode; x: number; y: number; w: number; h: number }[] {
  const valid = nodes.filter((n) => n.size_bytes > 0);
  if (valid.length === 0 || w < 1.5 || h < 1.5) return [];
  const total = valid.reduce((a, b) => a + b.size_bytes, 0);
  if (total <= 0) return [];

  const area = w * h;
  const items = valid.map((node) => ({
    node,
    area: (node.size_bytes / total) * area,
  }));

  const out: { node: DiskTreeNode; x: number; y: number; w: number; h: number }[] = [];
  let cx = x;
  let cy = y;
  let cw = w;
  let ch = h;
  let row: { node: DiskTreeNode; area: number }[] = [];

  const worst = (r: { area: number }[], side: number): number => {
    if (r.length === 0 || side <= 0) return Infinity;
    let sum = 0;
    let maxA = 0;
    let minA = Infinity;
    for (const it of r) {
      sum += it.area;
      if (it.area > maxA) maxA = it.area;
      if (it.area < minA) minA = it.area;
    }
    const s2 = side * side;
    const sum2 = sum * sum;
    return Math.max((s2 * maxA) / sum2, sum2 / (s2 * Math.max(minA, 0.0001)));
  };

  const commit = (r: { node: DiskTreeNode; area: number }[]) => {
    const sum = r.reduce((a, b) => a + b.area, 0);
    if (sum <= 0) return;
    if (cw >= ch) {
      const stripW = sum / ch;
      let oy = cy;
      for (const it of r) {
        const ih = it.area / stripW;
        out.push({ node: it.node, x: cx, y: oy, w: stripW, h: ih });
        oy += ih;
      }
      cx += stripW;
      cw -= stripW;
    } else {
      const stripH = sum / cw;
      let ox = cx;
      for (const it of r) {
        const iw = it.area / stripH;
        out.push({ node: it.node, x: ox, y: cy, w: iw, h: stripH });
        ox += iw;
      }
      cy += stripH;
      ch -= stripH;
    }
  };

  for (const item of items) {
    const side = Math.min(cw, ch);
    if (row.length === 0) {
      row.push(item);
      continue;
    }
    const cand = [...row, item];
    if (worst(cand, side) <= worst(row, side)) {
      row = cand;
    } else {
      commit(row);
      row = [item];
    }
  }
  if (row.length > 0) commit(row);
  return out;
}

export function recyclePathKey(path: string) {
  return path.replace(/\//g, '\\').replace(/\\+$/, '').toLowerCase();
}

export function sameOrBelow(path: string, parent: string) {
  const key = recyclePathKey(path);
  const parentKey = recyclePathKey(parent);
  return key === parentKey || key.startsWith(`${parentKey}\\`);
}

export function removeNodeFromTree(
  node: DiskTreeNode,
  targetPath: string
): { updated: DiskTreeNode; removedBytes: number; removedFiles: number; removedDirs: number } {
  let removedBytes = 0;
  let removedFiles = 0;
  let removedDirs = 0;
  const nextChildren: DiskTreeNode[] = [];

  for (const ch of node.children) {
    if (!ch.is_aggregate && recyclePathKey(ch.path) === recyclePathKey(targetPath)) {
      removedBytes += ch.size_bytes;
      removedFiles += ch.file_count;
      removedDirs += ch.dir_count + (ch.is_dir ? 1 : 0);
      continue;
    }
    if (ch.is_dir && ch.children.length > 0) {
      const sub = removeNodeFromTree(ch, targetPath);
      if (sub.removedBytes > 0 || sub.removedFiles > 0 || sub.removedDirs > 0) {
        removedBytes += sub.removedBytes;
        removedFiles += sub.removedFiles;
        removedDirs += sub.removedDirs;
      }
      nextChildren.push(sub.updated);
    } else {
      nextChildren.push(ch);
    }
  }

  return {
    updated: {
      ...node,
      size_bytes: Math.max(0, node.size_bytes - removedBytes),
      file_count: Math.max(0, node.file_count - removedFiles),
      dir_count: Math.max(0, node.dir_count - removedDirs),
      children: nextChildren,
    },
    removedBytes,
    removedFiles,
    removedDirs,
  };
}
