use super::{read_json_body, unknown_endpoint};
use crate::state::AppState;
use anyhow::Result;
use dism_core::diskmap::{
    analyze_path_with_options, find_duplicate_files, find_orphaned_app_dirs, get_presets,
};
use dism_core::duplicate_recycle::{recycle_duplicates, DuplicateRecycleTarget};
use dism_core::lockcheck::{find_locking_processes, terminate_locking_process};
use dism_core::path_recycle::{recycle_reviewed_path, review_recycle_path, RecycleTarget};
use dism_core::SystemInfo;
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    process::Command,
};
use tiny_http::{Method, Request};

#[derive(Deserialize)]
struct DiskAnalyzeReq {
    path: String,
    max_depth: Option<usize>,
    #[serde(default)]
    measure_allocation: bool,
}

#[derive(Deserialize)]
struct RevealReq {
    path: String,
}

#[derive(Deserialize)]
struct TerminatePidReq {
    path: String,
    pid: u32,
    process_start_time: String,
}

#[derive(Deserialize)]
struct DuplicatesReq {
    path: String,
    min_size_bytes: Option<u64>,
}

#[derive(Deserialize)]
struct RecycleDuplicatesReq {
    keeper: DuplicateRecycleTarget,
    candidates: Vec<DuplicateRecycleTarget>,
}

#[derive(Deserialize)]
struct RecycleReviewReq {
    path: PathBuf,
    is_dir: bool,
    scan_started_at: String,
}

pub(super) fn handle(req: &mut Request, path: &str, state: &AppState) -> Result<serde_json::Value> {
    match (req.method(), path) {
        (&Method::Get, "/api/diskmap/presets") => {
            let presets = get_presets(&SystemInfo::collect());
            Ok(serde_json::json!({ "presets": presets }))
        }
        (&Method::Post, "/api/diskmap/analyze") => {
            let payload: DiskAnalyzeReq = read_json_body(req)?;
            let report = state.scan_jobs.run_inline(|| {
                analyze_path_with_options(
                    Path::new(payload.path.trim()),
                    payload.max_depth.unwrap_or(4),
                    payload.measure_allocation,
                    None,
                )
            })?;
            Ok(serde_json::to_value(report)?)
        }
        (&Method::Post, "/api/diskmap/reveal") => {
            let payload: RevealReq = read_json_body(req)?;
            let target = Path::new(payload.path.trim());
            if !target.exists() {
                return Err(anyhow::anyhow!("Path does not exist: {}", payload.path));
            }
            let windows_dir = dism_core::system_tools::windows_dir()?;
            let _ = Command::new(windows_dir.join("explorer.exe"))
                .current_dir(windows_dir)
                .arg(format!("/select,{}", target.to_string_lossy()))
                .spawn();
            Ok(serde_json::json!({ "ok": true }))
        }
        (&Method::Post, "/api/diskmap/recycle") => {
            let payload: RecycleTarget = read_json_body(req)?;
            let _operation = state.begin_cleanup_mutation();
            let item = state
                .scan_jobs
                .run_inline(|| Ok(recycle_reviewed_path(&payload)))?;
            Ok(serde_json::json!({
                "item": item,
                "system": SystemInfo::collect(),
            }))
        }
        (&Method::Post, "/api/diskmap/recycle/review") => {
            let payload: RecycleReviewReq = read_json_body(req)?;
            let cutoff = payload.scan_started_at.parse()?;
            let review = state
                .scan_jobs
                .run_inline(|| Ok(review_recycle_path(&payload.path, payload.is_dir, cutoff)))?;
            Ok(serde_json::to_value(review)?)
        }
        (&Method::Post, "/api/diskmap/recycle-duplicates") => {
            let payload: RecycleDuplicatesReq = read_json_body(req)?;
            let _operation = state.begin_cleanup_mutation();
            let items = state
                .scan_jobs
                .run_inline(|| Ok(recycle_duplicates(&payload.keeper, &payload.candidates)))?;
            Ok(serde_json::json!({ "items": items, "system": SystemInfo::collect() }))
        }
        (&Method::Post, "/api/diskmap/duplicates") => {
            let payload: DuplicatesReq = read_json_body(req)?;
            let report = state.scan_jobs.run_inline(|| {
                find_duplicate_files(
                    Path::new(payload.path.trim()),
                    payload.min_size_bytes.unwrap_or(512 * 1024),
                )
            })?;
            Ok(serde_json::to_value(report)?)
        }
        (&Method::Get, "/api/diskmap/orphans") => {
            let orphans = find_orphaned_app_dirs();
            Ok(serde_json::json!({ "orphans": orphans }))
        }
        (&Method::Post, "/api/lockcheck/inspect") => {
            let payload: RevealReq = read_json_body(req)?;
            let processes = find_locking_processes(Path::new(payload.path.trim()));
            Ok(serde_json::json!({ "processes": processes }))
        }
        (&Method::Post, "/api/lockcheck/terminate") => {
            let payload: TerminatePidReq = read_json_body(req)?;
            let message = terminate_locking_process(
                Path::new(payload.path.trim()),
                payload.pid,
                payload.process_start_time.parse()?,
            )?;
            Ok(serde_json::json!({ "ok": true, "message": message }))
        }
        _ => unknown_endpoint(path),
    }
}
