import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  Badge,
  Button,
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
  Lang,
  api,
  formatBytes,
  pickText,
} from '../api';

interface Props {
  lang: Lang;
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
    gridTemplateColumns: 'minmax(180px, 1fr) 68px 78px 40px',
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

export const DiskAnalyzerView: React.FC<Props> = ({ lang }) => {
  const styles = useStyles();
  const [presets, setPresets] = useState<DiskPreset[]>([]);
  const [targetPath, setTargetPath] = useState('');
  const [loading, setLoading] = useState(false);
  const [report, setReport] = useState<DiskAnalysisReport | null>(null);
  const [expandedPaths, setExpandedPaths] = useState<Set<string>>(new Set());
  const [loadingDirs, setLoadingDirs] = useState<Set<string>>(new Set());
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [zoomStack, setZoomStack] = useState<string[]>([]);
  const [leftTab, setLeftTab] = useState<'tree' | 'files'>('tree');
  const [activeExt, setActiveExt] = useState<string | null>(null);
  const [hoveredLeaf, setHoveredLeaf] = useState<PaintedLeaf | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
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
    if (!trimmed) return;
    setLoading(true);
    setErrorMsg(null);
    setActiveExt(null);
    setHoveredLeaf(null);
    try {
      const res = await api.analyzeDisk(trimmed, 4);
      setReport(res);
      setTargetPath(res.root_path);
      setExpandedPaths(new Set([res.tree.path]));
      setSelectedPath(res.tree.path);
      setZoomStack([res.tree.path]);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setLoading(false);
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
      setLoadingDirs((prev) => new Set(prev).add(p));
      try {
        const sub = await api.analyzeDisk(p, 2);
        setReport((prev) =>
          prev ? { ...prev, tree: spliceTreeChildren(prev.tree, p, sub.tree.children) } : prev
        );
      } catch (e) {
        setErrorMsg(String(e));
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

  const formatNodeTitle = (node: DiskTreeNode) =>
    node.is_aggregate
      ? lang === 'zh'
        ? `[其余 ${node.aggregate_count} 个较小项]`
        : `[+${node.aggregate_count} smaller items]`
      : node.name;

  return (
    <div className={styles.root}>
      {errorMsg && (
        <MessageBar intent="error">
          <MessageBarBody>{errorMsg}</MessageBarBody>
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
              disabled={loading}
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
              disabled={loading || !targetPath.trim()}
              onClick={() => runAnalyze(targetPath)}
            >
              {loading
                ? lang === 'zh'
                  ? '扫描中...'
                  : 'Scanning...'
                : lang === 'zh'
                ? '开始分析'
                : 'Analyze'}
            </Button>
          </div>

          {report && (
            <div style={{ display: 'flex', gap: 6, alignItems: 'center', flexWrap: 'wrap' }}>
              <Badge size="small" appearance="tint" color="brand">
                {formatBytes(report.total_bytes)}
              </Badge>
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
      </div>

      {loading ? (
        <div style={{ flex: 1, display: 'flex', alignItems: 'center', justifyContent: 'center' }}>
          <Spinner
            label={
              lang === 'zh'
                ? '正在并行扫描目录树并构建立体空间分布图...'
                : 'Scanning directory tree and computing layout...'
            }
          />
        </div>
      ) : !report ? (
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
                      <span style={{ textAlign: 'right' }}>{lang === 'zh' ? '大小' : 'Size'}</span>
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
                ) : (
                  <>
                    <div className={`${styles.tableHeader} ${styles.largeFileCols}`}>
                      <span>{lang === 'zh' ? '文件路径' : 'File Path'}</span>
                      <span>{lang === 'zh' ? '类型' : 'Ext'}</span>
                      <span style={{ textAlign: 'right' }}>{lang === 'zh' ? '大小' : 'Size'}</span>
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
                          </span>
                          <Button
                            size="small"
                            appearance="subtle"
                            icon={<FolderOpen16Regular />}
                            onClick={(e) => {
                              e.stopPropagation();
                              api.revealInExplorer(lf.path);
                            }}
                          />
                        </div>
                      ))}
                    </div>
                  </>
                )}

                <div className={styles.leftPaneFooter}>
                  <TabList
                    size="small"
                    selectedValue={leftTab}
                    onTabSelect={(_, d) => setLeftTab(d.value as 'tree' | 'files')}
                  >
                    <Tab value="tree">{lang === 'zh' ? '所有文件' : 'All Files'}</Tab>
                    <Tab value="files">
                      {lang === 'zh'
                        ? `最大文件 (${report.largest_files.length})`
                        : `Largest Files (${report.largest_files.length})`}
                    </Tab>
                  </TabList>
                  <Text size={100} style={{ color: tokens.colorNeutralForeground3 }}>
                    {lang === 'zh' ? '双击目录展开 / 懒加载深层子项' : 'Double-click dir to expand'}
                  </Text>
                </div>
              </div>

              {/* Right Pane: Extensions Table */}
              <div className={styles.paneCard}>
                <div className={`${styles.tableHeader} ${styles.extGridCols}`}>
                  <span>{lang === 'zh' ? '扩展名' : 'Ext'}</span>
                  <span>{lang === 'zh' ? '色' : 'C'}</span>
                  <span>{lang === 'zh' ? '描述' : 'Description'}</span>
                  <span style={{ textAlign: 'right' }}>{lang === 'zh' ? '大小' : 'Size'}</span>
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
                    {formatBytes(activeDisplayNode.size_bytes)}
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
                  appearance="primary"
                  icon={<FolderOpen16Regular />}
                  onClick={() => api.revealInExplorer(selectedNode.path)}
                >
                  {lang === 'zh' ? '在资源管理器中定位' : 'Reveal in Explorer'}
                </Button>
              </div>
            )}
          </div>
        </>
      )}
    </div>
  );
};
