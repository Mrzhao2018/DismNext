import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  Badge,
  Button,
  Checkbox,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Input,
  MessageBar,
  MessageBarBody,
  Spinner,
  Tab,
  TabList,
  Text,
  makeStyles,
  tokens,
} from '@fluentui/react-components';
import {
  ArrowUp20Regular,
  ChevronDown16Regular,
  ChevronRight16Regular,
  Copy16Regular,
  Delete16Regular,
  Document16Regular,
  Folder16Regular,
  FolderOpen16Regular,
  Search20Regular,
  ZoomIn16Regular,
  ZoomOut16Regular,
} from '@fluentui/react-icons';
import {
  DiskAnalysisReport,
  DiskPreset,
  DiskTreeNode,
  DuplicateGroup,
  DuplicateRecycleResult,
  DuplicateRecycleTarget,
  DuplicateScanReport,
  Lang,
  OrphanDirEntry,
  PathRecyclePreview,
  PathRecycleResult,
  SystemInfo,
  api,
  formatBytes,
  pickText,
} from '../api';
import { useScanJob } from '../useScanJob';
import { ScanStatusBar } from '../ScanStatusBar';

interface Props {
  lang: Lang;
  onSystemUpdated?: (sys: SystemInfo) => void;
}

interface FlatTreeRow {
  node: DiskTreeNode;
  depth: number;
  parentBytes: number;
  hasChildren: boolean;
  isExpanded: boolean;
  isLoading: boolean;
}

interface PaintedLeaf {
  node: DiskTreeNode;
  ancestorPaths: string[];
  x: number;
  y: number;
  w: number;
  h: number;
  color: string;
}

const DIR_FALLBACK_COLORS = [
  '#2E74B5',
  '#388E3C',
  '#C85A32',
  '#7E57C2',
  '#00838F',
  '#AD1457',
  '#D84315',
  '#455A64',
];

const useStyles = makeStyles({
  root: {
    display: 'flex',
    flexDirection: 'column',
    height: '100%',
    gap: '8px',
    overflow: 'hidden',
  },
  toolbar: {
    display: 'flex',
    flexDirection: 'column',
    gap: '6px',
    padding: '8px 12px',
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    flexShrink: 0,
  },
  presetRow: {
    display: 'flex',
    flexWrap: 'wrap',
    alignItems: 'center',
    gap: '6px',
  },
  inputRow: {
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'space-between',
    gap: '10px',
    flexWrap: 'wrap',
  },
  workspaceSplit: {
    flex: 1,
    minHeight: 0,
    display: 'flex',
    flexDirection: 'column',
    gap: '0px',
    overflow: 'hidden',
  },
  topSplit: {
    display: 'grid',
    gridTemplateColumns: '1.45fr 1fr',
    gap: '8px',
    minHeight: '160px',
    overflow: 'hidden',
  },
  paneCard: {
    display: 'flex',
    flexDirection: 'column',
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    overflow: 'hidden',
    minHeight: 0,
  },
  tableHeader: {
    display: 'grid',
    alignItems: 'center',
    padding: '6px 10px',
    fontSize: '12px',
    fontWeight: 600,
    color: tokens.colorNeutralForeground2,
    backgroundColor: tokens.colorNeutralBackground3,
    borderBottom: `1px solid ${tokens.colorNeutralStroke2}`,
    flexShrink: 0,
    userSelect: 'none',
  },
  treeGridCols: {
    gridTemplateColumns: 'minmax(180px, 1fr) 92px 54px 76px 56px',
    gap: '6px',
  },
  extGridCols: {
    gridTemplateColumns: '74px 26px minmax(90px, 1fr) 74px 50px 54px',
    gap: '6px',
  },
  largeFileCols: {
    gridTemplateColumns: 'minmax(180px, 1fr) 68px 78px 72px',
    gap: '6px',
  },
  scrollBody: {
    flex: 1,
    overflowY: 'auto',
    minHeight: 0,
  },
  gridRow: {
    display: 'grid',
    alignItems: 'center',
    padding: '4px 10px',
    fontSize: '12px',
    borderBottom: `1px solid ${tokens.colorNeutralStroke3}`,
    cursor: 'pointer',
    ':hover': {
      backgroundColor: tokens.colorNeutralBackground1Hover,
    },
  },
  gridRowSelected: {
    backgroundColor: tokens.colorNeutralBackground1Selected,
  },
  leftPaneFooter: {
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'space-between',
    padding: '2px 8px',
    borderTop: `1px solid ${tokens.colorNeutralStroke2}`,
    backgroundColor: tokens.colorNeutralBackground3,
    flexShrink: 0,
  },
  resizer: {
    height: '8px',
    cursor: 'row-resize',
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'center',
    flexShrink: 0,
    ':hover': {
      backgroundColor: tokens.colorNeutralBackground3,
    },
  },
  resizerPill: {
    width: '48px',
    height: '3px',
    borderRadius: '2px',
    backgroundColor: tokens.colorNeutralStroke1,
  },
  treemapCard: {
    flex: 1,
    minHeight: '140px',
    display: 'flex',
    flexDirection: 'column',
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    overflow: 'hidden',
  },
  treemapToolbar: {
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'space-between',
    padding: '4px 10px',
    backgroundColor: tokens.colorNeutralBackground3,
    borderBottom: `1px solid ${tokens.colorNeutralStroke2}`,
    flexShrink: 0,
    gap: '8px',
  },
  canvasWrap: {
    position: 'relative',
    flex: 1,
    minHeight: 0,
    backgroundColor: '#111418',
    overflow: 'hidden',
  },
  statusBar: {
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'space-between',
    gap: '10px',
    padding: '6px 12px',
    borderRadius: tokens.borderRadiusMedium,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    flexShrink: 0,
  },
});

function adjustHexColor(hex: string, factor: number): string {
  const clean = hex.replace('#', '');
  if (clean.length !== 6) return hex;
  const r = parseInt(clean.slice(0, 2), 16);
  const g = parseInt(clean.slice(2, 4), 16);
  const b = parseInt(clean.slice(4, 6), 16);
  const clamp = (v: number) => Math.max(0, Math.min(255, Math.round(v)));
  const apply = (c: number) => (factor >= 0 ? clamp(c + (255 - c) * factor) : clamp(c * (1 + factor)));
  const nr = apply(r).toString(16).padStart(2, '0');
  const ng = apply(g).toString(16).padStart(2, '0');
  const nb = apply(b).toString(16).padStart(2, '0');
  return `#${nr}${ng}${nb}`;
}

