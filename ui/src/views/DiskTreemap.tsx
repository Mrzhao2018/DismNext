import { squarifyRects } from '../diskAnalysis';
import { useEffect, useMemo, useRef, useState, useSyncExternalStore, type PointerEvent, type MouseEvent } from 'react';
import { Badge, Text, tokens } from '@fluentui/react-components';
import { formatBytes, type DiskTreeNode, type Lang } from '../api';

export interface PaintedLeaf {
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

export function adjustHexColor(hex: string, factor: number): string {
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

export function createTreemapHover() {
  let leaf: PaintedLeaf | null = null;
  const subscribers = new Set<() => void>();
  return {
    get: () => leaf,
    subscribe: (callback: () => void) => {
      subscribers.add(callback);
      return () => { subscribers.delete(callback); };
    },
    set: (next: PaintedLeaf | null) => {
      if (leaf === next) return;
      leaf = next;
      subscribers.forEach((callback) => callback());
    },
  };
}

type TreemapHover = ReturnType<typeof createTreemapHover>;

interface TreemapCanvasProps {
  className: string;
  root: DiskTreeNode | null;
  activeExt: string | null;
  selectedPath: string | null;
  extColorMap: Map<string, string>;
  visible: boolean;
  hover: TreemapHover;
  onSelect: (leaf: PaintedLeaf) => void;
  onZoom: (leaf: PaintedLeaf) => void;
}

export function TreemapCanvas({
  className, root: zoomRootNode, activeExt, selectedPath, extColorMap, visible, hover, onSelect, onZoom,
}: TreemapCanvasProps) {
  const canvasWrapRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const overlayRef = useRef<HTMLCanvasElement>(null);
  const [canvasSize, setCanvasSize] = useState({ w: 800, h: 260 });
  const pendingFrame = useRef<number | null>(null);
  const pointer = useRef({ x: 0, y: 0 });
  const hoveredLeaf = useSyncExternalStore(hover.subscribe, hover.get);

  useEffect(() => {
    const host = canvasWrapRef.current;
    if (!host) return;
    const observer = new ResizeObserver(([entry]) => {
      const w = Math.floor(entry.contentRect.width);
      const h = Math.floor(entry.contentRect.height);
      if (w > 20 && h > 20) {
        setCanvasSize((previous) => previous.w === w && previous.h === h ? previous : { w, h });
      }
    });
    observer.observe(host);
    return () => observer.disconnect();
  }, []);

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
    const width = Math.max(1, Math.floor(canvasSize.w * dpr));
    const height = Math.max(1, Math.floor(canvasSize.h * dpr));
    if (canvas.width !== width) canvas.width = width;
    if (canvas.height !== height) canvas.height = height;
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

  }, [paintedLeaves, canvasSize, activeExt, selectedPath]);

  useEffect(() => {
    const overlay = overlayRef.current;
    if (!overlay) return;
    const dpr = window.devicePixelRatio || 1;
    overlay.width = Math.max(1, Math.floor(canvasSize.w * dpr));
    overlay.height = Math.max(1, Math.floor(canvasSize.h * dpr));
    overlay.getContext('2d')?.setTransform(dpr, 0, 0, dpr, 0, 0);
  }, [canvasSize]);

  useEffect(() => {
    const ctx = overlayRef.current?.getContext('2d');
    if (!ctx) return;
    ctx.clearRect(0, 0, canvasSize.w, canvasSize.h);
    if (!hoveredLeaf || !visible) return;
    ctx.strokeStyle = '#60CDFF';
    ctx.lineWidth = 2;
    ctx.strokeRect(hoveredLeaf.x + 1, hoveredLeaf.y + 1,
      Math.max(1, hoveredLeaf.w - 2), Math.max(1, hoveredLeaf.h - 2));
  }, [canvasSize, hoveredLeaf, visible]);

  const clearHover = () => {
    if (pendingFrame.current !== null) {
      window.cancelAnimationFrame(pendingFrame.current);
      pendingFrame.current = null;
    }
    hover.set(null);
  };

  useEffect(() => () => {
    if (pendingFrame.current !== null) {
      window.cancelAnimationFrame(pendingFrame.current);
      pendingFrame.current = null;
    }
    hover.set(null);
  }, [hover, paintedLeaves, visible]);

  const hitTest = (clientX: number, clientY: number) => {
    const rect = canvasRef.current?.getBoundingClientRect();
    if (!rect) return null;
    const x = clientX - rect.left;
    const y = clientY - rect.top;
    for (let i = paintedLeaves.length - 1; i >= 0; i--) {
      const leaf = paintedLeaves[i];
      if (x >= leaf.x && x <= leaf.x + leaf.w && y >= leaf.y && y <= leaf.y + leaf.h) return leaf;
    }
    return null;
  };

  const movePointer = (event: PointerEvent<HTMLCanvasElement>) => {
    if (event.pointerType === 'touch') return;
    pointer.current = { x: event.clientX, y: event.clientY };
    if (pendingFrame.current !== null) return;
    pendingFrame.current = window.requestAnimationFrame(() => {
      pendingFrame.current = null;
      hover.set(hitTest(pointer.current.x, pointer.current.y));
    });
  };

  const select = (event: MouseEvent<HTMLCanvasElement>, action: (leaf: PaintedLeaf) => void) => {
    const leaf = hitTest(event.clientX, event.clientY);
    if (leaf) action(leaf);
  };

  return (
    <div ref={canvasWrapRef} className={className}>
      <canvas
        ref={canvasRef}
        style={{ position: 'absolute', inset: 0, width: '100%', height: '100%', cursor: 'crosshair' }}
        onPointerMove={movePointer}
        onPointerLeave={clearHover}
        onClick={(event) => select(event, onSelect)}
        onDoubleClick={(event) => select(event, onZoom)}
      />
      <canvas
        ref={overlayRef}
        aria-hidden="true"
        style={{ position: 'absolute', inset: 0, width: '100%', height: '100%', pointerEvents: 'none' }}
      />
    </div>
  );
}

export function TreemapReadout({ hover, selectedNode, logicalSizeLabel, extDescMap, lang }: {
  hover: TreemapHover;
  selectedNode: DiskTreeNode | null;
  logicalSizeLabel: string;
  extDescMap: Map<string, string>;
  lang: Lang;
}) {
  const activeDisplayNode = useSyncExternalStore(hover.subscribe, hover.get)?.node || selectedNode;
  return (
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
            <Text size={100} truncate style={{ color: tokens.colorNeutralForeground3 }}>
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
  );
}
