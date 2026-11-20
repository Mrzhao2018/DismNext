use crate::catalog::LocalizedText;
use crate::disk_allocation::AllocationScan;
pub use crate::disk_allocation::DiskAllocation;
use crate::path_recycle::{recycle_path_immediately, scan_timestamp};
use crate::scan_progress::{ScanPhase, ScanProgress};
use crate::sysinfo::SystemInfo;
use crate::win_util::{windows_dir_or_default, FILE_ATTRIBUTE_REPARSE_POINT};
use anyhow::{anyhow, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

mod duplicates;
mod orphans;
mod protected;

pub use duplicates::{
    find_duplicate_files, find_duplicate_files_with_progress, DuplicateFileEntry, DuplicateGroup,
    DuplicateScanReport,
};
pub(crate) use orphans::measure_dir_stats;
pub use orphans::{find_orphaned_app_dirs, invalidate_orphans_cache, OrphanDirEntry};
pub use protected::is_protected_recycle_target;

const MAX_CHILDREN_PER_NODE: usize = 60;
const MAX_LARGEST_FILES: usize = 100;
const MAX_EXTENSIONS: usize = 40;

const PALETTE: &[&str] = &[
    "#0078D4", "#107C10", "#D83B01", "#8764B8", "#00B7C3", "#E3008C", "#CA5010", "#038387",
    "#498205", "#C239B3", "#004E8C", "#5C2D91", "#8E562E", "#0099BC", "#E74856", "#7A7574",
    "#4F6BED", "#0B6A0B", "#9A0089", "#B4009E",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskTreeNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_aggregate: bool,
    pub is_cache_hint: bool,
    pub aggregate_count: usize,
    pub size_bytes: u64,
    pub file_count: usize,
    pub dir_count: usize,
    pub ext: Option<String>,
    pub children: Vec<DiskTreeNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionStat {
    pub ext: String,
    pub description: LocalizedText,
    pub size_bytes: u64,
    pub file_count: usize,
    pub color: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LargeFileEntry {
    pub path: String,
    pub name: String,
    pub ext: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocated_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskPreset {
    pub label: LocalizedText,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskAnalysisReport {
    pub root_path: String,
    #[serde(default)]
    pub scan_started_at: String,
    pub elapsed_ms: u64,
    pub total_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocation: Option<DiskAllocation>,
    pub total_files: usize,
    pub total_dirs: usize,
    pub tree: DiskTreeNode,
    pub extensions: Vec<ExtensionStat>,
    pub largest_files: Vec<LargeFileEntry>,
}

struct ScanAccumulator {
    node: RawNode,
    exts: HashMap<String, (u64, usize)>,
    top_files: LargestFiles,
}

#[derive(Default)]
struct LargestFiles {
    heap: BinaryHeap<RankedFile>,
    sequence: usize,
}

struct RankedFile {
    entry: LargeFileEntry,
    order: usize,
}

impl Ord for RankedFile {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .entry
            .size_bytes
            .cmp(&self.entry.size_bytes)
            .then_with(|| self.order.cmp(&other.order))
    }
}

impl PartialOrd for RankedFile {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for RankedFile {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for RankedFile {}

impl LargestFiles {
    fn accepts(&self, size: u64) -> bool {
        self.heap.len() < MAX_LARGEST_FILES
            || self
                .heap
                .peek()
                .is_some_and(|entry| size > entry.entry.size_bytes)
    }

    fn insert(&mut self, entry: LargeFileEntry) {
        if !self.accepts(entry.size_bytes) {
            return;
        }
        let ranked = RankedFile {
            entry,
            order: self.sequence,
        };
        self.sequence += 1;
        self.push_ranked(ranked);
    }

    fn push_ranked(&mut self, ranked: RankedFile) {
        if self.heap.len() < MAX_LARGEST_FILES {
            self.heap.push(ranked);
        } else if self.heap.peek().is_some_and(|last| ranked < *last) {
            *self.heap.peek_mut().unwrap() = ranked;
        }
    }

    fn merge(&mut self, other: Self) {
        let offset = self.sequence;
        self.sequence += other.sequence;
        for mut ranked in other.heap {
            ranked.order += offset;
            self.push_ranked(ranked);
        }
    }

    fn into_ranked_entries(self) -> Vec<LargeFileEntry> {
        self.heap
            .into_sorted_vec()
            .into_iter()
            .map(|ranked| ranked.entry)
            .collect()
    }
}

struct RawNode {
    name: String,
    path: String,
    is_dir: bool,
    size_bytes: u64,
    file_count: usize,
    dir_count: usize,
    ext: Option<String>,
    children: Vec<RawNode>,
    omitted_children: OmittedChildren,
}

#[derive(Default)]
struct OmittedChildren {
    size_bytes: u64,
    file_count: usize,
    dir_count: usize,
    count: usize,
}

impl OmittedChildren {
    fn add(&mut self, node: &RawNode) {
        if node.size_bytes != 0 || node.is_dir {
            self.size_bytes += node.size_bytes;
            self.file_count += node.file_count;
            self.dir_count += node.dir_count + usize::from(node.is_dir);
            self.count += 1;
        }
    }
}

pub fn get_presets(sys: &SystemInfo) -> Vec<DiskPreset> {
    let mut presets = Vec::new();
    let var_candidates = [
        (
            "%LOCALAPPDATA%",
            "本地应用数据 (AppData\\Local)",
            "AppData\\Local",
        ),
        (
            "%APPDATA%",
            "漫游应用数据 (AppData\\Roaming)",
            "AppData\\Roaming",
        ),
        ("%USERPROFILE%", "用户主目录", "User Profile"),
        ("%ProgramData%", "程序数据 (ProgramData)", "ProgramData"),
        (
            "%SystemRoot%\\SystemTemp",
            "系统临时目录 (SystemTemp)",
            "Windows SystemTemp",
        ),
    ];
    for (env_expr, zh, en) in var_candidates {
        let key = env_expr.trim_matches('%');
        let resolved = if env_expr.contains('\\') {
            windows_dir_or_default()
                .join("SystemTemp")
                .to_string_lossy()
                .into_owned()
        } else {
            std::env::var(key).unwrap_or_default()
        };
        if !resolved.is_empty() && Path::new(&resolved).exists() {
            presets.push(DiskPreset {
                label: LocalizedText {
                    zh: zh.to_string(),
                    en: en.to_string(),
                },
                path: resolved,
            });
        }
    }
    for drv in &sys.fixed_drives {
        presets.push(DiskPreset {
            label: LocalizedText {
                zh: format!("本地磁盘 ({})", drv.trim_end_matches('\\')),
                en: format!("Local Disk ({})", drv.trim_end_matches('\\')),
            },
            path: drv.clone(),
        });
    }
    presets
}

/// Performs a parallel directory tree & extension analysis without following NTFS reparse points.
pub fn analyze_path(root: &Path, max_depth: usize) -> Result<DiskAnalysisReport> {
    analyze_path_with_options(root, max_depth, false, None)
}

pub fn analyze_path_with_progress(
    root: &Path,
    max_depth: usize,
    progress: &ScanProgress,
) -> Result<DiskAnalysisReport> {
    analyze_path_with_options(root, max_depth, false, Some(progress))
}

fn check_scan(progress: Option<&ScanProgress>) -> Result<()> {
    progress.map_or(Ok(()), ScanProgress::check)
}

/// Optionally measures NTFS file data allocation without changing logical-size rankings.
pub fn analyze_path_with_options(
    root: &Path,
    max_depth: usize,
    measure_allocation: bool,
    progress: Option<&ScanProgress>,
) -> Result<DiskAnalysisReport> {
    check_scan(progress)?;
    if !root.exists() {
        return Err(anyhow!("Path does not exist: {}", root.display()));
    }
    let started = Instant::now();
    let scan_started_at = scan_timestamp();
    let depth_limit = max_depth.clamp(1, 6);

    let allocation = measure_allocation.then(|| AllocationScan::for_path(root));
    let tracker = allocation.as_ref().and_then(|result| result.as_ref().ok());
    let acc = scan_recursive(root, Some(depth_limit), progress, tracker)?;
    if let Some(progress) = progress {
        progress.set_phase(ScanPhase::Finishing, None);
    }
    let total_bytes = acc.node.size_bytes;
    let total_files = acc.node.file_count;
    let total_dirs = acc.node.dir_count;

    let tree = into_disk_tree(acc.node);

    let mut ext_vec: Vec<(String, u64, usize)> = acc
        .exts
        .into_iter()
        .map(|(k, (sz, cnt))| (k, sz, cnt))
        .collect();
    ext_vec.sort_by_key(|entry| std::cmp::Reverse(entry.1));

    let mut extensions = Vec::new();
    let mut other_bytes = 0u64;
    let mut other_count = 0usize;
    for (i, (ext, sz, cnt)) in ext_vec.into_iter().enumerate() {
        if i < MAX_EXTENSIONS {
            let ext = extension_label(&ext);
            let description = describe_extension(&ext);
            extensions.push(ExtensionStat {
                ext,
                description,
                size_bytes: sz,
                file_count: cnt,
                color: PALETTE[i % PALETTE.len()].to_string(),
            });
        } else {
            other_bytes += sz;
            other_count += cnt;
        }
    }
    if other_count > 0 {
        extensions.push(ExtensionStat {
            ext: "(other)".to_string(),
            description: LocalizedText {
                zh: "其他扩展名汇总".to_string(),
                en: "Other Extensions".to_string(),
            },
            size_bytes: other_bytes,
            file_count: other_count,
            color: "#8A8886".to_string(),
        });
    }

    check_scan(progress)?;
    Ok(DiskAnalysisReport {
        root_path: root.to_string_lossy().into_owned(),
        scan_started_at,
        elapsed_ms: started.elapsed().as_millis() as u64,
        total_bytes,
        allocation: allocation.map(|result| match result {
            Ok(scan) => scan.summary(),
            Err(status) => status,
        }),
        total_files,
        total_dirs,
        tree,
        extensions,
        largest_files: acc.top_files.into_ranked_entries(),
    })
}

fn scan_recursive(
    path: &Path,
    visible_depth: Option<usize>,
    progress: Option<&ScanProgress>,
    allocation: Option<&AllocationScan>,
) -> Result<ScanAccumulator> {
    check_scan(progress)?;
    let (name, path_str) = if visible_depth.is_some() {
        let path_str = path.to_string_lossy().into_owned();
        let name = path
            .file_name()
            .filter(|name| !name.is_empty())
            .map_or_else(
                || path_str.clone(),
                |name| name.to_string_lossy().into_owned(),
            );
        (name, path_str)
    } else {
        (String::new(), String::new())
    };
    let child_depth = visible_depth.and_then(|depth| depth.checked_sub(1));

    let Ok(meta) = std::fs::symlink_metadata(path) else {
        if let Some(scan) = allocation {
            scan.record_unreadable();
        }
        return Ok(empty_acc(name, path_str, false));
    };

    if (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
        return Ok(empty_acc(name, path_str, meta.is_dir()));
    }

    if meta.is_file() {
        if let Some(progress) = progress {
            progress.file_processed();
        }
        let size = meta.len();
        let ext = normalized_extension(path);
        let mut exts = HashMap::new();
        let label = extension_label(&ext);
        exts.insert(ext.into_owned(), (size, 1));
        let mut top_files = LargestFiles::default();
        top_files.insert(LargeFileEntry {
            path: path_str.clone(),
            name: name.clone(),
            ext: label.clone(),
            size_bytes: size,
            allocated_bytes: allocation
                .and_then(|scan| scan.record_file(path, meta.file_attributes())),
        });
        return Ok(ScanAccumulator {
            node: RawNode {
                name,
                path: path_str,
                is_dir: false,
                size_bytes: size,
                file_count: 1,
                dir_count: 0,
                ext: Some(label),
                children: Vec::new(),
                omitted_children: OmittedChildren::default(),
            },
            exts,
            top_files,
        });
    }

    let Ok(rd) = std::fs::read_dir(path) else {
        if let Some(scan) = allocation {
            scan.record_unreadable();
        }
        return Ok(empty_acc(name, path_str, true));
    };

    let mut total_bytes = 0u64;
    let mut total_files = 0usize;
    let mut total_dirs = 0usize;
    let mut children: Vec<RawNode> = Vec::new();
    let mut omitted_children = OmittedChildren::default();
    let mut exts: HashMap<String, (u64, usize)> = HashMap::new();
    let mut top_files = LargestFiles::default();
    let mut visible_cutoff = None;
    let mut sub_dirs: Vec<PathBuf> = Vec::new();

    for entry in rd {
        check_scan(progress)?;
        let Ok((entry, c_meta)) = entry.and_then(|entry| {
            let metadata = entry.metadata()?;
            Ok((entry, metadata))
        }) else {
            if let Some(scan) = allocation {
                scan.record_unreadable();
            }
            continue;
        };
        if (c_meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
            continue;
        }
        if c_meta.is_file() {
            if let Some(progress) = progress {
                progress.file_processed();
            }
            let size = c_meta.len();
            let file_name = entry.file_name();
            let ext = normalized_extension(Path::new(&file_name));

            total_bytes = total_bytes.saturating_add(size);
            total_files += 1;
            add_extension(&mut exts, &ext, size, 1);

            let largest = top_files.accepts(size);
            let visible =
                child_depth.is_some() && visible_cutoff.is_none_or(|cutoff| size > cutoff);
            if child_depth.is_some() && !visible && size != 0 {
                omitted_children.size_bytes = omitted_children.size_bytes.saturating_add(size);
                omitted_children.file_count += 1;
                omitted_children.count += 1;
            }
            if !(largest || visible || allocation.is_some()) {
                continue;
            }
            let child_path = entry.path();
            let allocated_bytes =
                allocation.and_then(|scan| scan.record_file(&child_path, c_meta.file_attributes()));
            if !(largest || visible) {
                continue;
            }
            let c_name = file_name.to_string_lossy().into_owned();
            let c_path_str = child_path.to_string_lossy().into_owned();
            let ext = extension_label(&ext);
            if visible {
                children.push(RawNode {
                    name: c_name.clone(),
                    path: c_path_str.clone(),
                    is_dir: false,
                    size_bytes: size,
                    file_count: 1,
                    dir_count: 0,
                    ext: Some(ext.clone()),
                    children: Vec::new(),
                    omitted_children: OmittedChildren::default(),
                });
                if children.len() == MAX_CHILDREN_PER_NODE * 2 {
                    trim_children(&mut children, &mut omitted_children);
                    visible_cutoff = children.last().map(|node| node.size_bytes);
                }
            }
            if largest {
                top_files.insert(LargeFileEntry {
                    path: c_path_str,
                    name: c_name,
                    ext,
                    size_bytes: size,
                    allocated_bytes,
                });
            }
        } else if c_meta.is_dir() {
            sub_dirs.push(entry.path());
        }
    }

    // Merge each batch before starting more subtrees, so completed branches can be dropped early.
    for batch in sub_dirs.chunks(32) {
        check_scan(progress)?;
        let child_accs = batch
            .par_iter()
            .map(|path| scan_recursive(path, child_depth, progress, allocation))
            .collect::<Result<Vec<_>>>()?;
        for c in child_accs {
            total_bytes = total_bytes.saturating_add(c.node.size_bytes);
            total_files += c.node.file_count;
            total_dirs += c.node.dir_count + usize::from(c.node.is_dir);
            if child_depth.is_some() {
                children.push(c.node);
                if children.len() == MAX_CHILDREN_PER_NODE * 2 {
                    trim_children(&mut children, &mut omitted_children);
                }
            }
            for (ext, (size, count)) in c.exts {
                add_extension(&mut exts, &ext, size, count);
            }
            top_files.merge(c.top_files);
        }
    }

    trim_children(&mut children, &mut omitted_children);

    Ok(ScanAccumulator {
        node: RawNode {
            name,
            path: path_str,
            is_dir: true,
            size_bytes: total_bytes,
            file_count: total_files,
            dir_count: total_dirs,
            ext: None,
            children,
            omitted_children,
        },
        exts,
        top_files,
    })
}

fn trim_children(children: &mut Vec<RawNode>, omitted: &mut OmittedChildren) {
    children.sort_by_key(|node| Reverse(node.size_bytes));
    if children.len() > MAX_CHILDREN_PER_NODE {
        for node in children.drain(MAX_CHILDREN_PER_NODE..) {
            omitted.add(&node);
        }
    }
}

fn empty_acc(name: String, path: String, is_dir: bool) -> ScanAccumulator {
    ScanAccumulator {
        node: RawNode {
            name,
            path,
            is_dir,
            size_bytes: 0,
            file_count: 0,
            dir_count: 0,
            ext: None,
            children: Vec::new(),
            omitted_children: OmittedChildren::default(),
        },
        exts: HashMap::new(),
        top_files: LargestFiles::default(),
    }
}

fn normalized_extension(path: &Path) -> Cow<'_, str> {
    let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
    if ext.bytes().any(|byte| byte.is_ascii_uppercase()) {
        Cow::Owned(ext.to_ascii_lowercase())
    } else {
        Cow::Borrowed(ext)
    }
}

fn extension_label(ext: &str) -> String {
    if ext.is_empty() {
        "(no ext)".to_string()
    } else {
        format!(".{ext}")
    }
}

fn add_extension(exts: &mut HashMap<String, (u64, usize)>, ext: &str, size: u64, count: usize) {
    if let Some(total) = exts.get_mut(ext) {
        total.0 = total.0.saturating_add(size);
        total.1 += count;
    } else {
        exts.insert(ext.to_string(), (size, count));
    }
}

fn is_cache_folder_name(name: &str, path: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let p = path.to_ascii_lowercase();
    if n == "cache"
        || n == "code cache"
        || n == "gpucache"
        || n == "dxcache"
        || n == "glcache"
        || n == "vkcache"
        || n == "shadercache"
        || n == "temp"
        || n == "systemtemp"
        || n == "squirreltemp"
        || n == "crashdumps"
        || n == "crashpad"
        || n == "npm-cache"
        || n == "pnpm-cache"
        || n == "v3-cache"
        || n.ends_with("-updater")
    {
        return true;
    }
    p.ends_with(r"\uv\cache") || p.ends_with(r"\pip\cache") || p.ends_with(r"\package cache")
}

fn describe_extension(ext: &str) -> LocalizedText {
    let (zh, en) = match ext {
        "(no ext)" => ("无扩展名", "No Extension"),
        ".dll" => ("应用程序扩展", "Application Extension"),
        ".exe" => ("应用程序", "Executable Application"),
        ".sys" => ("系统驱动程序", "System Driver"),
        ".msi" => ("Windows 安装程序包", "Windows Installer Package"),
        ".msp" => ("Windows 安装补丁", "Windows Installer Patch"),
        ".msix" | ".appx" => ("现代应用安装包", "Windows App Package"),
        ".vsix" => ("Visual Studio 扩展包", "Visual Studio Extension"),
        ".cab" => ("Windows 压缩包", "Cabinet Archive"),
        ".vhdx" | ".vhd" => ("虚拟硬盘映像", "Virtual Hard Disk Image"),
        ".wim" | ".esd" => ("Windows 系统映像", "Windows Imaging Format"),
        ".iso" => ("光盘镜像文件", "ISO Disc Image"),
        ".zip" => ("ZIP 压缩归档", "ZIP Archive"),
        ".7z" => ("7-Zip 压缩归档", "7-Zip Archive"),
        ".rar" => ("RAR 压缩归档", "RAR Archive"),
        ".zst" => ("ZSTD 压缩文件", "Zstandard Compressed File"),
        ".gz" | ".tgz" | ".tar" => ("Tar / GZip 归档", "Tar / GZip Archive"),
        ".pak" => ("打包资源文件", "Packed Resource File"),
        ".asar" => ("桌面应用打包归档", "ASAR Application Archive"),
        ".bin" => ("二进制数据文件", "Binary Data File"),
        ".dat" => ("数据存储文件", "Data File"),
        ".db" | ".sqlite" | ".mdb" => ("数据库文件", "Database File"),
        ".etl" => ("系统事件跟踪日志", "Event Trace Log"),
        ".evtx" => ("Windows 事件日志", "Windows Event Log"),
        ".log" => ("文本日志文件", "Log File"),
        ".dmp" | ".mdmp" => ("崩溃内存转储", "Crash Memory Dump"),
        ".tmp" => ("临时文件", "Temporary File"),
        ".bak" => ("备份文件", "Backup File"),
        ".pdb" => ("调试符号数据库", "Program Debug Database"),
        ".lib" => ("静态链接库", "Static Library"),
        ".obj" | ".o" => ("编译目标文件", "Compiled Object File"),
        ".rlib" => ("Rust 编译库", "Rust Compiled Library"),
        ".node" => ("Node.js 原生扩展", "Node.js Native Addon"),
        ".js" | ".mjs" | ".cjs" => ("JavaScript 脚本", "JavaScript Source"),
        ".ts" | ".tsx" => ("TypeScript 源码", "TypeScript Source"),
        ".jsx" => ("React JSX 源码", "React JSX Source"),
        ".json" => ("JSON 配置文件", "JSON Data File"),
        ".xml" | ".xaml" => ("XML 结构化文件", "XML Document"),
        ".yaml" | ".yml" | ".toml" | ".ini" => ("配置清单文件", "Configuration File"),
        ".html" | ".htm" | ".css" | ".scss" => ("网页与样式文件", "Web Document / Stylesheet"),
        ".wasm" => ("WebAssembly 模块", "WebAssembly Module"),
        ".py" | ".pyc" | ".whl" => ("Python 脚本 / Wheel 包", "Python Script / Wheel"),
        ".rs" | ".c" | ".cpp" | ".h" | ".hpp" | ".cs" | ".go" | ".java" => {
            ("编程语言源码", "Source Code")
        }
        ".jar" => ("Java 归档包", "Java Archive"),
        ".ttf" | ".otf" | ".woff" | ".woff2" => ("字体文件", "Font File"),
        ".png" | ".jpg" | ".jpeg" | ".webp" | ".gif" | ".bmp" | ".ico" | ".svg" => {
            ("图像 / 图标资源", "Image / Icon")
        }
        ".mp4" | ".mkv" | ".avi" | ".mov" | ".webm" => ("视频媒体文件", "Video Media"),
        ".mp3" | ".wav" | ".flac" | ".ogg" => ("音频媒体文件", "Audio Media"),
        ".pdf" => ("PDF 文档", "PDF Document"),
        ".docx" | ".xlsx" | ".pptx" => ("Office 文档", "Office Document"),
        ".txt" | ".md" => ("纯文本 / Markdown", "Text / Markdown"),
        ".onnx" | ".safetensors" | ".pt" | ".gguf" => ("AI 模型权重文件", "AI Model Weights"),
        ".nvph" => ("显卡着色器缓存", "GPU Shader Cache"),
        ".pri" => ("Windows 资源索引", "Windows Resource Index"),
        ".mui" => ("多语言界面资源", "Multilingual UI Resource"),
        ".so" => ("共享动态库", "Shared Object Library"),
        _ => {
            let clean = ext.trim_start_matches('.').to_ascii_uppercase();
            return LocalizedText {
                zh: format!("{} 文件", clean),
                en: format!("{} File", clean),
            };
        }
    };
    LocalizedText {
        zh: zh.to_string(),
        en: en.to_string(),
    }
}

fn into_disk_tree(raw: RawNode) -> DiskTreeNode {
    let cache_hint = raw.is_dir && is_cache_folder_name(&raw.name, &raw.path);
    let mut children: Vec<_> = raw
        .children
        .into_iter()
        .filter(|node| node.size_bytes != 0 || node.is_dir)
        .map(into_disk_tree)
        .collect();
    let omitted = raw.omitted_children;
    if omitted.size_bytes > 0 {
        children.push(DiskTreeNode {
            name: format!("[+{} smaller items]", omitted.count),
            path: raw.path.clone(),
            is_dir: false,
            is_aggregate: true,
            is_cache_hint: false,
            aggregate_count: omitted.count,
            size_bytes: omitted.size_bytes,
            file_count: omitted.file_count,
            dir_count: omitted.dir_count,
            ext: Some("(other)".to_string()),
            children: Vec::new(),
        });
    }

    DiskTreeNode {
        name: raw.name,
        path: raw.path,
        is_dir: raw.is_dir,
        is_aggregate: false,
        is_cache_hint: cache_hint,
        aggregate_count: 0,
        size_bytes: raw.size_bytes,
        file_count: raw.file_count,
        dir_count: raw.dir_count,
        ext: raw.ext,
        children,
    }
}

/// Verifies and moves a file or directory into the Windows Recycle Bin, returning its logical size.
pub fn recycle_path(target: &Path) -> Result<u64> {
    recycle_path_immediately(target)
}

#[cfg(test)]
mod tests {
    use super::duplicates::full_hash;
    use super::orphans::measure_dir_stats_up_to;
    use super::*;
    use std::fs::{self, File};
    use std::io::{Read, Write};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn full_hash_checks_for_cancellation_before_reading_the_next_chunk() {
        struct CancelAfterChunk<'a> {
            progress: &'a ScanProgress,
            reads: usize,
        }
        impl Read for CancelAfterChunk<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.reads += 1;
                buffer.fill(1);
                self.progress.cancel();
                Ok(buffer.len())
            }
        }
        let progress = ScanProgress::default();
        let mut reader = CancelAfterChunk {
            progress: &progress,
            reads: 0,
        };
        assert!(full_hash(&mut reader, Some(&progress)).is_none());
        assert_eq!(reader.reads, 1);
        let snapshot = serde_json::to_value(progress.snapshot()).unwrap();
        assert_eq!(snapshot["bytes_read"], 65536);
    }

    #[test]
    fn test_diskmap_aggregation_extensions_and_pruning() {
        let base =
            std::env::temp_dir().join(format!("dismnext_diskmap_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let sub_a = base.join("Cache");
        let sub_b = base.join("beta");
        fs::create_dir_all(&sub_a).unwrap();
        fs::create_dir_all(&sub_b).unwrap();

        File::create(sub_a.join("big.bin"))
            .unwrap()
            .write_all(&vec![b'A'; 4096])
            .unwrap();
        File::create(sub_b.join("mid.log"))
            .unwrap()
            .write_all(&vec![b'B'; 1024])
            .unwrap();
        File::create(base.join("README"))
            .unwrap()
            .write_all(&vec![b'C'; 256])
            .unwrap();

        let report = analyze_path(&base, 2).unwrap();
        assert_eq!(report.total_bytes, 4096 + 1024 + 256);
        assert_eq!(report.total_files, 3);
        assert_eq!(report.total_dirs, 2);
        assert_eq!(report.tree.children[0].name, "Cache");
        assert!(report.tree.children[0].is_cache_hint);
        assert_eq!(report.extensions[0].ext, ".bin");
        assert_eq!(report.extensions[0].description.en, "Binary Data File");
        assert_eq!(report.extensions[0].size_bytes, 4096);
        assert_eq!(report.largest_files[0].name, "big.bin");
        assert!(report.extensions.iter().any(|e| e.ext == "(no ext)"));

        let progress = ScanProgress::default();
        let tracked = analyze_path_with_progress(&base, 2, &progress).unwrap();
        let snapshot = serde_json::to_value(progress.snapshot()).unwrap();
        assert_eq!(tracked.total_bytes, report.total_bytes);
        assert_eq!(snapshot["processed_files"], 3);
        assert_eq!(snapshot["phase"], "finishing");
        progress.cancel();
        assert!(analyze_path_with_progress(&base, 2, &progress)
            .unwrap_err()
            .is::<crate::scan_progress::ScanCancelled>());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn ntfs_allocation_counts_hard_links_once_across_directories() {
        let root = std::env::temp_dir().join(format!("dismnext_allocation_{}", std::process::id()));
        let first_dir = root.join("first");
        let second_dir = root.join("second");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&first_dir).unwrap();
        fs::create_dir(&second_dir).unwrap();
        let original = first_dir.join("original.bin");
        let alias = second_dir.join("alias.bin");
        fs::write(&original, vec![0x89; 8192]).unwrap();
        fs::hard_link(&original, &alias).unwrap();

        let logical = analyze_path(&root, 1).unwrap();
        assert!(logical.allocation.is_none());
        assert!(serde_json::to_value(&logical)
            .unwrap()
            .get("allocation")
            .is_none());
        let measured = analyze_path_with_options(&root, 1, true, None).unwrap();
        assert_eq!(measured.total_bytes, 16_384);
        assert_eq!(measured.total_bytes, logical.total_bytes);
        assert_eq!(measured.total_files, 2);
        let DiskAllocation::Measured {
            allocated_bytes,
            hard_link_aliases,
            unmeasured_files,
            unreadable_entries,
        } = measured.allocation.unwrap()
        else {
            panic!("Expected NTFS allocation data from the test volume");
        };
        assert!(allocated_bytes > 0);
        assert_eq!(hard_link_aliases, 1);
        assert_eq!(unmeasured_files, 0);
        assert_eq!(unreadable_entries, 0);
        assert!(measured
            .largest_files
            .iter()
            .all(|file| file.allocated_bytes == Some(allocated_bytes)));
        assert_eq!(measured.largest_files.len(), 2);
        fs::remove_file(alias).unwrap();
        fs::remove_file(original).unwrap();
        fs::remove_dir(second_dir).unwrap();
        fs::remove_dir(first_dir).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn diskmap_limits_visible_nodes_without_losing_deep_totals() {
        let base = std::env::temp_dir().join(format!("dismnext_wide_tree_{}", std::process::id()));
        let deep = base.join("nested").join("deeper");
        fs::create_dir_all(&deep).unwrap();
        for size in 1..=140 {
            fs::write(base.join(format!("{size}.txt")), vec![0; size]).unwrap();
        }
        fs::write(deep.join("largest.bin"), vec![0; 8192]).unwrap();

        let report = analyze_path(&base, 1).unwrap();
        fs::remove_dir_all(&base).unwrap();

        assert_eq!(report.total_files, 141);
        assert_eq!(report.total_dirs, 2);
        assert_eq!(report.total_bytes, (1..=140).sum::<u64>() + 8192);
        assert_eq!(report.tree.children.len(), MAX_CHILDREN_PER_NODE + 1);
        let nested = &report.tree.children[0];
        assert_eq!(nested.name, "nested");
        assert_eq!(nested.size_bytes, 8192);
        assert!(nested.children.is_empty());
        let omitted = report.tree.children.last().unwrap();
        assert!(omitted.is_aggregate);
        assert_eq!(omitted.aggregate_count, 81);
        assert_eq!(omitted.size_bytes, (1..=81).sum::<u64>());
        assert_eq!(report.largest_files.len(), MAX_LARGEST_FILES);
        assert_eq!(report.largest_files[0].name, "largest.bin");
        assert_eq!(report.largest_files.last().unwrap().size_bytes, 42);
        assert!(report
            .extensions
            .iter()
            .any(|ext| ext.ext == ".bin" && ext.size_bytes == 8192));
    }

    #[test]
    fn test_protected_recycle_targets_and_sandbox_recycle() {
        assert!(is_protected_recycle_target(Path::new(r"C:\")));
        assert!(is_protected_recycle_target(Path::new(r"C:\Windows")));
        assert!(is_protected_recycle_target(Path::new(
            r"C:\Windows\System32"
        )));
        assert!(is_protected_recycle_target(Path::new(
            r"C:\Windows\System32\drivers\etc\hosts"
        )));
        assert!(is_protected_recycle_target(Path::new(
            r"C:\Recovery\sample"
        )));
        assert!(!is_protected_recycle_target(Path::new(
            r"C:\Windows\System32-sample"
        )));
        assert!(is_protected_recycle_target(Path::new(r"C:\Program Files")));

        let sample =
            std::env::temp_dir().join(format!("dismnext_recycle_test_{}.tmp", std::process::id()));
        File::create(&sample)
            .unwrap()
            .write_all(b"recycle_me")
            .unwrap();
        assert!(!is_protected_recycle_target(&sample));
        let freed = recycle_path(&sample).unwrap();
        assert_eq!(freed, 10);
        assert!(!sample.exists());
    }

    #[test]
    fn test_find_duplicate_files_sandbox() {
        let base = std::env::temp_dir().join(format!("dismnext_dups_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("a")).unwrap();
        fs::create_dir_all(base.join("b")).unwrap();

        let same_payload = vec![0x5Au8; 70 * 1024];
        let diff_payload = vec![0x7Bu8; 70 * 1024];
        File::create(base.join("a\\dup1.bin"))
            .unwrap()
            .write_all(&same_payload)
            .unwrap();
        File::create(base.join("b\\dup2.bin"))
            .unwrap()
            .write_all(&same_payload)
            .unwrap();
        File::create(base.join("b\\unique.bin"))
            .unwrap()
            .write_all(&diff_payload)
            .unwrap();

        let same_second = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        for (name, milliseconds) in [("a\\dup1.bin", 100), ("b\\dup2.bin", 900)] {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(base.join(name))
                .unwrap();
            file.set_times(
                std::fs::FileTimes::new()
                    .set_modified(same_second + Duration::from_millis(milliseconds)),
            )
            .unwrap();
        }

        let report = find_duplicate_files(&base, 64 * 1024).unwrap();
        assert_eq!(report.total_groups, 1);
        assert_eq!(report.total_wasted_bytes, 70 * 1024);
        assert_eq!(report.groups[0].files.len(), 2);
        assert_eq!(report.groups[0].files[0].name, "dup2.bin");
        assert_eq!(
            report.groups[0].hash_hex,
            blake3::hash(&same_payload).to_hex().to_string()
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn hard_link_aliases_are_removed_before_reading_duplicate_content() {
        let root =
            std::env::temp_dir().join(format!("dismnext_hardlink_dups_{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let size = 72 * 1024;
        let bytes = vec![0x42; size];
        let original = root.join("a.bin");
        let alias = root.join("z-link.bin");
        let copy = root.join("b.bin");
        fs::write(&original, &bytes).unwrap();
        fs::hard_link(&original, &alias).unwrap();
        fs::write(&copy, &bytes).unwrap();
        let progress = ScanProgress::default();
        let report = find_duplicate_files_with_progress(&root, 64 * 1024, &progress).unwrap();
        let counters = serde_json::to_value(progress.snapshot()).unwrap();
        assert_eq!(counters["bytes_read"], 2 * (size + 2 * 4096));
        assert_eq!(report.total_wasted_bytes, size as u64);
        assert_eq!(report.groups[0].files.len(), 2);
        assert!(report.groups[0]
            .files
            .iter()
            .all(|file| file.name != "z-link.bin"));
        for path in [alias, original, copy] {
            fs::remove_file(path).unwrap();
        }
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn largest_files_heap_preserves_rank_and_ties_across_subtrees() {
        let file = |size_bytes, name: String| LargeFileEntry {
            path: name.clone(),
            name,
            ext: "(no ext)".into(),
            size_bytes,
            allocated_bytes: None,
        };
        let mut parent = LargestFiles::default();
        let mut child = LargestFiles::default();
        for index in 0..120 {
            parent.insert(file(10, format!("original-{index}")));
            child.insert(file(10, format!("later-{index}")));
        }
        child.insert(file(20, "largest".into()));
        parent.merge(child);
        let entries = parent.into_ranked_entries();
        assert_eq!(entries.len(), MAX_LARGEST_FILES);
        assert_eq!(entries[0].name, "largest");
        assert_eq!(entries[1].name, "original-0");
        assert_eq!(entries.last().unwrap().name, "original-98");
    }

    #[test]
    fn limited_directory_statistics_are_marked_as_incomplete() {
        let dir =
            std::env::temp_dir().join(format!("dismnext-limited-stats-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        for index in 0..3 {
            std::fs::write(dir.join(index.to_string()), b"1234").unwrap();
        }
        let limited = measure_dir_stats_up_to(&dir, 2);
        let full = measure_dir_stats(&dir);
        std::fs::remove_dir_all(dir).unwrap();
        assert!(limited.incomplete);
        assert_eq!((limited.size_bytes, limited.file_count), (8, 2));
        assert!(!full.incomplete);
        assert_eq!((full.size_bytes, full.file_count), (12, 3));
    }
}