function squarifyRects(
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

function collectTreemapLeaves(
  node: DiskTreeNode,
  x: number,
  y: number,
  w: number,
  h: number,
  ancestors: string[],
  extColorMap: Map<string, string>,
  fallbackIdx: number,
  out: PaintedLeaf[]
): void {
  if (w < 1.2 || h < 1.2 || node.size_bytes <= 0) return;
  if (!node.is_dir || node.children.length === 0 || (w < 6 && h < 6)) {
    const color =
      node.is_aggregate
        ? '#595959'
        : node.ext
        ? extColorMap.get(node.ext) || '#0078D4'
        : DIR_FALLBACK_COLORS[fallbackIdx % DIR_FALLBACK_COLORS.length];
    out.push({
      node,
      ancestorPaths: ancestors,
      x,
      y,
      w,
      h,
      color,
    });
    return;
  }

  const nextAncestors = [...ancestors, node.path];
  const pad = w > 18 && h > 18 ? 1 : 0;
  const rects = squarifyRects(node.children, x + pad, y + pad, w - pad * 2, h - pad * 2);
  rects.forEach((r, idx) => {
    collectTreemapLeaves(
      r.node,
      r.x,
      r.y,
      r.w,
      r.h,
      nextAncestors,
      extColorMap,
      fallbackIdx + idx + 1,
      out
    );
  });
}

function spliceTreeChildren(
  root: DiskTreeNode,
  targetPath: string,
  children: DiskTreeNode[]
): DiskTreeNode {
  if (root.path === targetPath && root.is_dir) {
    return { ...root, children };
  }
  if (!root.children.length) return root;
  return {
    ...root,
    children: root.children.map((c) => spliceTreeChildren(c, targetPath, children)),
  };
}

function findNodeAndAncestors(
  node: DiskTreeNode,
  targetPath: string,
  trail: string[] = []
): string[] | null {
  if (node.path === targetPath) return trail;
  for (const ch of node.children) {
    const found = findNodeAndAncestors(ch, targetPath, [...trail, node.path]);
    if (found) return found;
  }
  return null;
}

function findNodeByPath(node: DiskTreeNode, targetPath: string): DiskTreeNode | null {
  if (node.path === targetPath) return node;
  for (const ch of node.children) {
    const f = findNodeByPath(ch, targetPath);
    if (f) return f;
  }
  return null;
}

function recyclePathKey(path: string) {
  return path.replace(/\//g, '\\').replace(/\\+$/, '').toLowerCase();
}

function sameOrBelow(path: string, parent: string) {
  const key = recyclePathKey(path);
  const parentKey = recyclePathKey(parent);
  return key === parentKey || key.startsWith(`${parentKey}\\`);
}

function removeNodeFromTree(
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

const skipReasons = {
  keeper_unavailable: { zh: '保留文件已变化或不可用', en: 'The file to keep changed or is unavailable' },
  changed: { zh: '目标自扫描后已变化，请重新扫描。', en: 'The target changed since the scan. Scan again.' },
  missing: { zh: '目标已不在原路径。', en: 'The target is no longer at the original path.' },
  same_file: { zh: '与保留文件或已处理文件指向同一文件', en: 'Refers to the file being kept or already processed' },
  protected: { zh: '目标受保护或路径包含重定向。', en: 'The target is protected or the path contains a redirect.' },
  different_content: { zh: '与保留文件内容不同', en: 'Content differs from the file being kept' },
  unverifiable: { zh: '无法完整核对目标。', en: 'The target could not be completely verified.' },
};

export const DiskAnalyzerView: React.FC<Props> = ({ lang, onSystemUpdated }) => {
  const styles = useStyles();
  const [presets, setPresets] = useState<DiskPreset[]>([]);
  const [targetPath, setTargetPath] = useState('');
  const scan = useScanJob();
  const [report, setReport] = useState<DiskAnalysisReport | null>(null);
  const [measureAllocation, setMeasureAllocation] = useState(false);
  const [allocationStale, setAllocationStale] = useState(false);
  const [expandedPaths, setExpandedPaths] = useState<Set<string>>(new Set());
  const [loadingDirs, setLoadingDirs] = useState<Set<string>>(new Set());
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [zoomStack, setZoomStack] = useState<string[]>([]);
  const [leftTab, setLeftTab] = useState<'tree' | 'files' | 'duplicates' | 'orphans'>('tree');
  const [dupReport, setDupReport] = useState<DuplicateScanReport | null>(null);
  const [orphanDirs, setOrphanDirs] = useState<OrphanDirEntry[] | null>(null);
  const [orphanLoading, setOrphanLoading] = useState(false);
  const [activeExt, setActiveExt] = useState<string | null>(null);
  const [hoveredLeaf, setHoveredLeaf] = useState<PaintedLeaf | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [recycleNotice, setRecycleNotice] = useState<string | null>(null);
  const [recycleResults, setRecycleResults] = useState<(DuplicateRecycleResult | PathRecycleResult)[]>([]);
  const [recycleConfirmTarget, setRecycleConfirmTarget] = useState<{
    paths: string[];
    size_bytes: number;
    label: string;
    review?: PathRecyclePreview;
    reviewError?: string;
    duplicateGroup?: {
      hash: string;
      keeper: DuplicateRecycleTarget;
      candidates: DuplicateRecycleTarget[];
    };
  } | null>(null);
  const [recycling, setRecycling] = useState(false);
  const [topHeightPct, setTopHeightPct] = useState(54);
  const [copiedNotice, setCopiedNotice] = useState(false);

  const splitRef = useRef<HTMLDivElement | null>(null);
  const canvasWrapRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [canvasSize, setCanvasSize] = useState({ w: 800, h: 260 });
  const rowRefs = useRef<Map<string, HTMLDivElement>>(new Map());

  useEffect(() => {
    api
      .getDiskPresets()
      .then((res) => {
        setPresets(res.presets);
        if (res.presets.length > 0) {
          setTargetPath(res.presets[0].path);
        }
      })
      .catch((e) => setErrorMsg(String(e)));
  }, []);

  useEffect(() => {
    const el = canvasWrapRef.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      for (const entry of entries) {
        const { width, height } = entry.contentRect;
        if (width > 20 && height > 20) {
          setCanvasSize({ w: Math.floor(width), h: Math.floor(height) });
        }
      }
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [report]);

  const runAnalyze = async (pathToScan: string) => {
    const trimmed = pathToScan.trim();
    if (!trimmed || scan.active || recycling) return;
    setErrorMsg(null);
    try {
      const res = await scan.run({
        kind: 'disk_analysis',
        path: trimmed,
        max_depth: 4,
        measure_allocation: measureAllocation,
      });
      if (!res) return;
      setActiveExt(null);
      setHoveredLeaf(null);
      setReport(res);
      setAllocationStale(false);
      setTargetPath(res.root_path);
      setExpandedPaths(new Set([res.tree.path]));
      setSelectedPath(res.tree.path);
      setZoomStack([res.tree.path]);
      setDupReport(null);
    } catch (e) {
      setErrorMsg(String(e));
    }
  };

  const runDupScan = async (rootPath: string) => {
    if (scan.active || recycling) return;
    setErrorMsg(null);
    try {
      const res = await scan.run({ kind: 'duplicate_files', path: rootPath, min_size_bytes: 512 * 1024 });
      if (res) setDupReport(res);
    } catch (e) {
      setErrorMsg(String(e));
    }
  };

  const runOrphanScan = async () => {
    setOrphanLoading(true);
    setErrorMsg(null);
    try {
      const res = await api.getOrphanDirs();
      setOrphanDirs(res.orphans);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setOrphanLoading(false);
    }
  };

  const handleSelectLeftTab = (next: 'tree' | 'files' | 'duplicates' | 'orphans') => {
    setLeftTab(next);
    if (next === 'duplicates' && report && (!dupReport || dupReport.root_path !== report.root_path) && !scan.active) {
      void runDupScan(report.root_path);
    }
    if (next === 'orphans' && orphanDirs === null && !orphanLoading) {
      void runOrphanScan();
    }
  };

  const toggleExpand = async (node: DiskTreeNode) => {
    if (!node.is_dir || node.is_aggregate) return;
    const p = node.path;
    if (expandedPaths.has(p)) {
      setExpandedPaths((prev) => {
        const next = new Set(prev);
        next.delete(p);
        return next;
      });
      return;
    }

    if (node.children.length === 0 && node.file_count + node.dir_count > 0) {
      if (scan.active || recycling) return;
      setErrorMsg(null);
      setLoadingDirs((prev) => new Set(prev).add(p));
      try {
        const sub = await scan.run({ kind: 'disk_analysis', path: p, max_depth: 2 });
        if (!sub) return;
        setReport((prev) =>
          prev ? { ...prev, tree: spliceTreeChildren(prev.tree, p, sub.tree.children) } : prev
        );
      } catch (e) {
        setErrorMsg(String(e));
        return;
      } finally {
        setLoadingDirs((prev) => {
          const next = new Set(prev);
          next.delete(p);
          return next;
        });
      }
    }

    setExpandedPaths((prev) => new Set(prev).add(p));
  };

  const flatRows = useMemo(() => {
    if (!report) return [];
    const rows: FlatTreeRow[] = [];
    const walk = (node: DiskTreeNode, depth: number, parentBytes: number) => {
      const hasChildren = node.is_dir && (node.children.length > 0 || node.file_count + node.dir_count > 0);
      const isExpanded = expandedPaths.has(node.path);
      rows.push({
        node,
        depth,
        parentBytes,
        hasChildren,
        isExpanded,
        isLoading: loadingDirs.has(node.path),
      });
      if (node.is_dir && isExpanded) {
        for (const ch of node.children) {
          walk(ch, depth + 1, node.size_bytes);
        }
      }
    };
    walk(report.tree, 0, report.total_bytes);
    return rows;
  }, [report, expandedPaths, loadingDirs]);

  const extColorMap = useMemo(() => {
    const map = new Map<string, string>();
    report?.extensions.forEach((e) => map.set(e.ext, e.color));
    return map;
  }, [report]);

  const extDescMap = useMemo(() => {
    const map = new Map<string, string>();
    report?.extensions.forEach((e) => map.set(e.ext, pickText(e.description, lang)));
    return map;
  }, [report, lang]);

  const zoomRootNode = useMemo(() => {
    if (!report) return null;
    const zPath = zoomStack[zoomStack.length - 1] || report.tree.path;
    return findNodeByPath(report.tree, zPath) || report.tree;
  }, [report, zoomStack]);

  const paintedLeaves = useMemo(() => {
    if (!zoomRootNode) return [];
    const leaves: PaintedLeaf[] = [];
    collectTreemapLeaves(
      zoomRootNode,
      0,
      0,
      canvasSize.w,
      canvasSize.h,
      [],
      extColorMap,
      0,
      leaves
    );
    return leaves;
  }, [zoomRootNode, canvasSize, extColorMap]);

  // Draw 3D Cushion Treemap onto HiDPI canvas
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.max(1, Math.floor(canvasSize.w * dpr));
    canvas.height = Math.max(1, Math.floor(canvasSize.h * dpr));
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = '#14171c';
    ctx.fillRect(0, 0, canvasSize.w, canvasSize.h);

    for (const leaf of paintedLeaves) {
      if (leaf.w < 1 || leaf.h < 1) continue;
      const isExtDimmed =
        activeExt !== null && (leaf.node.is_dir ? true : leaf.node.ext !== activeExt);
      const isSelected =
        selectedPath !== null &&
        (leaf.node.path === selectedPath || leaf.ancestorPaths.includes(selectedPath));

      ctx.save();
      if (isExtDimmed) {
        ctx.globalAlpha = 0.18;
      }

      if (leaf.w >= 4 && leaf.h >= 4) {
        const grad = ctx.createLinearGradient(leaf.x, leaf.y, leaf.x + leaf.w, leaf.y + leaf.h);
        grad.addColorStop(0, adjustHexColor(leaf.color, 0.36));
        grad.addColorStop(0.32, leaf.color);
        grad.addColorStop(1, adjustHexColor(leaf.color, -0.45));
        ctx.fillStyle = grad;
        ctx.fillRect(leaf.x, leaf.y, leaf.w, leaf.h);

        // Specular top-left bevel & dark bottom-right seam for crisp 3D cushion effect
        ctx.strokeStyle = 'rgba(255,255,255,0.28)';
        ctx.lineWidth = 1;
        ctx.beginPath();
        ctx.moveTo(leaf.x + 0.5, leaf.y + leaf.h - 0.5);
        ctx.lineTo(leaf.x + 0.5, leaf.y + 0.5);
        ctx.lineTo(leaf.x + leaf.w - 0.5, leaf.y + 0.5);
        ctx.stroke();

        ctx.strokeStyle = 'rgba(0,0,0,0.45)';
        ctx.beginPath();
        ctx.moveTo(leaf.x + leaf.w - 0.5, leaf.y + 0.5);
        ctx.lineTo(leaf.x + leaf.w - 0.5, leaf.y + leaf.h - 0.5);
        ctx.lineTo(leaf.x + 0.5, leaf.y + leaf.h - 0.5);
        ctx.stroke();
      } else {
        ctx.fillStyle = leaf.color;
        ctx.fillRect(leaf.x, leaf.y, leaf.w, leaf.h);
      }

      if (isSelected) {
        ctx.globalAlpha = 1;
        ctx.strokeStyle = '#FFFFFF';
        ctx.lineWidth = 1.6;
        ctx.strokeRect(leaf.x + 1, leaf.y + 1, Math.max(1, leaf.w - 2), Math.max(1, leaf.h - 2));
      }

      if (leaf.w >= 58 && leaf.h >= 20 && !isExtDimmed) {
        ctx.fillStyle = 'rgba(255,255,255,0.92)';
        ctx.font = '11px "Segoe UI", "Microsoft YaHei UI", sans-serif';
        ctx.shadowColor = 'rgba(0,0,0,0.75)';
        ctx.shadowBlur = 2;
        const label = leaf.node.is_aggregate
          ? `+${leaf.node.aggregate_count}`
          : leaf.node.name;
        ctx.save();
        ctx.beginPath();
        ctx.rect(leaf.x + 3, leaf.y + 2, leaf.w - 6, leaf.h - 4);
        ctx.clip();
        ctx.fillText(label, leaf.x + 4, leaf.y + 13);
        ctx.restore();
      }

      ctx.restore();
    }

    if (hoveredLeaf) {
      ctx.save();
      ctx.strokeStyle = '#60CDFF';
      ctx.lineWidth = 2;
      ctx.strokeRect(
        hoveredLeaf.x + 1,
        hoveredLeaf.y + 1,
        Math.max(1, hoveredLeaf.w - 2),
        Math.max(1, hoveredLeaf.h - 2)
      );
      ctx.restore();
    }
  }, [paintedLeaves, canvasSize, activeExt, selectedPath, hoveredLeaf]);

  const hitTest = useCallback(
    (clientX: number, clientY: number): PaintedLeaf | null => {
      const canvas = canvasRef.current;
      if (!canvas) return null;
      const rect = canvas.getBoundingClientRect();
      const x = clientX - rect.left;
      const y = clientY - rect.top;
      for (let i = paintedLeaves.length - 1; i >= 0; i--) {
        const lf = paintedLeaves[i];
        if (x >= lf.x && x <= lf.x + lf.w && y >= lf.y && y <= lf.y + lf.h) {
          return lf;
        }
      }
      return null;
    },
    [paintedLeaves]
  );

  const selectAndRevealInTree = (path: string) => {
    setSelectedPath(path);
    if (!report) return;
    const ancestors = findNodeAndAncestors(report.tree, path);
    if (ancestors) {
      setExpandedPaths((prev) => {
        const next = new Set(prev);
        ancestors.forEach((p) => next.add(p));
        return next;
      });
      setTimeout(() => {
        rowRefs.current.get(path)?.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
      }, 40);
    }
  };

  const handleCanvasClick = (e: React.MouseEvent<HTMLCanvasElement>) => {
    const leaf = hitTest(e.clientX, e.clientY);
    if (!leaf) return;
    selectAndRevealInTree(leaf.node.path);
    if (!leaf.node.is_dir && leaf.node.ext) {
      setActiveExt((prev) => (prev === leaf.node.ext ? prev : null));
    }
  };

  const handleCanvasDoubleClick = (e: React.MouseEvent<HTMLCanvasElement>) => {
    const leaf = hitTest(e.clientX, e.clientY);
    if (!leaf || !report) return;
    const targetDir = leaf.node.is_dir
      ? leaf.node.path
      : leaf.ancestorPaths[leaf.ancestorPaths.length - 1];
    if (targetDir && targetDir !== zoomStack[zoomStack.length - 1]) {
      const targetNode = findNodeByPath(report.tree, targetDir);
      if (targetNode && targetNode.children.length > 0) {
        setZoomStack((prev) => [...prev, targetDir]);
      } else {
        runAnalyze(targetDir);
      }
    }
  };

  const startResize = (e: React.MouseEvent) => {
    e.preventDefault();
    const container = splitRef.current;
    if (!container) return;
    const onMove = (ev: MouseEvent) => {
      const box = container.getBoundingClientRect();
      const pct = ((ev.clientY - box.top) / box.height) * 100;
      setTopHeightPct(Math.max(25, Math.min(78, pct)));
    };
    const onUp = () => {
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
  };

  const selectedNode = useMemo(() => {
    if (!report || !selectedPath) return report?.tree || null;
    return findNodeByPath(report.tree, selectedPath) || report.tree;
  }, [report, selectedPath]);

  const activeDisplayNode = hoveredLeaf?.node || selectedNode;

  const prepareRecycle = async (path: string, isDir: boolean, scanStartedAt: string) => {
    if (scan.active || recycling) return;
    const request = { paths: [path], label: path, size_bytes: 0 };
    setRecycleConfirmTarget(request);
    try {
      const review = await api.reviewRecyclePath(path, isDir, scanStartedAt);
      setRecycleConfirmTarget((current) => current === request
        ? { ...request, review, size_bytes: review.status === 'ready' ? review.size_bytes : 0 }
        : current);
    } catch (error) {
      setRecycleConfirmTarget((current) => current === request
        ? { ...request, reviewError: String(error) }
        : current);
    }
  };

  const confirmRecycle = async () => {
    if (!recycleConfirmTarget || !report || scan.active || recycling) return;
    const { paths, duplicateGroup, review } = recycleConfirmTarget;
    if (!duplicateGroup && review?.status !== 'ready') return;
    setRecycling(true);
    setErrorMsg(null);
    setRecycleNotice(null);
    setRecycleResults([]);
    try {
      let items: (DuplicateRecycleResult | PathRecycleResult)[];
      if (duplicateGroup) {
        const response = await api.recycleDuplicates(duplicateGroup.keeper, duplicateGroup.candidates);
        items = response.items;
        onSystemUpdated?.(response.system);
      } else if (review?.status === 'ready') {
        const response = await api.recyclePath(paths[0], review.stamp);
        items = [response.item];
        onSystemUpdated?.(response.system);
      } else {
        return;
      }
      const recycled = items.filter((item) => item.status === 'recycled');
      const targets = recycled.map((item) => item.path);
      const rootRemoved = targets.some((target) => sameOrBelow(report.root_path, target));
      const withinReport = recycled.filter(({ path }) => sameOrBelow(path, report.root_path));
      let movedSize = 0;
      let nextTree = report.tree;
      let nextTotalBytes = report.total_bytes;
      let nextTotalFiles = report.total_files;
      let nextTotalDirs = report.total_dirs;
      let nextLargest = report.largest_files;

      for (const item of recycled) {
        movedSize += item.size_bytes;
      }
      for (const item of withinReport) {
        const { path: target, size_bytes } = item;
        const { updated, removedBytes, removedFiles, removedDirs } = removeNodeFromTree(
          nextTree,
          target
        );
        nextTree = updated;
        const deltaBytes = removedBytes > 0 ? removedBytes : size_bytes;
        nextTotalBytes = Math.max(0, nextTotalBytes - deltaBytes);
        const fileCount = 'file_count' in item ? item.file_count : 1;
        const dirCount = 'dir_count' in item ? item.dir_count : 0;
        nextTotalFiles = Math.max(0, nextTotalFiles - (removedFiles || fileCount));
        nextTotalDirs = Math.max(0, nextTotalDirs - (removedDirs || dirCount));
        nextLargest = nextLargest.filter((file) => !sameOrBelow(file.path, target));
      }

      if (rootRemoved || withinReport.length > 0) {
        setReport(rootRemoved ? null : {
          ...report,
          allocation: undefined,
          total_bytes: nextTotalBytes,
          total_files: nextTotalFiles,
          total_dirs: nextTotalDirs,
          tree: { ...nextTree, file_count: nextTotalFiles, dir_count: nextTotalDirs, size_bytes: nextTotalBytes },
          largest_files: nextLargest,
        });
        if (report.allocation?.status === 'measured') setAllocationStale(true);
      }

      const wasRecycled = (path: string) => targets.some((target) => sameOrBelow(path, target));
      const staleDuplicates = new Set<string>();
      let keeperUnavailable = false;
      for (const item of items) {
        if (!duplicateGroup || item.status !== 'skipped') continue;
        if (item.reason === 'keeper_unavailable') keeperUnavailable = true;
        if (item.reason !== 'protected' && item.reason !== 'keeper_unavailable' && item.reason !== 'unverifiable') {
          staleDuplicates.add(recyclePathKey(item.path));
        }
      }
      setDupReport((prev) => {
        if (!prev) return prev;
        const nextGroups = prev.groups
          .filter((group) => !keeperUnavailable || group.hash_hex !== duplicateGroup?.hash)
          .map((g) => {
            const keptFiles = g.files.filter((file) => !staleDuplicates.has(recyclePathKey(file.path)) && !wasRecycled(file.path));
            return {
              ...g,
              files: keptFiles,
              wasted_bytes:
                keptFiles.length > 1 ? g.size_bytes * (keptFiles.length - 1) : 0,
            };
          })
          .filter((g) => g.files.length >= 2);
        return {
          ...prev,
          groups: nextGroups,
          total_groups: Math.max(0, prev.total_groups - prev.groups.length + nextGroups.length),
          total_wasted_bytes: Math.max(0, prev.total_wasted_bytes
            - prev.groups.reduce((total, group) => total + group.wasted_bytes, 0)
            + nextGroups.reduce((total, group) => total + group.wasted_bytes, 0)),
        };
      });
      setOrphanDirs((prev) =>
        prev ? prev.filter((entry) => !wasRecycled(entry.path)) : prev
      );

      if (rootRemoved || withinReport.length > 0) setSelectedPath(rootRemoved ? null : nextTree.path);
      setZoomStack((prev) => prev.filter((path) => !wasRecycled(path)));
      const skipped = items.filter((item) => item.status === 'skipped').length;
      const failed = items.length - targets.length - skipped;
      setRecycleNotice(lang === 'zh'
        ? `已移入回收站 ${targets.length} 项（逻辑大小 ${formatBytes(movedSize)}），跳过 ${skipped} 项，失败 ${failed} 项。`
        : `Moved ${targets.length} to the Recycle Bin (${formatBytes(movedSize)} logical size), skipped ${skipped}, failed ${failed}.`);
      setRecycleResults(items);
      setRecycleConfirmTarget(null);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setRecycling(false);
    }
  };

  const formatNodeTitle = (node: DiskTreeNode) =>
    node.is_aggregate
      ? lang === 'zh'
        ? `[其余 ${node.aggregate_count} 个较小项]`
        : `[+${node.aggregate_count} smaller items]`
      : node.name;
  const logicalSizeLabel = lang === 'zh' ? '逻辑大小' : 'Logical size';
  const allocation = report?.allocation;

  return (
    <div className={styles.root}>
      {errorMsg && (
        <MessageBar intent="error">
          <MessageBarBody>{errorMsg}</MessageBarBody>
        </MessageBar>
      )}
      {recycleNotice && (
        <MessageBar intent={recycleResults.some((item) => item.status !== 'recycled') ? 'warning' : 'success'}>
          <MessageBarBody>
            {recycleNotice}
            <details open={recycleResults.some((item) => item.status !== 'recycled')}>
              <summary>{lang === 'zh' ? '查看每项结果' : 'Show item results'}</summary>
              <ul style={{ maxHeight: 128, overflowY: 'auto', overflowWrap: 'anywhere' }}>
                {recycleResults.map((item, index) => (
                  <li key={`${item.path}-${index}`}>
                    {item.path} · {item.status === 'skipped' ? `${lang === 'zh' ? '已跳过' : 'Skipped'} · ${skipReasons[item.reason][lang]}`
                      : item.status === 'failed' ? `${lang === 'zh' ? '失败' : 'Failed'} · ${item.error}`
                      : lang === 'zh' ? '已移入回收站' : 'Moved to the Recycle Bin'}
                  </li>
                ))}
              </ul>
            </details>
          </MessageBarBody>
        </MessageBar>
      )}

      <div className={styles.toolbar}>
        <div className={styles.presetRow}>
          <Text size={200} weight="semibold">
            {lang === 'zh' ? '扫描位置：' : 'Target:'}
          </Text>
          {presets.map((p) => (
            <Button
              key={p.path}
              size="small"
              appearance={targetPath === p.path ? 'primary' : 'outline'}
              disabled={Boolean(scan.active) || recycling}
              onClick={() => runAnalyze(p.path)}
            >
              {pickText(p.label, lang)}
            </Button>
          ))}
        </div>

        <div className={styles.inputRow}>
          <div style={{ display: 'flex', gap: 8, flex: 1, minWidth: 280 }}>
            <Input
              size="small"
              style={{ flex: 1 }}
              contentBefore={<Search20Regular />}
              placeholder={
                lang === 'zh'
                  ? '输入任意目录或盘符（例如 C:\\ 或 C:\\Users）'
                  : 'Enter any drive or folder path (e.g. C:\\)'
              }
              value={targetPath}
              onChange={(_, d) => setTargetPath(d.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') runAnalyze(targetPath);
              }}
            />
            <Button
              size="small"
              appearance="primary"
              disabled={Boolean(scan.active) || recycling || !targetPath.trim()}
              onClick={() => runAnalyze(targetPath)}
            >
              {lang === 'zh' ? '开始分析' : 'Analyze'}
            </Button>
          </div>

          <Checkbox
            checked={measureAllocation}
            disabled={Boolean(scan.active) || recycling}
            onChange={(_, data) => setMeasureAllocation(data.checked === true)}
            label={lang === 'zh' ? '统计 NTFS 磁盘占用' : 'Measure NTFS allocation'}
            title={lang === 'zh' ? '额外读取每个文件的分配信息，扫描可能更慢。' : 'Reads allocation information for each file and may take longer.'}
          />

          {report && (
            <div style={{ display: 'flex', gap: 6, alignItems: 'center', flexWrap: 'wrap' }}>
              <Badge size="small" appearance="tint" color="brand">
                {logicalSizeLabel} · {formatBytes(report.total_bytes)}
              </Badge>
              {allocation?.status === 'measured' && (
                <Badge size="small" appearance="tint" color="informative">
                  {lang === 'zh' ? '已测得占用' : 'Measured allocation'} · {formatBytes(allocation.allocated_bytes)}
                </Badge>
              )}
              <Badge size="small" appearance="outline">
                {report.total_files.toLocaleString()} {lang === 'zh' ? '文件' : 'files'}
              </Badge>
              <Badge size="small" appearance="outline">
                {report.total_dirs.toLocaleString()} {lang === 'zh' ? '目录' : 'dirs'}
              </Badge>
              <Badge size="small" appearance="tint" color="success">
                {report.elapsed_ms} ms
              </Badge>
            </div>
          )}
        </div>
        {allocation?.status === 'measured' && (
          <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
            {lang === 'zh'
              ? `统计普通文件数据的分配量 · ${allocation.hard_link_aliases} 个重复硬链接已去重`
              : `Regular file data allocation · ${allocation.hard_link_aliases} hard-link aliases excluded`}
            {(allocation.unmeasured_files > 0 || allocation.unreadable_entries > 0) && (
              lang === 'zh'
                ? ` · ${allocation.unmeasured_files} 个文件未测得占用，${allocation.unreadable_entries} 项无法读取`
                : ` · ${allocation.unmeasured_files} unmeasured files, ${allocation.unreadable_entries} unreadable entries`
            )}
          </Text>
        )}
        {allocation?.status === 'unsupported_file_system' && (
          <Text size={200}>{lang === 'zh' ? '磁盘占用测量支持 NTFS 卷。' : 'Allocation measurement is supported on NTFS volumes.'}</Text>
        )}
        {allocation?.status === 'unavailable' && (
          <Text size={200}>{lang === 'zh' ? '无法读取目标卷的分配信息。' : 'Could not read target volume information for allocation measurement.'}</Text>
        )}
        {allocationStale && (
          <Text size={200}>{lang === 'zh' ? '文件已变更。重新分析可更新磁盘占用。' : 'Files changed. Analyze again to update disk allocation.'}</Text>
        )}
      </div>

      <ScanStatusBar active={scan.active} cancelled={scan.cancelled} cancel={scan.cancel} lang={lang} />

      {!report ? (
        <div
          className={styles.paneCard}
          style={{ flex: 1, alignItems: 'center', justifyContent: 'center', gap: 8, padding: 24 }}
        >
          <Text size={400} weight="semibold">
            {lang === 'zh' ? '高速磁盘空间透视分析' : 'High-Speed Disk Space Visualizer'}
          </Text>
          <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
            {lang === 'zh'
              ? '点击上方快捷目录或盘符，通过可展开目录树、文件类型分布与 3D 曲面树图三栏联动快速定位大文件与冗余缓存。'
              : 'Select a target folder or drive above to inspect directories, file types, and 3D cushion treemap.'}
          </Text>
        </div>
      ) : (
        <>
          <div ref={splitRef} className={styles.workspaceSplit}>
            {/* Top Split: Left TreeGrid / Largest Files + Right Extension Breakdown */}
            <div className={styles.topSplit} style={{ height: `${topHeightPct}%` }}>
              {/* Left Pane */}
              <div className={styles.paneCard}>
                {leftTab === 'tree' ? (
                  <>
                    <div className={`${styles.tableHeader} ${styles.treeGridCols}`}>
                      <span>{lang === 'zh' ? '名称' : 'Name'}</span>
                      <span>{lang === 'zh' ? '占比条' : 'Subtree %'}</span>
                      <span style={{ textAlign: 'right' }}>{lang === 'zh' ? '比例' : '%'}</span>
                      <span style={{ textAlign: 'right' }}>{logicalSizeLabel}</span>
                      <span style={{ textAlign: 'right' }}>{lang === 'zh' ? '文件' : 'Files'}</span>
                    </div>
                    <div className={styles.scrollBody}>
                      {flatRows.map((row) => {
                        const pct =
                          row.parentBytes > 0
                            ? (row.node.size_bytes / row.parentBytes) * 100
                            : 100;
                        const isSel = selectedPath === row.node.path;
                        return (
                          <div
                            key={`${row.node.path}-${row.depth}-${row.node.name}`}
                            ref={(el) => {
                              if (el) rowRefs.current.set(row.node.path, el);
                            }}
                            className={`${styles.gridRow} ${styles.treeGridCols} ${
                              isSel ? styles.gridRowSelected : ''
                            }`}
                            onClick={() => setSelectedPath(row.node.path)}
                            onDoubleClick={() => {
                              if (row.node.is_dir && !row.node.is_aggregate) {
                                toggleExpand(row.node);
                              }
                            }}
                          >
                            <div
                              style={{
                                display: 'flex',
                                alignItems: 'center',
                                gap: 4,
                                paddingLeft: row.depth * 14,
                                minWidth: 0,
                              }}
                            >
                              {row.hasChildren ? (
                                <span
                                  onClick={(e) => {
                                    e.stopPropagation();
                                    toggleExpand(row.node);
                                  }}
                                  style={{
                                    display: 'inline-flex',
                                    alignItems: 'center',
                                    width: 16,
                                    flexShrink: 0,
                                  }}
                                >
                                  {row.isLoading ? (
                                    <Spinner size="extra-tiny" />
                                  ) : row.isExpanded ? (
                                    <ChevronDown16Regular />
                                  ) : (
                                    <ChevronRight16Regular />
                                  )}
                                </span>
                              ) : (
                                <span style={{ width: 16, flexShrink: 0 }} />
                              )}
                              {row.node.is_dir ? (
                                <Folder16Regular
                                  style={{
                                    color: tokens.colorPaletteDarkOrangeForeground1,
                                    flexShrink: 0,
                                  }}
                                />
                              ) : (
                                <Document16Regular
                                  style={{ color: tokens.colorBrandForeground1, flexShrink: 0 }}
                                />
                              )}
                              <span
                                style={{
                                  overflow: 'hidden',
                                  textOverflow: 'ellipsis',
                                  whiteSpace: 'nowrap',
                                }}
                                title={row.node.path}
                              >
                                {formatNodeTitle(row.node)}
                              </span>
                              {row.node.is_cache_hint && (
                                <Badge size="extra-small" appearance="tint" color="warning">
                                  {lang === 'zh' ? '缓存' : 'Cache'}
                                </Badge>
                              )}
                            </div>

                            <div
                              style={{
                                height: 7,
                                borderRadius: 3,
                                backgroundColor: tokens.colorNeutralBackground4,
                                overflow: 'hidden',
                              }}
                            >
                              <div
                                style={{
                                  width: `${Math.max(1.5, pct)}%`,
                                  height: '100%',
                                  backgroundColor: row.node.is_cache_hint
                                    ? tokens.colorPaletteDarkOrangeBackground3
                                    : tokens.colorBrandBackground,
                                }}
                              />
                            </div>
                            <span style={{ textAlign: 'right', fontVariantNumeric: 'tabular-nums' }}>
                              {pct.toFixed(1)}%
                            </span>
                            <span
                              style={{
                                textAlign: 'right',
                                fontWeight: 600,
                                fontVariantNumeric: 'tabular-nums',
                              }}
                            >
                              {formatBytes(row.node.size_bytes)}
                            </span>
                            <span
                              style={{
                                textAlign: 'right',
                                color: tokens.colorNeutralForeground3,
                                fontVariantNumeric: 'tabular-nums',
                              }}
                            >
                              {row.node.file_count.toLocaleString()}
                            </span>
                          </div>
                        );
                      })}
                    </div>
                  </>
                ) : leftTab === 'files' ? (
                  <>
                    <div className={`${styles.tableHeader} ${styles.largeFileCols}`}>
                      <span>{lang === 'zh' ? '文件路径' : 'File Path'}</span>
                      <span>{lang === 'zh' ? '类型' : 'Ext'}</span>
                      <span style={{ textAlign: 'right' }}>{logicalSizeLabel}</span>
                      <span style={{ textAlign: 'center' }}>{lang === 'zh' ? '定位' : 'Open'}</span>
                    </div>
                    <div className={styles.scrollBody}>
                      {report.largest_files.map((lf, i) => (
                        <div
                          key={`${lf.path}-${i}`}
                          className={`${styles.gridRow} ${styles.largeFileCols} ${
                            selectedPath === lf.path ? styles.gridRowSelected : ''
                          }`}
                          onClick={() => selectAndRevealInTree(lf.path)}
                        >
                          <span
                            title={lf.path}
                            style={{
                              overflow: 'hidden',
                              textOverflow: 'ellipsis',
                              whiteSpace: 'nowrap',
                              fontFamily: 'Consolas, monospace',
                            }}
                          >
                            {lf.path}
                          </span>
                          <Badge size="small" appearance="outline">
                            {lf.ext}
                          </Badge>
                          <span style={{ textAlign: 'right', fontWeight: 600 }}>
                            {formatBytes(lf.size_bytes)}
                            {allocation?.status === 'measured' && (
                              <Text block size={100} style={{ color: tokens.colorNeutralForeground3 }}>
                                {lang === 'zh' ? '占用' : 'Allocated'}{' '}
                                {lf.allocated_bytes === undefined ? '—' : formatBytes(lf.allocated_bytes)}
                              </Text>
                            )}
                          </span>
                          <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 2 }}>
                            <Button
                              size="small"
                              appearance="subtle"
                              icon={<FolderOpen16Regular />}
                              title={lang === 'zh' ? '在资源管理器中定位' : 'Reveal'}
                              onClick={(e) => {
                                e.stopPropagation();
                                api.revealInExplorer(lf.path);
                              }}
                            />
                            <Button
                              size="small"
                              appearance="subtle"
                              icon={<Delete16Regular />}
                              title={lang === 'zh' ? '移入回收站' : 'Move to Recycle Bin'}
                              onClick={(e) => {
                                e.stopPropagation();
                                void prepareRecycle(lf.path, false, report.scan_started_at);
                              }}
                            />
                          </div>
                        </div>
                      ))}
                    </div>
                  </>
                ) : leftTab === 'duplicates' ? (
                  <div className={styles.scrollBody} style={{ padding: 10 }}>
                    <div
                      style={{
                        display: 'flex',
                        justifyContent: 'space-between',
                        alignItems: 'center',
                        marginBottom: 8,
                        gap: 8,
                      }}
                    >
                      <Text size={200} weight="semibold">
                        {dupReport
                          ? lang === 'zh'
                            ? `扫描耗时 ${dupReport.elapsed_ms}ms · ${dupReport.total_groups} 组重复文件 · 候选副本逻辑大小 ${formatBytes(dupReport.total_wasted_bytes)}`
                            : `${dupReport.total_groups} duplicate groups · ${formatBytes(dupReport.total_wasted_bytes)} candidate logical size (${dupReport.elapsed_ms}ms)`
                          : lang === 'zh'
                          ? '重复文件扫描'
                          : 'Duplicate file scan'}
                      </Text>
                      <Button
                        size="small"
                        appearance="outline"
                        disabled={Boolean(scan.active) || recycling}
                        onClick={() => void runDupScan(report.root_path)}
                      >
                        {dupReport ? (lang === 'zh' ? '重新查重' : 'Rescan') : (lang === 'zh' ? '开始查重' : 'Find duplicates')}
                      </Button>
                    </div>
                    {!dupReport ? null : dupReport.groups.length === 0 ? (
                      <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                        {lang === 'zh' ? '该目录下未发现大于 512 KB 的重复文件。' : 'No duplicate files (>512 KB) found.'}
                      </Text>
                    ) : (
                      <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
                        {dupReport.groups.map((g: DuplicateGroup) => (
                          <div
                            key={g.hash_hex}
                            style={{
                              padding: 8,
                              borderRadius: tokens.borderRadiusMedium,
                              backgroundColor: tokens.colorNeutralBackground1,
                              border: `1px solid ${tokens.colorNeutralStroke2}`,
                              display: 'flex',
                              flexDirection: 'column',
                              gap: 4,
                            }}
                          >
                            <div
                              style={{
                                display: 'flex',
                                justifyContent: 'space-between',
                                alignItems: 'center',
                                gap: 8,
                              }}
                            >
                              <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                                <Badge size="small" appearance="tint" color="warning">
                                  {g.files.length}x
                                </Badge>
                                <Text size={200} weight="semibold">
                                  {g.files[0]?.name} · {formatBytes(g.size_bytes)} (
                                  {lang === 'zh' ? '候选副本' : 'candidate copies'} {formatBytes(g.wasted_bytes)})
                                </Text>
                              </div>
                              <Button
                                size="small"
                                appearance="primary"
                                icon={<Delete16Regular />}
                                disabled={Boolean(scan.active) || recycling || g.files.some((file) => !file.stamp)}
                                onClick={() => {
                                  const [kept, ...copies] = g.files;
                                  if (!kept?.stamp) return;
                                  const candidates = copies.flatMap((file) => file.stamp ? [{ path: file.path, stamp: file.stamp }] : []);
                                  const toRecycle = candidates.map((file) => file.path);
                                  if (toRecycle.length === 0) return;
                                  setRecycleConfirmTarget({
                                    paths: toRecycle,
                                    size_bytes: g.wasted_bytes,
                                    duplicateGroup: { hash: g.hash_hex, keeper: { path: kept.path, stamp: kept.stamp }, candidates },
                                    label:
                                      lang === 'zh'
                                        ? `保留最新 (${g.files[0].path})，其余 ${toRecycle.length} 个副本移入回收站`
                                        : `Keep newest (${g.files[0].path}), recycle ${toRecycle.length} older copies`,
                                  });
                                }}
                              >
                                {lang === 'zh' ? '保留最新，清理副本' : 'Keep Newest'}
                              </Button>
                            </div>
                            {g.files.map((f, idx) => (
                              <div
                                key={f.path}
                                style={{
                                  display: 'flex',
                                  justifyContent: 'space-between',
                                  alignItems: 'center',
                                  gap: 8,
                                  fontSize: 11,
                                  color: tokens.colorNeutralForeground2,
                                }}
                              >
                                <span
                                  style={{
                                    overflow: 'hidden',
                                    textOverflow: 'ellipsis',
                                    whiteSpace: 'nowrap',
                                    fontFamily: 'Consolas, monospace',
                                  }}
                                  title={f.path}
                                >
                                  {idx === 0 ? '★ ' : '• '}
                                  {f.path}
                                </span>
                                <span style={{ flexShrink: 0 }}>{f.modified_date}</span>
                              </div>
                            ))}
                          </div>
                        ))}
                      </div>
                    )}
                  </div>
                ) : (
                  <div className={styles.scrollBody} style={{ padding: 10 }}>
                    <div
                      style={{
                        display: 'flex',
                        justifyContent: 'space-between',
                        alignItems: 'center',
                        marginBottom: 8,
                        gap: 8,
                      }}
                    >
                      <Text size={200} weight="semibold">
                        {orphanDirs
                          ? lang === 'zh'
                            ? `识别到 ${orphanDirs.length} 个未匹配本机已安装程序的 AppData / ProgramData 较大目录（删除前请核实是否为绿色免安装软件）`
                            : `${orphanDirs.length} unmatched AppData / ProgramData directories (>1 MB)`
                          : lang === 'zh'
                          ? '正在比对本机软件清单...'
                          : 'Matching installed apps...'}
                      </Text>
                      <Button
                        size="small"
                        appearance="outline"
                        disabled={orphanLoading}
                        onClick={() => void runOrphanScan()}
                      >
                        {orphanLoading ? <Spinner size="tiny" /> : lang === 'zh' ? '重新检测' : 'Rescan'}
                      </Button>
                    </div>
                    {orphanLoading ? (
                      <div style={{ display: 'flex', justifyContent: 'center', padding: 30 }}>
                        <Spinner label={lang === 'zh' ? '正在交叉比对 Uninstall 与 AppX 清单...' : 'Cross-checking installed software...'} />
                      </div>
                    ) : !orphanDirs || orphanDirs.length === 0 ? (
                      <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                        {lang === 'zh' ? '未发现明显的疑似卸载残留大目录。' : 'No suspected orphaned directories found.'}
                      </Text>
                    ) : (
                      <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
                        {orphanDirs.map((od) => (
                          <div
                            key={od.path}
                            style={{
                              display: 'flex',
                              justifyContent: 'space-between',
                              alignItems: 'center',
                              gap: 10,
                              padding: 8,
                              borderRadius: tokens.borderRadiusMedium,
                              backgroundColor: tokens.colorNeutralBackground1,
                              border: `1px solid ${tokens.colorNeutralStroke2}`,
                            }}
                          >
                            <div style={{ minWidth: 0, flex: 1 }}>
                              <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                                <Text size={200} weight="semibold">
                                  {od.name}
                                </Text>
                                <Badge size="small" appearance="outline">
                                  {od.scope_label}
                                </Badge>
                                <Badge size="small" appearance="tint" color="warning">
                                  {formatBytes(od.size_bytes)}
                                </Badge>
                              </div>
                              <Text
                                size={100}
                                truncate
                                block
                                title={od.path}
                                style={{
                                  fontFamily: 'Consolas, monospace',
                                  color: tokens.colorNeutralForeground3,
                                }}
                              >
                                {od.path} ({od.file_count} files · {od.modified_date})
                              </Text>
                            </div>
                            <div style={{ display: 'flex', gap: 4, flexShrink: 0 }}>
                              <Button
                                size="small"
                                appearance="subtle"
                                icon={<FolderOpen16Regular />}
                                onClick={() => api.revealInExplorer(od.path)}
                              />
                              <Button
                                size="small"
                                appearance="outline"
                                icon={<Delete16Regular />}
                                onClick={() => void prepareRecycle(od.path, true, od.scan_started_at)}
                              >
                                {lang === 'zh' ? '回收站' : 'Recycle'}
                              </Button>
                            </div>
                          </div>
                        ))}
                      </div>
                    )}
                  </div>
                )}

                <div className={styles.leftPaneFooter}>
                  <TabList
                    size="small"
                    selectedValue={leftTab}
                    onTabSelect={(_, d) =>
                      handleSelectLeftTab(d.value as 'tree' | 'files' | 'duplicates' | 'orphans')
                    }
                  >
                    <Tab value="tree">{lang === 'zh' ? '所有文件' : 'All Files'}</Tab>
                    <Tab value="files">
                      {lang === 'zh'
                        ? `最大文件 (${report.largest_files.length})`
                        : `Largest (${report.largest_files.length})`}
                    </Tab>
                    <Tab value="duplicates">
                      {lang === 'zh'
                        ? `重复大文件${dupReport ? ` (${dupReport.total_groups})` : ''}`
                        : `Duplicates${dupReport ? ` (${dupReport.total_groups})` : ''}`}
                    </Tab>
                    <Tab value="orphans">
                      {lang === 'zh'
                        ? `卸载残留${orphanDirs ? ` (${orphanDirs.length})` : ''}`
                        : `Orphans${orphanDirs ? ` (${orphanDirs.length})` : ''}`}
                    </Tab>
                  </TabList>
                </div>
              </div>

              {/* Right Pane: Extensions Table */}
              <div className={styles.paneCard}>
                <div className={`${styles.tableHeader} ${styles.extGridCols}`}>
                  <span>{lang === 'zh' ? '扩展名' : 'Ext'}</span>
                  <span>{lang === 'zh' ? '色' : 'C'}</span>
                  <span>{lang === 'zh' ? '描述' : 'Description'}</span>
                  <span style={{ textAlign: 'right' }}>{logicalSizeLabel}</span>
                  <span style={{ textAlign: 'right' }}>{lang === 'zh' ? '占比' : '%'}</span>
                  <span style={{ textAlign: 'right' }}>{lang === 'zh' ? '文件' : 'Files'}</span>
                </div>
                <div className={styles.scrollBody}>
                  {report.extensions.map((ext) => {
                    const pct =
                      report.total_bytes > 0 ? (ext.size_bytes / report.total_bytes) * 100 : 0;
                    const isAct = activeExt === ext.ext;
                    return (
                      <div
                        key={ext.ext}
                        className={`${styles.gridRow} ${styles.extGridCols} ${
                          isAct ? styles.gridRowSelected : ''
                        }`}
                        onClick={() => setActiveExt(isAct ? null : ext.ext)}
                      >
                        <span style={{ fontWeight: 600 }}>{ext.ext}</span>
                        <span
                          style={{
                            width: 14,
                            height: 14,
                            borderRadius: 3,
                            background: `linear-gradient(135deg, ${adjustHexColor(
                              ext.color,
                              0.35
                            )}, ${ext.color}, ${adjustHexColor(ext.color, -0.4)})`,
                            border: '1px solid rgba(255,255,255,0.2)',
                            display: 'inline-block',
                          }}
                        />
                        <span
                          style={{
                            overflow: 'hidden',
                            textOverflow: 'ellipsis',
                            whiteSpace: 'nowrap',
                            color: tokens.colorNeutralForeground2,
                          }}
                        >
                          {pickText(ext.description, lang)}
                        </span>
                        <span
                          style={{
                            textAlign: 'right',
                            fontWeight: 600,
                            fontVariantNumeric: 'tabular-nums',
                          }}
                        >
                          {formatBytes(ext.size_bytes)}
                        </span>
                        <span style={{ textAlign: 'right', fontVariantNumeric: 'tabular-nums' }}>
                          {pct.toFixed(1)}%
                        </span>
                        <span
                          style={{
                            textAlign: 'right',
                            color: tokens.colorNeutralForeground3,
                            fontVariantNumeric: 'tabular-nums',
                          }}
                        >
                          {ext.file_count.toLocaleString()}
                        </span>
                      </div>
                    );
                  })}
                </div>
              </div>
            </div>

            {/* Draggable Splitter */}
            <div
              className={styles.resizer}
              onMouseDown={startResize}
              title={lang === 'zh' ? '上下拖动调整分区高度' : 'Drag to resize panes'}
            >
              <div className={styles.resizerPill} />
            </div>

            {/* Bottom Pane: Full-width 3D Cushion Treemap */}
            <div className={styles.treemapCard}>
              <div className={styles.treemapToolbar}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 6, minWidth: 0 }}>
                  <Button
                    size="small"
                    appearance="subtle"
                    icon={<ArrowUp20Regular />}
                    disabled={zoomStack.length <= 1}
                    onClick={() => setZoomStack((prev) => prev.slice(0, -1))}
                  >
                    {lang === 'zh' ? '返回上级视口' : 'Zoom Out'}
                  </Button>
                  <Button
                    size="small"
                    appearance="subtle"
                    icon={<ZoomIn16Regular />}
                    disabled={!selectedNode || !selectedNode.is_dir || selectedNode.path === zoomRootNode?.path}
                    onClick={() => {
                      if (selectedNode && selectedNode.is_dir) {
                        if (selectedNode.children.length > 0) {
                          setZoomStack((prev) => [...prev, selectedNode.path]);
                        } else {
                          runAnalyze(selectedNode.path);
                        }
                      }
                    }}
                  >
                    {lang === 'zh' ? '聚焦选中目录' : 'Focus Selected'}
                  </Button>
                  {zoomStack.length > 1 && (
                    <Button
                      size="small"
                      appearance="subtle"
                      icon={<ZoomOut16Regular />}
                      onClick={() => setZoomStack([report.tree.path])}
                    >
                      {lang === 'zh' ? '重置全局' : 'Reset Zoom'}
                    </Button>
                  )}
                  <Text size={200} truncate style={{ color: tokens.colorNeutralForeground2 }}>
                    {zoomRootNode?.path}
                  </Text>
                </div>

                <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexShrink: 0 }}>
                  {activeExt && (
                    <Badge
                      size="small"
                      appearance="filled"
                      color="brand"
                      style={{ cursor: 'pointer' }}
                      onClick={() => setActiveExt(null)}
                    >
                      {lang === 'zh' ? `高亮 ${activeExt} (点击取消)` : `Filter: ${activeExt} ×`}
                    </Badge>
                  )}
                  <Text size={100} style={{ color: tokens.colorNeutralForeground3 }}>
                    {lang === 'zh'
                      ? '单击色块联动定位 · 双击目录聚焦下钻'
                      : 'Click block to locate · Double-click to zoom'}
                  </Text>
                </div>
              </div>

              <div ref={canvasWrapRef} className={styles.canvasWrap}>
                <canvas
                  ref={canvasRef}
                  style={{ width: '100%', height: '100%', display: 'block', cursor: 'crosshair' }}
                  onMouseMove={(e) => setHoveredLeaf(hitTest(e.clientX, e.clientY))}
                  onMouseLeave={() => setHoveredLeaf(null)}
                  onClick={handleCanvasClick}
                  onDoubleClick={handleCanvasDoubleClick}
                />
              </div>
            </div>
          </div>

          {/* Bottom Status Bar */}
          <div className={styles.statusBar}>
            <div style={{ display: 'flex', alignItems: 'center', gap: 10, minWidth: 0, flex: 1 }}>
              {activeDisplayNode ? (
                <>
                  <Text
                    size={200}
                    truncate
                    style={{ fontFamily: 'Consolas, monospace', color: tokens.colorNeutralForeground1 }}
                    title={activeDisplayNode.path}
                  >
                    {activeDisplayNode.path}
                  </Text>
                  <Badge size="small" appearance="tint" color="brand">
                    {logicalSizeLabel} · {formatBytes(activeDisplayNode.size_bytes)}
                  </Badge>
                  {activeDisplayNode.ext && extDescMap.get(activeDisplayNode.ext) && (
                    <Text size={100} style={{ color: tokens.colorNeutralForeground3 }}>
                      {extDescMap.get(activeDisplayNode.ext)}
                    </Text>
                  )}
                </>
              ) : (
                <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
                  {lang === 'zh' ? '就绪' : 'Ready'}
                </Text>
              )}
            </div>

            {selectedNode && !selectedNode.is_aggregate && (
              <div style={{ display: 'flex', alignItems: 'center', gap: 6, flexShrink: 0 }}>
                <Button
                  size="small"
                  appearance="subtle"
                  icon={<Copy16Regular />}
                  onClick={() => {
                    navigator.clipboard?.writeText(selectedNode.path);
                    setCopiedNotice(true);
                    setTimeout(() => setCopiedNotice(false), 1500);
                  }}
                >
                  {copiedNotice
                    ? lang === 'zh'
                      ? '已复制'
                      : 'Copied'
                    : lang === 'zh'
                    ? '复制路径'
                    : 'Copy Path'}
                </Button>
                <Button
                  size="small"
                  appearance="outline"
                  icon={<FolderOpen16Regular />}
                  onClick={() => api.revealInExplorer(selectedNode.path)}
                >
                  {lang === 'zh' ? '在资源管理器中定位' : 'Reveal'}
                </Button>
                <Button
                  size="small"
                  appearance="primary"
                  icon={<Delete16Regular />}
                  disabled={selectedNode.path === report.tree.path}
                  onClick={() => void prepareRecycle(selectedNode.path, selectedNode.is_dir, report.scan_started_at)}
                >
                  {lang === 'zh' ? '移入回收站' : 'Move to Recycle Bin'}
                </Button>
              </div>
            )}
          </div>
        </>
      )}

      <Dialog
        open={Boolean(recycleConfirmTarget)}
        onOpenChange={(_, d) => {
          if (!d.open && !recycling) setRecycleConfirmTarget(null);
        }}
      >
        <DialogSurface>
          <DialogBody>
            <DialogTitle>
              {lang === 'zh' ? '确认移入系统回收站？' : 'Move to Recycle Bin?'}
            </DialogTitle>
            <DialogContent>
              <Text block>
                {recycleConfirmTarget?.duplicateGroup
                  ? (lang === 'zh' ? '确认后逐项核对内容，仍与保留文件相同的副本才会移入回收站。'
                    : 'Each copy will be compared with the file to keep before it is moved to the Recycle Bin.')
                  : (lang === 'zh'
                    ? '确认后会再次核对目标。内容变化、路径重定向或无法完整读取时会跳过。'
                    : 'The target will be checked again. Changed, redirected, or unreadable items will be skipped.')}
              </Text>
              {recycleConfirmTarget && (
                <div style={{ marginTop: 8, display: 'flex', flexDirection: 'column', gap: 4 }}>
                  <Text
                    block
                    style={{
                      fontFamily: 'Consolas, monospace',
                      wordBreak: 'break-all',
                      fontWeight: 600,
                    }}
                  >
                    {recycleConfirmTarget.label}
                    {(recycleConfirmTarget.duplicateGroup || recycleConfirmTarget.review?.status === 'ready')
                      && ` (${formatBytes(recycleConfirmTarget.size_bytes)})`}
                  </Text>
                  {recycleConfirmTarget.paths.length > 1 &&
                    recycleConfirmTarget.paths.slice(0, 5).map((p) => (
                      <Text
                        key={p}
                        size={100}
                        block
                        style={{ fontFamily: 'Consolas, monospace', color: tokens.colorNeutralForeground3 }}
                      >
                        • {p}
                      </Text>
                    ))}
                </div>
              )}
              {recycleConfirmTarget && !recycleConfirmTarget.duplicateGroup && (
                <div style={{ marginTop: 8 }}>
                  {recycleConfirmTarget.reviewError ? (
                    <MessageBar intent="error"><MessageBarBody>{recycleConfirmTarget.reviewError}</MessageBarBody></MessageBar>
                  ) : recycleConfirmTarget.review?.status === 'blocked' ? (
                    <MessageBar intent="warning">
                      <MessageBarBody>{skipReasons[recycleConfirmTarget.review.reason][lang]}</MessageBarBody>
                    </MessageBar>
                  ) : recycleConfirmTarget.review?.status === 'ready' ? (
                    <Text size={200}>
                      {lang === 'zh'
                        ? `${recycleConfirmTarget.review.file_count} 个文件 · ${recycleConfirmTarget.review.dir_count} 个目录`
                        : `${recycleConfirmTarget.review.file_count} files · ${recycleConfirmTarget.review.dir_count} folders`}
                    </Text>
                  ) : (
                    <Spinner size="small" label={lang === 'zh' ? '正在核对目标…' : 'Reviewing target…'} />
                  )}
                </div>
              )}
            </DialogContent>
            <DialogActions>
              <Button
                appearance="secondary"
                disabled={recycling}
                onClick={() => setRecycleConfirmTarget(null)}
              >
                {lang === 'zh' ? '取消' : 'Cancel'}
              </Button>
              <Button
                appearance="primary"
                disabled={recycling || Boolean(scan.active)
                  || (!recycleConfirmTarget?.duplicateGroup && recycleConfirmTarget?.review?.status !== 'ready')}
                onClick={confirmRecycle}
              >
                {recycling
                  ? lang === 'zh'
                    ? '移动中...'
                    : 'Moving...'
                  : lang === 'zh'
                  ? '确认移入回收站'
                  : 'Move to Recycle Bin'}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </div>
  );
};
