use crate::catalog::LocalizedText;
use crate::sysinfo::SystemInfo;
use anyhow::{anyhow, Result};
use chrono::DateTime;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant, UNIX_EPOCH};
use walkdir::WalkDir;
use winreg::enums::{
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
};
use winreg::RegKey;
use windows_sys::Win32::UI::Shell::{
    SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE,
    SHFILEOPSTRUCTW,
};

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskPreset {
    pub label: LocalizedText,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskAnalysisReport {
    pub root_path: String,
    pub elapsed_ms: u64,
    pub total_bytes: u64,
    pub total_files: usize,
    pub total_dirs: usize,
    pub tree: DiskTreeNode,
    pub extensions: Vec<ExtensionStat>,
    pub largest_files: Vec<LargeFileEntry>,
}

struct ScanAccumulator {
    node: RawNode,
    exts: HashMap<String, (u64, usize)>,
    top_files: Vec<LargeFileEntry>,
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
}

pub fn get_presets(sys: &SystemInfo) -> Vec<DiskPreset> {
    let mut presets = Vec::new();
    let var_candidates = [
        ("%LOCALAPPDATA%", "本地应用数据 (AppData\\Local)", "AppData\\Local"),
        ("%APPDATA%", "漫游应用数据 (AppData\\Roaming)", "AppData\\Roaming"),
        ("%USERPROFILE%", "用户主目录", "User Profile"),
        ("%ProgramData%", "程序数据 (ProgramData)", "ProgramData"),
        ("%SystemRoot%\\SystemTemp", "系统临时目录 (SystemTemp)", "Windows SystemTemp"),
    ];
    for (env_expr, zh, en) in var_candidates {
        let key = env_expr.trim_matches('%');
        let resolved = if env_expr.contains('\\') {
            let sys_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
            format!(r"{}\SystemTemp", sys_root)
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
    if !root.exists() {
        return Err(anyhow!("Path does not exist: {}", root.display()));
    }
    let started = Instant::now();
    let depth_limit = max_depth.clamp(1, 6);

    let mut acc = scan_recursive(root);
    let total_bytes = acc.node.size_bytes;
    let total_files = acc.node.file_count;
    let total_dirs = acc.node.dir_count;

    let tree = prune_node(acc.node, 0, depth_limit);

    let mut ext_vec: Vec<(String, u64, usize)> = acc
        .exts
        .into_iter()
        .map(|(k, (sz, cnt))| (k, sz, cnt))
        .collect();
    ext_vec.sort_by(|a, b| b.1.cmp(&a.1));

    let mut extensions = Vec::new();
    let mut other_bytes = 0u64;
    let mut other_count = 0usize;
    for (i, (ext, sz, cnt)) in ext_vec.into_iter().enumerate() {
        if i < MAX_EXTENSIONS {
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

    acc.top_files.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    acc.top_files.truncate(MAX_LARGEST_FILES);

    Ok(DiskAnalysisReport {
        root_path: root.to_string_lossy().into_owned(),
        elapsed_ms: started.elapsed().as_millis() as u64,
        total_bytes,
        total_files,
        total_dirs,
        tree,
        extensions,
        largest_files: acc.top_files,
    })
}

fn scan_recursive(path: &Path) -> ScanAccumulator {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let path_str = path.to_string_lossy().into_owned();

    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return empty_acc(name, path_str, false);
    };

    if (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
        return empty_acc(name, path_str, meta.is_dir());
    }

    if meta.is_file() {
        let size = meta.len();
        let ext = extract_ext(path);
        let mut exts = HashMap::new();
        exts.insert(ext.clone(), (size, 1));
        let top_files = vec![LargeFileEntry {
            path: path_str.clone(),
            name: name.clone(),
            ext: ext.clone(),
            size_bytes: size,
        }];
        return ScanAccumulator {
            node: RawNode {
                name,
                path: path_str,
                is_dir: false,
                size_bytes: size,
                file_count: 1,
                dir_count: 0,
                ext: Some(ext),
                children: Vec::new(),
            },
            exts,
            top_files,
        };
    }

    let Ok(rd) = std::fs::read_dir(path) else {
        return empty_acc(name, path_str, true);
    };

    let mut total_bytes = 0u64;
    let mut total_files = 0usize;
    let mut total_dirs = 0usize;
    let mut children: Vec<RawNode> = Vec::new();
    let mut exts: HashMap<String, (u64, usize)> = HashMap::new();
    let mut top_files: Vec<LargeFileEntry> = Vec::new();
    let mut sub_dirs: Vec<PathBuf> = Vec::new();

    for entry in rd.flatten() {
        let Ok(c_meta) = entry.metadata() else {
            continue;
        };
        if (c_meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
            continue;
        }
        let child_path = entry.path();
        if c_meta.is_file() {
            let sz = c_meta.len();
            let c_name = entry.file_name().to_string_lossy().into_owned();
            let c_path_str = child_path.to_string_lossy().into_owned();
            let ext = extract_ext(&child_path);

            total_bytes = total_bytes.saturating_add(sz);
            total_files += 1;

            let ent = exts.entry(ext.clone()).or_insert((0, 0));
            ent.0 = ent.0.saturating_add(sz);
            ent.1 += 1;

            top_files.push(LargeFileEntry {
                path: c_path_str.clone(),
                name: c_name.clone(),
                ext: ext.clone(),
                size_bytes: sz,
            });
            children.push(RawNode {
                name: c_name,
                path: c_path_str,
                is_dir: false,
                size_bytes: sz,
                file_count: 1,
                dir_count: 0,
                ext: Some(ext),
                children: Vec::new(),
            });
        } else if c_meta.is_dir() {
            sub_dirs.push(child_path);
        }
    }

    let child_accs: Vec<ScanAccumulator> = if sub_dirs.len() >= 2 {
        sub_dirs.par_iter().map(|p| scan_recursive(p)).collect()
    } else {
        sub_dirs.iter().map(|p| scan_recursive(p)).collect()
    };

    for c in child_accs {
        total_bytes = total_bytes.saturating_add(c.node.size_bytes);
        total_files += c.node.file_count;
        total_dirs += c.node.dir_count + usize::from(c.node.is_dir);
        children.push(c.node);
        for (ext, (sz, cnt)) in c.exts {
            let entry = exts.entry(ext).or_insert((0, 0));
            entry.0 = entry.0.saturating_add(sz);
            entry.1 += cnt;
        }
        top_files.extend(c.top_files);
    }

    if top_files.len() > MAX_LARGEST_FILES * 2 {
        top_files.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
        top_files.truncate(MAX_LARGEST_FILES);
    }

    ScanAccumulator {
        node: RawNode {
            name,
            path: path_str,
            is_dir: true,
            size_bytes: total_bytes,
            file_count: total_files,
            dir_count: total_dirs,
            ext: None,
            children,
        },
        exts,
        top_files,
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
        },
        exts: HashMap::new(),
        top_files: Vec::new(),
    }
}

fn extract_ext(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map(|s| format!(".{}", s.to_ascii_lowercase()))
        .unwrap_or_else(|| "(no ext)".to_string())
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
        ".rs" | ".c" | ".cpp" | ".h" | ".hpp" | ".cs" | ".go" | ".java" => ("编程语言源码", "Source Code"),
        ".jar" => ("Java 归档包", "Java Archive"),
        ".ttf" | ".otf" | ".woff" | ".woff2" => ("字体文件", "Font File"),
        ".png" | ".jpg" | ".jpeg" | ".webp" | ".gif" | ".bmp" | ".ico" | ".svg" => ("图像 / 图标资源", "Image / Icon"),
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

fn prune_node(mut raw: RawNode, depth: usize, max_depth: usize) -> DiskTreeNode {
    let cache_hint = raw.is_dir && is_cache_folder_name(&raw.name, &raw.path);
    let children = if !raw.is_dir || depth >= max_depth {
        Vec::new()
    } else {
        raw.children.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
        let mut kept = Vec::new();
        let mut rest_bytes = 0u64;
        let mut rest_files = 0usize;
        let mut rest_dirs = 0usize;
        let mut rest_count = 0usize;

        for (i, ch) in raw.children.into_iter().enumerate() {
            if ch.size_bytes == 0 && !ch.is_dir {
                continue;
            }
            if i < MAX_CHILDREN_PER_NODE {
                kept.push(prune_node(ch, depth + 1, max_depth));
            } else {
                rest_bytes += ch.size_bytes;
                rest_files += ch.file_count;
                rest_dirs += ch.dir_count + usize::from(ch.is_dir);
                rest_count += 1;
            }
        }
        if rest_count > 0 && rest_bytes > 0 {
            kept.push(DiskTreeNode {
                name: format!("[+{} smaller items]", rest_count),
                path: raw.path.clone(),
                is_dir: false,
                is_aggregate: true,
                is_cache_hint: false,
                aggregate_count: rest_count,
                size_bytes: rest_bytes,
                file_count: rest_files,
                dir_count: rest_dirs,
                ext: Some("(other)".to_string()),
                children: Vec::new(),
            });
        }
        kept
    };

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

/// Checks whether `target` is a protected drive root or OS core folder that must never be recycled as a whole.
pub fn is_protected_recycle_target(target: &Path) -> bool {
    let norm = normalize_win_path(target).to_ascii_lowercase();
    let trimmed = norm.trim_end_matches('\\');
    if trimmed.len() <= 2 && trimmed.ends_with(':') {
        return true;
    }
    let sys_root = std::env::var("SystemRoot")
        .unwrap_or_else(|_| r"C:\Windows".to_string())
        .to_ascii_lowercase();
    let protected_env_dirs = [
        sys_root.clone(),
        format!(r"{}\system32", sys_root),
        format!(r"{}\syswow64", sys_root),
        format!(r"{}\winsxs", sys_root),
        std::env::var("ProgramFiles").unwrap_or_default().to_ascii_lowercase(),
        std::env::var("ProgramFiles(x86)").unwrap_or_default().to_ascii_lowercase(),
        std::env::var("ProgramW6432").unwrap_or_default().to_ascii_lowercase(),
        std::env::var("ProgramData").unwrap_or_default().to_ascii_lowercase(),
        std::env::var("USERPROFILE").unwrap_or_default().to_ascii_lowercase(),
        std::env::var("LOCALAPPDATA").unwrap_or_default().to_ascii_lowercase(),
        std::env::var("APPDATA").unwrap_or_default().to_ascii_lowercase(),
    ];
    protected_env_dirs
        .iter()
        .filter(|s| !s.is_empty())
        .any(|p| p.trim_end_matches('\\') == trimmed)
}

/// Safely moves a file or directory into the Windows Recycle Bin (`FOF_ALLOWUNDO`) and returns bytes freed.
pub fn recycle_path(target: &Path) -> Result<u64> {
    if !target.exists() {
        return Err(anyhow!("Path does not exist: {}", target.display()));
    }
    if is_protected_recycle_target(target) {
        return Err(anyhow!(
            "Refusing to recycle protected system/root directory: {}",
            target.display()
        ));
    }

    let size_bytes = measure_path_bytes(target);
    let clean_str = normalize_win_path(target);
    let mut wide: Vec<u16> = OsStr::new(&clean_str).encode_wide().collect();
    wide.push(0);
    wide.push(0); // SHFileOperationW requires double-null terminated pFrom

    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.wFunc = FO_DELETE;
    op.pFrom = wide.as_ptr();
    op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as u16;

    let rc = unsafe { SHFileOperationW(&mut op) };
    if rc != 0 || op.fAnyOperationsAborted != 0 {
        return Err(anyhow!(
            "Failed to move '{}' to Recycle Bin (code {}, aborted={})",
            clean_str,
            rc,
            op.fAnyOperationsAborted
        ));
    }

    invalidate_orphans_cache();
    Ok(size_bytes)
}

fn normalize_win_path(path: &Path) -> String {
    let pb: PathBuf = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let s = pb.to_string_lossy().into_owned();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

fn measure_path_bytes(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
        return 0;
    }
    if meta.is_file() {
        return meta.len();
    }
    let Ok(rd) = std::fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0u64;
    let mut sub_dirs = Vec::new();
    for entry in rd.flatten() {
        let Ok(m) = entry.metadata() else {
            continue;
        };
        if (m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
            continue;
        }
        if m.is_file() {
            total = total.saturating_add(m.len());
        } else if m.is_dir() {
            sub_dirs.push(entry.path());
        }
    }
    total.saturating_add(sub_dirs.par_iter().map(|d| measure_path_bytes(d)).sum::<u64>())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateFileEntry {
    pub path: String,
    pub name: String,
    pub modified_date: String,
    pub modified_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateGroup {
    pub hash_hex: String,
    pub size_bytes: u64,
    pub wasted_bytes: u64,
    pub files: Vec<DuplicateFileEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateScanReport {
    pub root_path: String,
    pub elapsed_ms: u64,
    pub total_groups: usize,
    pub total_wasted_bytes: u64,
    pub groups: Vec<DuplicateGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrphanDirEntry {
    pub name: String,
    pub path: String,
    pub scope_label: String,
    pub size_bytes: u64,
    pub file_count: usize,
    pub modified_date: String,
}

/// Three-stage parallel duplicate file finder (`size -> 4KB head/tail sample -> 128-bit full hash`).
pub fn find_duplicate_files(root: &Path, min_size_bytes: u64) -> Result<DuplicateScanReport> {
    if !root.exists() {
        return Err(anyhow!("Path does not exist: {}", root.display()));
    }
    let started = Instant::now();
    let threshold = min_size_bytes.max(64 * 1024);
    let sys_root = std::env::var("SystemRoot")
        .unwrap_or_else(|_| r"C:\Windows".to_string())
        .to_ascii_lowercase();
    let winsxs_prefix = format!(r"{}\winsxs", sys_root);
    let driverstore_prefix = format!(r"{}\system32\driverstore", sys_root);

    let mut by_size: HashMap<u64, Vec<(PathBuf, u64)>> = HashMap::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let p_str = e.path().to_string_lossy().to_ascii_lowercase();
            if p_str.starts_with(&winsxs_prefix) || p_str.starts_with(&driverstore_prefix) {
                return false;
            }
            if e.path() == root {
                return true;
            }
            std::fs::symlink_metadata(e.path())
                .map(|m| (m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) == 0)
                .unwrap_or(false)
        })
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let sz = meta.len();
        if sz < threshold {
            continue;
        }
        let mod_unix = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        by_size.entry(sz).or_default().push((entry.into_path(), mod_unix));
    }

    let size_candidates: Vec<(u64, PathBuf, u64)> = by_size
        .into_iter()
        .filter(|(_, list)| list.len() >= 2)
        .flat_map(|(sz, list)| list.into_iter().map(move |(p, m)| (sz, p, m)))
        .collect();

    let sampled: Vec<(u64, u64, PathBuf, u64)> = size_candidates
        .into_par_iter()
        .filter_map(|(sz, path, mod_unix)| {
            let sample = sample_hash_file(&path, sz)?;
            Some((sz, sample, path, mod_unix))
        })
        .collect();

    let mut by_sample: HashMap<(u64, u64), Vec<(PathBuf, u64)>> = HashMap::new();
    for (sz, sample, path, mod_unix) in sampled {
        by_sample.entry((sz, sample)).or_default().push((path, mod_unix));
    }

    let full_candidates: Vec<(u64, PathBuf, u64)> = by_sample
        .into_iter()
        .filter(|(_, list)| list.len() >= 2)
        .flat_map(|((sz, _), list)| list.into_iter().map(move |(p, m)| (sz, p, m)))
        .collect();

    let hashed: Vec<(u64, u128, PathBuf, u64)> = full_candidates
        .into_par_iter()
        .filter_map(|(sz, path, mod_unix)| {
            let h = full_hash_128(&path)?;
            Some((sz, h, path, mod_unix))
        })
        .collect();

    let mut by_full: HashMap<(u64, u128), Vec<DuplicateFileEntry>> = HashMap::new();
    for (sz, h, path, mod_unix) in hashed {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let modified_date = DateTime::from_timestamp(mod_unix as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        by_full.entry((sz, h)).or_default().push(DuplicateFileEntry {
            path: path.to_string_lossy().into_owned(),
            name,
            modified_date,
            modified_unix: mod_unix,
        });
    }

    let mut groups = Vec::new();
    for ((sz, h), mut files) in by_full {
        if files.len() < 2 {
            continue;
        }
        files.sort_by(|a, b| b.modified_unix.cmp(&a.modified_unix));
        let wasted_bytes = sz.saturating_mul((files.len() - 1) as u64);
        groups.push(DuplicateGroup {
            hash_hex: format!("{:032x}", h),
            size_bytes: sz,
            wasted_bytes,
            files,
        });
    }

    groups.sort_by(|a, b| b.wasted_bytes.cmp(&a.wasted_bytes));
    let total_groups = groups.len();
    let total_wasted_bytes: u64 = groups.iter().map(|g| g.wasted_bytes).sum();
    groups.truncate(100);

    Ok(DuplicateScanReport {
        root_path: root.to_string_lossy().into_owned(),
        elapsed_ms: started.elapsed().as_millis() as u64,
        total_groups,
        total_wasted_bytes,
        groups,
    })
}

fn sample_hash_file(path: &Path, size: u64) -> Option<u64> {
    let mut f = File::open(path).ok()?;
    let mut h: u64 = 0xcbf29ce484222325 ^ size;
    let mut buf = [0u8; 4096];
    let n1 = f.read(&mut buf).ok()?;
    for &b in &buf[..n1] {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    if size > 4096 {
        let tail_pos = size.saturating_sub(4096);
        f.seek(SeekFrom::Start(tail_pos)).ok()?;
        let n2 = f.read(&mut buf).ok()?;
        for &b in &buf[..n2] {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    Some(h)
}

fn full_hash_128(path: &Path) -> Option<u128> {
    let mut f = File::open(path).ok()?;
    let mut h1: u64 = 0xcbf29ce484222325;
    let mut h2: u64 = 0x9e3779b97f4a7c15;
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        for (i, &b) in buf[..n].iter().enumerate() {
            h1 ^= b as u64;
            h1 = h1.wrapping_mul(0x100000001b3);
            h2 = h2.wrapping_add((b as u64).wrapping_mul((i as u64).wrapping_add(1)));
            h2 = h2.rotate_left(13).wrapping_mul(0xff51afd7ed558ccd);
        }
    }
    Some(((h1 as u128) << 64) | (h2 as u128))
}

static ORPHANS_CACHE: OnceLock<RwLock<Option<(Instant, Vec<OrphanDirEntry>)>>> = OnceLock::new();

pub fn invalidate_orphans_cache() {
    if let Some(lock) = ORPHANS_CACHE.get() {
        if let Ok(mut w) = lock.write() {
            *w = None;
        }
    }
}

/// Scans top-level AppData / ProgramData directories and flags large folders not matched
/// against active Uninstall keys, installed AppX apps, or known Windows/vendor safelists.
pub fn find_orphaned_app_dirs() -> Vec<OrphanDirEntry> {
    let cache = ORPHANS_CACHE.get_or_init(|| RwLock::new(None));
    if let Ok(guard) = cache.read() {
        if let Some((ts, ref list)) = *guard {
            if ts.elapsed() < Duration::from_secs(30) {
                return list.clone();
            }
        }
    }
    let computed = compute_orphaned_app_dirs();
    if let Ok(mut w) = cache.write() {
        *w = Some((Instant::now(), computed.clone()));
    }
    computed
}

fn compute_orphaned_app_dirs() -> Vec<OrphanDirEntry> {
    let (installed_tokens, installed_dirs) = collect_installed_app_tokens();
    let mut safelist: HashSet<&'static str> = [
        "microsoft",
        "windows",
        "package cache",
        "packages",
        "temp",
        "d3dscache",
        "connecteddevicesplatform",
        "crashdumps",
        "comms",
        "publishers",
        "virtualstore",
        "cef",
        "squirreltemp",
        "fontcache",
        "usoshared",
        "ssh",
        "git",
        "npm",
        "npm-cache",
        "pip",
        "uv",
        "pnpm",
        "yarn",
        "cargo",
        "rustup",
        "go-build",
        "nuget",
        "jetbrains",
        "vscode",
        "code",
        "google",
        "mozilla",
        "nvidia",
        "nvidia corporation",
        "intel",
        "amd",
        "realtek",
        "dolby",
        "lenovo",
        "apple",
        "apple computer",
        "steam",
        "epicgameslauncher",
        "unity",
        "unrealengine",
        "solidworks",
        "ansys",
        "codesys",
        "codemeter",
        "bonjour",
        "coremail",
        "baidunetdisk",
        "iconcache",
        "elevateddiagnostics",
        "low",
        "programs",
        "peernetworking",
        "application data",
        "history",
        "local",
        "roaming",
    ]
    .into_iter()
    .collect();
    let _ = &mut safelist;

    let mut candidates: Vec<(String, PathBuf, String)> = Vec::new();
    for (env_var, label) in [
        ("LOCALAPPDATA", "AppData\\Local"),
        ("APPDATA", "AppData\\Roaming"),
        ("ProgramData", "ProgramData"),
    ] {
        let Ok(base_str) = std::env::var(env_var) else {
            continue;
        };
        let Ok(rd) = std::fs::read_dir(&base_str) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            if !meta.is_dir() || (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name.starts_with('$') {
                continue;
            }
            let lower_name = name.trim().to_ascii_lowercase();
            let lower_path = p.to_string_lossy().to_ascii_lowercase();

            if safelist.contains(lower_name.as_str())
                || installed_dirs.iter().any(|id_path| lower_path.starts_with(id_path) || id_path.starts_with(&lower_path))
                || matches_app_tokens(&lower_name, &installed_tokens, &safelist)
            {
                continue;
            }
            let mod_date = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .and_then(|d| DateTime::from_timestamp(d.as_secs() as i64, 0))
                .map(|dt| dt.format("%Y-%m-%d").to_string())
                .unwrap_or_default();
            candidates.push((name, p, format!("{}|{}", label, mod_date)));
        }
    }

    let mut results: Vec<OrphanDirEntry> = candidates
        .into_par_iter()
        .filter_map(|(name, path, meta_tag)| {
            let (size_bytes, file_count) = measure_dir_stats(&path);
            if size_bytes < 1_048_576 {
                return None;
            }
            let (scope_label, modified_date) = meta_tag.split_once('|').unwrap_or(("", ""));
            Some(OrphanDirEntry {
                name,
                path: path.to_string_lossy().into_owned(),
                scope_label: scope_label.to_string(),
                size_bytes,
                file_count,
                modified_date: modified_date.to_string(),
            })
        })
        .collect();

    results.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    results
}

fn measure_dir_stats(dir: &Path) -> (u64, usize) {
    const MAX_FILES_PER_ORPHAN_DIR: usize = 20_000;
    let mut size = 0u64;
    let mut files = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&cur) else {
            continue;
        };
        for entry in rd.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                continue;
            }
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                size = size.saturating_add(meta.len());
                files += 1;
                if files >= MAX_FILES_PER_ORPHAN_DIR {
                    return (size, files);
                }
            }
        }
    }
    (size, files)
}

fn collect_installed_app_tokens() -> (HashSet<String>, Vec<String>) {
    let mut tokens = HashSet::new();
    let mut dirs = Vec::new();

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let targets = [
        (&hklm, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall", KEY_READ | KEY_WOW64_64KEY),
        (&hklm, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall", KEY_READ | KEY_WOW64_32KEY),
        (&hkcu, r"Software\Microsoft\Windows\CurrentVersion\Uninstall", KEY_READ),
    ];

    for (root, sub, sam) in targets {
        if let Ok(uninst) = root.open_subkey_with_flags(sub, sam) {
            for k in uninst.enum_keys().flatten() {
                let Ok(sk) = uninst.open_subkey_with_flags(&k, sam) else {
                    continue;
                };
                for val_name in ["DisplayName", "Publisher"] {
                    if let Ok(s) = sk.get_value::<String, _>(val_name) {
                        add_tokens(&s, &mut tokens);
                    }
                }
                if let Ok(loc) = sk.get_value::<String, _>("InstallLocation") {
                    let norm = loc.trim().trim_end_matches('\\').to_ascii_lowercase();
                    if norm.len() >= 4 {
                        dirs.push(norm);
                    }
                }
            }
        }
    }

    for pkg in crate::appx::list_appx_packages() {
        add_tokens(&pkg.name, &mut tokens);
        add_tokens(&pkg.display_name.zh, &mut tokens);
        add_tokens(&pkg.display_name.en, &mut tokens);
    }

    (tokens, dirs)
}

fn add_tokens(s: &str, tokens: &mut HashSet<String>) {
    let full = s.trim().to_ascii_lowercase();
    if full.len() >= 3 {
        tokens.insert(full);
    }
    for part in s.split(|c: char| !c.is_ascii_alphanumeric()) {
        let lp = part.to_ascii_lowercase();
        if lp.len() >= 3 {
            tokens.insert(lp);
        }
    }
}

fn matches_app_tokens(
    dir_lower: &str,
    tokens: &HashSet<String>,
    safelist: &HashSet<&'static str>,
) -> bool {
    if tokens.contains(dir_lower) {
        return true;
    }
    for part in dir_lower.split(|c: char| !c.is_ascii_alphanumeric()) {
        if part.len() >= 4 && (tokens.contains(part) || safelist.contains(part)) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;

    #[test]
    fn test_diskmap_aggregation_extensions_and_pruning() {
        let base = std::env::temp_dir().join(format!("dismnext_diskmap_test_{}", std::process::id()));
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

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn test_protected_recycle_targets_and_sandbox_recycle() {
        assert!(is_protected_recycle_target(Path::new(r"C:\")));
        assert!(is_protected_recycle_target(Path::new(r"C:\Windows")));
        assert!(is_protected_recycle_target(Path::new(r"C:\Windows\System32")));
        assert!(is_protected_recycle_target(Path::new(r"C:\Program Files")));

        let sample = std::env::temp_dir().join(format!("dismnext_recycle_test_{}.tmp", std::process::id()));
        File::create(&sample).unwrap().write_all(b"recycle_me").unwrap();
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
        File::create(base.join("a\\dup1.bin")).unwrap().write_all(&same_payload).unwrap();
        File::create(base.join("b\\dup2.bin")).unwrap().write_all(&same_payload).unwrap();
        File::create(base.join("b\\unique.bin")).unwrap().write_all(&diff_payload).unwrap();

        let report = find_duplicate_files(&base, 64 * 1024).unwrap();
        assert_eq!(report.total_groups, 1);
        assert_eq!(report.total_wasted_bytes, 70 * 1024);
        assert_eq!(report.groups[0].files.len(), 2);

        let _ = fs::remove_dir_all(&base);
    }
}
