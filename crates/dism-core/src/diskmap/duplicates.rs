use super::check_scan;
use crate::duplicate_recycle::{FileIdentity, FileState};
use crate::file_stamp::open_for_verification;
use crate::scan_progress::{ScanPhase, ScanProgress};
use crate::win_util::{normalize_win_path, windows_dir_or_default, FILE_ATTRIBUTE_REPARSE_POINT};
use anyhow::{anyhow, Result};
use chrono::DateTime;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::time::Instant;
use walkdir::WalkDir;
use windows_sys::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_READ_DATA, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateFileEntry {
    pub path: String,
    pub name: String,
    pub modified_date: String,
    pub modified_unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stamp: Option<String>,
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

/// Finds duplicate files by size, sampled content and a full BLAKE3 hash.
pub fn find_duplicate_files(root: &Path, min_size_bytes: u64) -> Result<DuplicateScanReport> {
    find_duplicate_files_inner(root, min_size_bytes, None)
}

pub fn find_duplicate_files_with_progress(
    root: &Path,
    min_size_bytes: u64,
    progress: &ScanProgress,
) -> Result<DuplicateScanReport> {
    find_duplicate_files_inner(root, min_size_bytes, Some(progress))
}

fn find_duplicate_files_inner(
    root: &Path,
    min_size_bytes: u64,
    progress: Option<&ScanProgress>,
) -> Result<DuplicateScanReport> {
    check_scan(progress)?;
    if !root.exists() {
        return Err(anyhow!("Path does not exist: {}", root.display()));
    }
    let started = Instant::now();
    let threshold = min_size_bytes.max(64 * 1024);
    let windows = windows_dir_or_default();
    let winsxs = normalize_win_path(&windows.join("WinSxS"));
    let driverstore = normalize_win_path(&windows.join(r"System32\DriverStore"));

    let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    let mut walker = WalkDir::new(root)
        .follow_links(false)
        .follow_root_links(false)
        .into_iter();
    loop {
        check_scan(progress)?;
        let Some(entry) = walker.next() else {
            break;
        };
        let Ok(entry) = entry else {
            continue;
        };
        let directory = entry.file_type().is_dir();
        // WalkDir uses the Windows directory entry's cached attributes and size.
        let Ok(meta) = entry.metadata() else {
            if directory {
                walker.skip_current_dir();
            }
            continue;
        };
        let servicing_path = if directory || entry.depth() == 0 {
            let path = normalize_win_path(entry.path());
            path.starts_with(&winsxs) || path.starts_with(&driverstore)
        } else {
            false
        };
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || servicing_path {
            if directory {
                walker.skip_current_dir();
            }
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        if let Some(progress) = progress {
            progress.file_processed();
        }
        let size = meta.len();
        if size >= threshold {
            by_size.entry(size).or_default().push(entry.into_path());
        }
    }

    let size_candidates = distinct_duplicate_candidates(by_size, progress)?;
    if let Some(progress) = progress {
        progress.set_phase(ScanPhase::Sampling, Some(size_candidates.len()));
    }
    let sampled = size_candidates
        .into_par_iter()
        .filter_map(|(sz, path)| {
            if let Err(error) = check_scan(progress) {
                return Some(Err(error));
            }
            let sample = sample_hash_file(&path, sz, progress);
            if let Some(progress) = progress {
                progress.file_processed();
            }
            sample.map(|hash| Ok((sz, hash, path)))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut by_sample: HashMap<(u64, blake3::Hash), Vec<PathBuf>> = HashMap::new();
    for (size, sample, path) in sampled {
        check_scan(progress)?;
        by_sample.entry((size, sample)).or_default().push(path);
    }

    let full_candidates: Vec<_> = by_sample
        .into_iter()
        .filter(|(_, paths)| paths.len() >= 2)
        .flat_map(|((size, _), paths)| paths.into_iter().map(move |path| (size, path)))
        .collect();

    if let Some(progress) = progress {
        progress.set_phase(ScanPhase::Hashing, Some(full_candidates.len()));
    }
    let hashed = full_candidates
        .into_par_iter()
        .filter_map(|(size, path)| {
            if let Err(error) = check_scan(progress) {
                return Some(Err(error));
            }
            let result = (|| {
                let mut file = open_hash_file(&path, size)?;
                let modified = file.metadata().ok()?.last_write_time();
                let state = FileState::read(&file).ok();
                let hash = full_hash(&mut file, progress)?;
                if state != FileState::read(&file).ok() {
                    return None;
                }
                Some((size, hash, path, modified, state))
            })();
            if let Some(progress) = progress {
                progress.file_processed();
            }
            result.map(Ok)
        })
        .collect::<Result<Vec<_>>>()?;

    if let Some(progress) = progress {
        progress.set_phase(ScanPhase::Finishing, None);
    }

    const WINDOWS_EPOCH_SECONDS: u64 = 11_644_473_600;
    const TICKS_PER_SECOND: u64 = 10_000_000;
    let mut by_full: HashMap<(u64, blake3::Hash), Vec<(u64, DuplicateFileEntry)>> = HashMap::new();
    let mut identities = HashSet::new();
    for (sz, h, path, modified, state) in hashed {
        check_scan(progress)?;
        let modified_ticks = state.as_ref().map_or(modified, |state| state.modified);
        let stamp = match state {
            Some(state) => {
                let stamp = state.stamp();
                // Also guard against aliases created after the first identity check.
                if !identities.insert(state.identity) {
                    continue;
                }
                Some(stamp)
            }
            None => None,
        };
        let mod_unix = (modified_ticks / TICKS_PER_SECOND).saturating_sub(WINDOWS_EPOCH_SECONDS);
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let modified_date = DateTime::from_timestamp(mod_unix as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        by_full.entry((sz, h)).or_default().push((
            modified_ticks,
            DuplicateFileEntry {
                path: path.to_string_lossy().into_owned(),
                name,
                modified_date,
                modified_unix: mod_unix,
                stamp,
            },
        ));
    }

    let mut groups = Vec::new();
    for ((sz, h), mut files) in by_full {
        check_scan(progress)?;
        if files.len() < 2 {
            continue;
        }
        files.sort_by(|(a_time, a), (b_time, b)| {
            b_time.cmp(a_time).then_with(|| a.path.cmp(&b.path))
        });
        let wasted_bytes = sz.saturating_mul((files.len() - 1) as u64);
        groups.push(DuplicateGroup {
            hash_hex: h.to_hex().to_string(),
            size_bytes: sz,
            wasted_bytes,
            files: files.into_iter().map(|(_, file)| file).collect(),
        });
    }

    groups.sort_by_key(|entry| std::cmp::Reverse(entry.wasted_bytes));
    let total_groups = groups.len();
    let total_wasted_bytes: u64 = groups.iter().map(|g| g.wasted_bytes).sum();
    groups.truncate(100);

    check_scan(progress)?;
    Ok(DuplicateScanReport {
        root_path: root.to_string_lossy().into_owned(),
        elapsed_ms: started.elapsed().as_millis() as u64,
        total_groups,
        total_wasted_bytes,
        groups,
    })
}

fn distinct_duplicate_candidates(
    by_size: HashMap<u64, Vec<PathBuf>>,
    progress: Option<&ScanProgress>,
) -> Result<Vec<(u64, PathBuf)>> {
    let candidates: Vec<_> = by_size
        .into_iter()
        .filter(|(_, paths)| paths.len() >= 2)
        .flat_map(|(size, paths)| paths.into_iter().map(move |path| (size, path)))
        .collect();
    let probed = candidates
        .into_par_iter()
        .map(|(size, path)| {
            check_scan(progress)?;
            let identity = hard_link_identity(&path);
            Ok((size, path, identity))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut linked: HashMap<FileIdentity, (u64, PathBuf)> = HashMap::new();
    let mut distinct = Vec::new();
    for (size, path, identity) in probed {
        check_scan(progress)?;
        if let Some(identity) = identity {
            let entry = linked
                .entry(identity)
                .or_insert_with(|| (size, path.clone()));
            if path < entry.1 {
                *entry = (size, path);
            }
        } else {
            distinct.push((size, path));
        }
    }
    distinct.extend(linked.into_values());
    let mut counts = HashMap::new();
    for (size, _) in &distinct {
        *counts.entry(*size).or_insert(0usize) += 1;
    }
    distinct.retain(|(size, _)| counts[size] > 1);
    Ok(distinct)
}

fn hard_link_identity(path: &Path) -> Option<FileIdentity> {
    let sharing = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    let file = open_for_verification(path, false, 0, sharing).ok()?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) };
    if ok != 0 && info.nNumberOfLinks > 1 {
        FileIdentity::read(&file).ok()
    } else {
        None
    }
}

fn read_hash_chunk(
    reader: &mut impl Read,
    buffer: &mut [u8],
    progress: Option<&ScanProgress>,
) -> Option<usize> {
    check_scan(progress).ok()?;
    let bytes = reader.read(buffer).ok()?;
    if let Some(progress) = progress {
        progress.read_bytes(bytes);
    }
    Some(bytes)
}

fn open_hash_file(path: &Path, expected_size: u64) -> Option<File> {
    let file = open_for_verification(
        path,
        false,
        FILE_READ_DATA,
        FILE_SHARE_READ | FILE_SHARE_DELETE,
    )
    .ok()?;
    let metadata = file.metadata().ok()?;
    (metadata.is_file()
        && metadata.len() == expected_size
        && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0)
        .then_some(file)
}

fn sample_hash_file(
    path: &Path,
    size: u64,
    progress: Option<&ScanProgress>,
) -> Option<blake3::Hash> {
    let mut file = open_hash_file(path, size)?;
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0; 4096];
    for offset in [0, size - buffer.len() as u64] {
        file.seek(SeekFrom::Start(offset)).ok()?;
        let mut copied = 0;
        while copied < buffer.len() {
            let count = read_hash_chunk(&mut file, &mut buffer[copied..], progress)?;
            if count == 0 {
                return None;
            }
            copied += count;
        }
        hash.update(&buffer);
    }
    Some(hash.finalize())
}

pub(crate) fn full_hash(
    mut reader: impl Read,
    progress: Option<&ScanProgress>,
) -> Option<blake3::Hash> {
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = read_hash_chunk(&mut reader, &mut buffer, progress)?;
        if count == 0 {
            return Some(hash.finalize());
        }
        hash.update(&buffer[..count]);
    }
}
