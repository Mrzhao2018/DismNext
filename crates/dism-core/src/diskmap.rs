use crate::catalog::LocalizedText;
use crate::sysinfo::SystemInfo;
use anyhow::{anyhow, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::os::windows::fs::MetadataExt;
use std::path::Path;
use std::time::Instant;

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
const MAX_CHILDREN_PER_NODE: usize = 36;
const MAX_LARGEST_FILES: usize = 100;
const MAX_EXTENSIONS: usize = 30;

const PALETTE: &[&str] = &[
    "#0078D4", "#00B7C3", "#8764B8", "#107C10", "#D83B01", "#E3008C", "#038387", "#CA5010",
    "#498205", "#C239B3", "#004E8C", "#5C2D91", "#8E562E", "#0099BC", "#E74856", "#7A7574",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskTreeNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_aggregate: bool,
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

/// Performs a parallel WinDirStat-style directory tree & extension analysis without following NTFS reparse points.
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
            extensions.push(ExtensionStat {
                ext,
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

    let child_paths: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    let child_accs: Vec<ScanAccumulator> = if child_paths.len() >= 2 {
        child_paths
            .par_iter()
            .map(|p| scan_recursive(p))
            .collect()
    } else {
        child_paths.iter().map(|p| scan_recursive(p)).collect()
    };

    let mut total_bytes = 0u64;
    let mut total_files = 0usize;
    let mut total_dirs = 0usize;
    let mut children = Vec::with_capacity(child_accs.len());
    let mut exts: HashMap<String, (u64, usize)> = HashMap::new();
    let mut top_files: Vec<LargeFileEntry> = Vec::new();

    for c in child_accs {
        total_bytes += c.node.size_bytes;
        total_files += c.node.file_count;
        total_dirs += c.node.dir_count + usize::from(c.node.is_dir);
        children.push(c.node);
        for (ext, (sz, cnt)) in c.exts {
            let entry = exts.entry(ext).or_insert((0, 0));
            entry.0 += sz;
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

fn prune_node(mut raw: RawNode, depth: usize, max_depth: usize) -> DiskTreeNode {
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
        aggregate_count: 0,
        size_bytes: raw.size_bytes,
        file_count: raw.file_count,
        dir_count: raw.dir_count,
        ext: raw.ext,
        children,
    }
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
        let sub_a = base.join("alpha");
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
        assert_eq!(report.tree.children[0].name, "alpha");
        assert_eq!(report.extensions[0].ext, ".bin");
        assert_eq!(report.extensions[0].size_bytes, 4096);
        assert_eq!(report.largest_files[0].name, "big.bin");
        assert!(report.extensions.iter().any(|e| e.ext == "(no ext)"));

        let _ = fs::remove_dir_all(&base);
    }
}
