import React, { useEffect, useMemo, useRef, useState } from 'react';
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
  ArrowUp24Regular,
  Document20Regular,
  Folder20Regular,
  FolderOpen20Regular,
  Search20Regular,
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

interface LayoutRect {
  node: DiskTreeNode;
  x: number;
  y: number;
  w: number;
  h: number;
  depth: number;
  children: LayoutRect[];
}

const FALLBACK_DIR_COLORS = [
  '#2B579A',
  '#217346',
  '#B7472A',
  '#742774',
  '#038387',
  '#4F6BED',
  '#8E562E',
  '#5C2D91',
];

const useStyles = makeStyles({
  root: {
    display: 'flex',
    flexDirection: 'column',
    height: '100%',
    gap: '10px',
    overflow: 'hidden',
  },
  toolbar: {
    display: 'flex',
    flexDirection: 'column',
    gap: '8px',
    padding: '10px 14px',
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
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
  topSplit: {
    display: 'grid',
    gridTemplateColumns: '1.2fr 1fr',
    gap: '10px',
    height: '220px',
    minHeight: '190px',
    flexShrink: 0,
  },
  panel: {
    display: 'flex',
    flexDirection: 'column',
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    padding: '10px',
    gap: '6px',
    minHeight: 0,
    overflow: 'hidden',
  },
  scrollList: {
    flex: 1,
    overflowY: 'auto',
    display: 'flex',
    flexDirection: 'column',
    gap: '4px',
  },
  treeRow: {
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'space-between',
    gap: '8px',
    padding: '5px 8px',
    borderRadius: tokens.borderRadiusSmall,
    cursor: 'pointer',
    ':hover': {
      backgroundColor: tokens.colorNeutralBackground1Hover,
    },
  },
  extRow: {
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'space-between',
    gap: '8px',
    padding: '5px 8px',
    borderRadius: tokens.borderRadiusSmall,
    cursor: 'pointer',
    ':hover': {
      backgroundColor: tokens.colorNeutralBackground1Hover,
    },
  },
  bottomCanvasCard: {
    flex: 1,
    display: 'flex',
    flexDirection: 'column',
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    padding: '10px',
    gap: '8px',
    minHeight: 0,
    overflow: 'hidden',
  },
  treemapHost: {
    position: 'relative',
    flex: 1,
    minHeight: '180px',
    borderRadius: tokens.borderRadiusMedium,
    backgroundColor: tokens.colorNeutralBackground4,
    overflow: 'hidden',
    userSelect: 'none',
  },
});

/**
 * Bruls–Huizing–van Wijk Squarified Treemap layout algorithm.
 * Minimizes rectangle aspect ratios so blocks are as close to squares as possible.
 */
function squarifyLayout(
  nodes: DiskTreeNode[],
  x: number,
  y: number,
  w: number,
  h: number,
  depth = 0
): LayoutRect[] {
  const valid = nodes.filter((n) => n.size_bytes > 0);
  if (valid.length === 0 || w <= 2 || h <= 2) return [];

  const total = valid.reduce((a, b) => a + b.size_bytes, 0);
  if (total <= 0) return [];

  const area = w * h;
  const items = valid.map((node) => ({
    node,
    area: (node.size_bytes / total) * area,
  }));

  const rects: LayoutRect[] = [];
  let cx = x;
  let cy = y;
  let cw = w;
  let ch = h;
  let row: { node: DiskTreeNode; area: number }[] = [];

  const worstRatio = (r: { area: number }[], side: number): number => {
    if (r.length === 0 || side <= 0) return Infinity;
    const sum = r.reduce((a, b) => a + b.area, 0);
    let maxA = 0;
    let minA = Infinity;
    for (const it of r) {
      if (it.area > maxA) maxA = it.area;
      if (it.area < minA) minA = it.area;
    }
    const s2 = side * side;
    const sum2 = sum * sum;
    return Math.max((s2 * maxA) / sum2, sum2 / (s2 * Math.max(minA, 0.0001)));
  };

  const commitRow = (r: { node: DiskTreeNode; area: number }[]) => {
    const sum = r.reduce((a, b) => a + b.area, 0);
    if (sum <= 0) return;
    if (cw >= ch) {
      const stripW = sum / ch;
      let oy = cy;
      for (const it of r) {
        const ih = it.area / stripW;
        const inner =
          depth < 1 && it.node.is_dir && it.node.children.length > 0 && stripW >= 54 && ih >= 46
            ? squarifyLayout(it.node.children, cx + 2, oy + 16, stripW - 4, ih - 18, depth + 1)
            : [];
        rects.push({
          node: it.node,
          x: cx,
          y: oy,
          w: stripW,
          h: ih,
          depth,
          children: inner,
        });
        oy += ih;
      }
      cx += stripW;
      cw -= stripW;
    } else {
      const stripH = sum / cw;
      let ox = cx;
      for (const it of r) {
        const iw = it.area / stripH;
        const inner =
          depth < 1 && it.node.is_dir && it.node.children.length > 0 && iw >= 54 && stripH >= 46
            ? squarifyLayout(it.node.children, ox + 2, cy + 16, iw - 4, stripH - 18, depth + 1)
            : [];
        rects.push({
          node: it.node,
          x: ox,
          y: cy,
          w: iw,
          h: stripH,
          depth,
          children: inner,
        });
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
    const candidate = [...row, item];
    if (worstRatio(candidate, side) <= worstRatio(row, side)) {
      row = candidate;
    } else {
      commitRow(row);
      row = [item];
    }
  }
  if (row.length > 0) commitRow(row);
  return rects;
}

export const DiskAnalyzerView: React.FC<Props> = ({ lang }) => {
  const styles = useStyles();
  const [presets, setPresets] = useState<DiskPreset[]>([]);
  const [targetPath, setTargetPath] = useState('');
  const [loading, setLoading] = useState(false);
  const [report, setReport] = useState<DiskAnalysisReport | null>(null);
  const [navStack, setNavStack] = useState<DiskTreeNode[]>([]);
  const [bottomTab, setBottomTab] = useState<'treemap' | 'files'>('treemap');
  const [activeExt, setActiveExt] = useState<string | null>(null);
  const [hoveredNode, setHoveredNode] = useState<DiskTreeNode | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const hostRef = useRef<HTMLDivElement | null>(null);
  const [boxSize, setBoxSize] = useState({ w: 800, h: 320 });

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
    const el = hostRef.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      for (const entry of entries) {
        const { width, height } = entry.contentRect;
        if (width > 10 && height > 10) {
          setBoxSize({ w: width, h: height });
        }
      }
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [report, bottomTab]);

  const runAnalyze = async (pathToScan: string) => {
    const trimmed = pathToScan.trim();
    if (!trimmed) return;
    setLoading(true);
    setErrorMsg(null);
    setActiveExt(null);
    setHoveredNode(null);
    try {
      const res = await api.analyzeDisk(trimmed, 3);
      setReport(res);
      setTargetPath(res.root_path);
      setNavStack([res.tree]);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setLoading(false);
    }
  };

  const currentNode = navStack.length > 0 ? navStack[navStack.length - 1] : report?.tree || null;

  const extColorMap = useMemo(() => {
    const map = new Map<string, string>();
    report?.extensions.forEach((e) => map.set(e.ext, e.color));
    return map;
  }, [report]);

  const treemapRects = useMemo(() => {
    if (!currentNode || bottomTab !== 'treemap') return [];
    return squarifyLayout(currentNode.children, 0, 0, boxSize.w, boxSize.h, 0);
  }, [currentNode, boxSize, bottomTab]);

  const handleDrillDown = (node: DiskTreeNode) => {
    if (node.is_aggregate || !node.is_dir) return;
    if (node.children.length > 0) {
      setNavStack((prev) => [...prev, node]);
    } else {
      // Pruned leaf directory: launch a targeted deep scan from this path
      runAnalyze(node.path);
    }
  };

  const formatNodeTitle = (node: DiskTreeNode) => {
    if (!node.is_aggregate) return node.name;
    return lang === 'zh'
      ? `[其余 ${node.aggregate_count} 个较小项]`
      : `[+${node.aggregate_count} smaller items]`;
  };

  const nodeColor = (node: DiskTreeNode, idx: number) => {
    if (node.is_aggregate) return '#605E5C';
    if (!node.is_dir && node.ext) {
      return extColorMap.get(node.ext) || '#0078D4';
    }
    return FALLBACK_DIR_COLORS[idx % FALLBACK_DIR_COLORS.length];
  };

  const matchesActiveExt = (node: DiskTreeNode): boolean => {
    if (!activeExt) return true;
    if (!node.is_dir) return node.ext === activeExt;
    return node.children.some((c) => matchesActiveExt(c));
  };

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
            {lang === 'zh' ? '快捷目标：' : 'Presets:'}
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
          <div style={{ display: 'flex', gap: 8, flex: 1, minWidth: 300 }}>
            <Input
              style={{ flex: 1 }}
              contentBefore={<Search20Regular />}
              placeholder={
                lang === 'zh'
                  ? '输入任意目录或盘符路径（例如 C:\\Users）'
                  : 'Enter any folder or drive path (e.g. C:\\Users)'
              }
              value={targetPath}
              onChange={(_, d) => setTargetPath(d.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') runAnalyze(targetPath);
              }}
            />
            <Button
              appearance="primary"
              disabled={loading || !targetPath.trim()}
              onClick={() => runAnalyze(targetPath)}
            >
              {loading
                ? lang === 'zh'
                  ? '并行扫描中...'
                  : 'Analyzing...'
                : lang === 'zh'
                ? '开始空间分析'
                : 'Analyze Space'}
            </Button>
          </div>

          {report && (
            <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
              <Badge appearance="tint" color="brand">
                {formatBytes(report.total_bytes)}
              </Badge>
              <Badge appearance="outline">
                {report.total_files.toLocaleString()} {lang === 'zh' ? '文件' : 'files'} /{' '}
                {report.total_dirs.toLocaleString()} {lang === 'zh' ? '目录' : 'dirs'}
              </Badge>
              <Badge appearance="tint" color="success">
                {report.elapsed_ms} ms
              </Badge>
            </div>
          )}
        </div>
      </div>

      {loading ? (
        <div
          style={{
            flex: 1,
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
          }}
        >
          <Spinner
            label={
              lang === 'zh'
                ? '正在使用并行工作窃取扫描目录树并构建扩展名分布...'
                : 'Scanning directory tree in parallel...'
            }
          />
        </div>
      ) : !report || !currentNode ? (
        <div
          className={styles.panel}
          style={{ flex: 1, alignItems: 'center', justifyContent: 'center' }}
        >
          <Text size={400} weight="semibold">
            {lang === 'zh'
              ? 'WinDirStat 风格空间分析器'
              : 'WinDirStat-Style Disk Space Analyzer'}
          </Text>
          <Text size={200} style={{ color: tokens.colorNeutralForeground3 }}>
            {lang === 'zh'
              ? '点击上方快捷目录（如 AppData\\Local 或 C:\\）即可可视化定位大文件与目录占比。'
              : 'Select a preset path or enter a folder above to visualize space usage.'}
          </Text>
        </div>
      ) : (
        <>
          <div className={styles.topSplit}>
            {/* Top Left: Directory Tree Breakdown */}
            <div className={styles.panel}>
              <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 6, minWidth: 0 }}>
                  <Button
                    size="small"
                    icon={<ArrowUp24Regular />}
                    disabled={navStack.length <= 1}
                    onClick={() => setNavStack((prev) => prev.slice(0, -1))}
                  />
                  <Text size={300} weight="bold" truncate>
                    {currentNode.path} ({formatBytes(currentNode.size_bytes)})
                  </Text>
                </div>
                <Button
                  size="small"
                  appearance="subtle"
                  icon={<FolderOpen20Regular />}
                  onClick={() => api.revealInExplorer(currentNode.path)}
                >
                  {lang === 'zh' ? '打开目录' : 'Reveal'}
                </Button>
              </div>

              <div className={styles.scrollList}>
                {currentNode.children.map((ch) => {
                  const pct =
                    currentNode.size_bytes > 0
                      ? (ch.size_bytes / currentNode.size_bytes) * 100
                      : 0;
                  return (
                    <div
                      key={`${ch.path}-${ch.name}`}
                      className={styles.treeRow}
                      onClick={() => handleDrillDown(ch)}
                      onMouseEnter={() => setHoveredNode(ch)}
                    >
                      <div style={{ display: 'flex', alignItems: 'center', gap: 6, minWidth: 0, flex: 1 }}>
                        {ch.is_dir ? (
                          <Folder20Regular style={{ color: tokens.colorPaletteDarkOrangeForeground1, flexShrink: 0 }} />
                        ) : (
                          <Document20Regular style={{ color: tokens.colorBrandForeground1, flexShrink: 0 }} />
                        )}
                        <Text size={200} truncate>
                          {formatNodeTitle(ch)}
                        </Text>
                      </div>
                      <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexShrink: 0 }}>
                        <div
                          style={{
                            width: 72,
                            height: 6,
                            borderRadius: 4,
                            backgroundColor: tokens.colorNeutralBackground4,
                            overflow: 'hidden',
                          }}
                        >
                          <div
                            style={{
                              width: `${Math.max(2, pct)}%`,
                              height: '100%',
                              backgroundColor: tokens.colorBrandBackground,
                            }}
                          />
                        </div>
                        <Text size={200} style={{ width: 42, textAlign: 'right' }}>
                          {pct.toFixed(1)}%
                        </Text>
                        <Text size={200} weight="semibold" style={{ width: 68, textAlign: 'right' }}>
                          {formatBytes(ch.size_bytes)}
                        </Text>
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>

            {/* Top Right: Extension Legend Breakdown */}
            <div className={styles.panel}>
              <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
                <Text size={300} weight="bold">
                  {lang === 'zh' ? '文件扩展名统计（点击高亮色块）' : 'Extensions (Click to Highlight)'}
                </Text>
                {activeExt && (
                  <Button size="small" appearance="subtle" onClick={() => setActiveExt(null)}>
                    {lang === 'zh' ? '取消高亮' : 'Clear'}
                  </Button>
                )}
              </div>

              <div className={styles.scrollList}>
                {report.extensions.map((ext) => {
                  const pct =
                    report.total_bytes > 0 ? (ext.size_bytes / report.total_bytes) * 100 : 0;
                  const isActive = activeExt === ext.ext;
                  return (
                    <div
                      key={ext.ext}
                      className={styles.extRow}
                      style={{
                        backgroundColor: isActive ? tokens.colorNeutralBackground1Selected : undefined,
                      }}
                      onClick={() => setActiveExt(isActive ? null : ext.ext)}
                    >
                      <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                        <span
                          style={{
                            width: 12,
                            height: 12,
                            borderRadius: 3,
                            backgroundColor: ext.color,
                            display: 'inline-block',
                          }}
                        />
                        <Text size={200} weight="semibold">
                          {ext.ext}
                        </Text>
                        <Text size={100} style={{ color: tokens.colorNeutralForeground3 }}>
                          ({ext.file_count.toLocaleString()})
                        </Text>
                      </div>
                      <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                        <Text size={200}>{pct.toFixed(1)}%</Text>
                        <Text size={200} weight="semibold" style={{ width: 70, textAlign: 'right' }}>
                          {formatBytes(ext.size_bytes)}
                        </Text>
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>
          </div>

          {/* Bottom: Squarified Treemap or Top 100 Largest Files */}
          <div className={styles.bottomCanvasCard}>
            <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', gap: 10 }}>
              <TabList
                selectedValue={bottomTab}
                onTabSelect={(_, d) => setBottomTab(d.value as 'treemap' | 'files')}
                size="small"
              >
                <Tab value="treemap">
                  {lang === 'zh' ? 'Squarified Treemap 矩形树图' : 'Squarified Treemap'}
                </Tab>
                <Tab value="files">
                  {lang === 'zh'
                    ? `Top ${report.largest_files.length} 最大单文件`
                    : `Top ${report.largest_files.length} Largest Files`}
                </Tab>
              </TabList>

              {hoveredNode && (
                <Text size={200} truncate style={{ color: tokens.colorNeutralForeground2 }}>
                  {hoveredNode.path} · <strong>{formatBytes(hoveredNode.size_bytes)}</strong>
                </Text>
              )}
            </div>

            {bottomTab === 'treemap' ? (
              <div ref={hostRef} className={styles.treemapHost}>
                {treemapRects.map((rect, idx) => {
                  const bg = nodeColor(rect.node, idx);
                  const dimmed = activeExt && !matchesActiveExt(rect.node);
                  return (
                    <div
                      key={`${rect.node.path}-${rect.node.name}-${idx}`}
                      title={`${formatNodeTitle(rect.node)}\n${rect.node.path}\n${formatBytes(rect.node.size_bytes)}`}
                      onMouseEnter={() => setHoveredNode(rect.node)}
                      onClick={() => handleDrillDown(rect.node)}
                      style={{
                        position: 'absolute',
                        left: rect.x + 1,
                        top: rect.y + 1,
                        width: Math.max(0, rect.w - 2),
                        height: Math.max(0, rect.h - 2),
                        backgroundColor: rect.children.length > 0 ? '#1F242D' : bg,
                        border:
                          rect.children.length > 0
                            ? `1px solid ${bg}`
                            : '1px solid rgba(255,255,255,0.22)',
                        boxSizing: 'border-box',
                        borderRadius: 3,
                        overflow: 'hidden',
                        cursor: rect.node.is_dir && !rect.node.is_aggregate ? 'pointer' : 'default',
                        opacity: dimmed ? 0.25 : 0.94,
                        transition: 'opacity 120ms ease',
                      }}
                    >
                      {rect.w >= 44 && rect.h >= 18 && (
                        <div
                          style={{
                            padding: '1px 4px',
                            fontSize: '11px',
                            lineHeight: '14px',
                            color: '#FFFFFF',
                            textShadow: '0 1px 2px rgba(0,0,0,0.75)',
                            whiteSpace: 'nowrap',
                            overflow: 'hidden',
                            textOverflow: 'ellipsis',
                            fontWeight: rect.children.length > 0 ? 600 : 400,
                          }}
                        >
                          {formatNodeTitle(rect.node)} ({formatBytes(rect.node.size_bytes)})
                        </div>
                      )}
                      {rect.children.map((ch, cIdx) => {
                        const cBg = nodeColor(ch.node, cIdx + 3);
                        const cDimmed = activeExt && !matchesActiveExt(ch.node);
                        return (
                          <div
                            key={`${ch.node.path}-${ch.node.name}-${cIdx}`}
                            title={`${formatNodeTitle(ch.node)}\n${ch.node.path}\n${formatBytes(ch.node.size_bytes)}`}
                            onMouseEnter={(e) => {
                              e.stopPropagation();
                              setHoveredNode(ch.node);
                            }}
                            onClick={(e) => {
                              e.stopPropagation();
                              handleDrillDown(ch.node);
                            }}
                            style={{
                              position: 'absolute',
                              left: ch.x - rect.x,
                              top: ch.y - rect.y,
                              width: Math.max(0, ch.w - 1),
                              height: Math.max(0, ch.h - 1),
                              backgroundColor: cBg,
                              border: '1px solid rgba(255,255,255,0.2)',
                              boxSizing: 'border-box',
                              borderRadius: 2,
                              overflow: 'hidden',
                              opacity: cDimmed ? 0.22 : 0.96,
                            }}
                          >
                            {ch.w >= 46 && ch.h >= 16 && (
                              <div
                                style={{
                                  padding: '1px 3px',
                                  fontSize: '10px',
                                  color: '#FFFFFF',
                                  textShadow: '0 1px 2px rgba(0,0,0,0.8)',
                                  whiteSpace: 'nowrap',
                                  overflow: 'hidden',
                                  textOverflow: 'ellipsis',
                                }}
                              >
                                {formatNodeTitle(ch.node)}
                              </div>
                            )}
                          </div>
                        );
                      })}
                    </div>
                  );
                })}
              </div>
            ) : (
              <div className={styles.scrollList}>
                {report.largest_files.map((f, idx) => (
                  <div
                    key={`${f.path}-${idx}`}
                    className={styles.treeRow}
                    style={{ backgroundColor: tokens.colorNeutralBackground1 }}
                  >
                    <div style={{ display: 'flex', flexDirection: 'column', minWidth: 0, flex: 1 }}>
                      <Text size={200} weight="semibold" truncate>
                        {f.name}
                      </Text>
                      <Text
                        size={100}
                        truncate
                        style={{ color: tokens.colorNeutralForeground3, fontFamily: 'Consolas, monospace' }}
                      >
                        {f.path}
                      </Text>
                    </div>
                    <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                      <Badge size="small" appearance="outline">
                        {f.ext}
                      </Badge>
                      <Text size={200} weight="bold" style={{ width: 76, textAlign: 'right' }}>
                        {formatBytes(f.size_bytes)}
                      </Text>
                      <Button
                        size="small"
                        appearance="subtle"
                        icon={<FolderOpen20Regular />}
                        onClick={() => api.revealInExplorer(f.path)}
                      >
                        {lang === 'zh' ? '定位' : 'Reveal'}
                      </Button>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        </>
      )}
    </div>
  );
};
