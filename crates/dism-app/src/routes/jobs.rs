use super::{parse_query_param, read_json_body, unknown_endpoint};
use crate::scan_jobs::ScanLane;
use crate::state::AppState;
use crate::state::MaintenanceGuard;
use anyhow::Result;
use dism_core::cleaner::{scan_all_with_progress, CleanupLease};
use dism_core::diskmap::{analyze_path_with_options, find_duplicate_files_with_progress};
use dism_core::features::{list_optional_features, set_feature_enabled_with_progress};
use dism_core::maintenance::MaintenanceProgress;
use dism_core::path_recycle::{
    recycle_reviewed_path_with_progress, review_recycle_path_with_progress, RecycleTarget,
};
use dism_core::rescue::run_rescue_action_with_progress;
use dism_core::systasks::run_system_task_with_progress;
use dism_core::SystemInfo;
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
use tiny_http::{Method, Request};

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ScanRequest {
    DiskAnalysis {
        path: String,
        max_depth: Option<usize>,
        #[serde(default)]
        measure_allocation: bool,
    },
    DuplicateFiles {
        path: String,
        min_size_bytes: Option<u64>,
    },
    CleanupRules,
    RecycleReview {
        path: PathBuf,
        is_dir: bool,
        scan_started_at: String,
    },
    RecyclePath(RecycleTarget),
}

#[derive(Deserialize)]
struct CancelScanReq {
    id: u64,
}

pub(super) fn handle(
    req: &mut Request,
    path: &str,
    query: &str,
    state: &Arc<AppState>,
) -> Result<serde_json::Value> {
    match (req.method(), path) {
        (&Method::Post, "/api/maintenance/start") => {
            let request: MaintenanceRequest = read_json_body(req)?;
            let app = Arc::clone(state);
            let id = state
                .maintenance_jobs
                .start(ScanLane::Default, move |progress| {
                    run_maintenance(&app, request, progress)
                })?;
            Ok(serde_json::json!({ "id": id }))
        }
        (&Method::Get, "/api/maintenance/status") => {
            let id = parse_query_param(query, "id")
                .ok_or_else(|| anyhow::anyhow!("Missing ?id= parameter"))?
                .parse()?;
            state.maintenance_jobs.status(id)
        }
        (&Method::Post, "/api/maintenance/cancel") => {
            let payload: CancelScanReq = read_json_body(req)?;
            state.maintenance_jobs.cancel(payload.id)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        (&Method::Post, "/api/scans/start") => {
            let payload: ScanRequest = read_json_body(req)?;
            let lane = match &payload {
                ScanRequest::DiskAnalysis { .. } => ScanLane::DiskAnalysis,
                ScanRequest::DuplicateFiles { .. } => ScanLane::Duplicates,
                ScanRequest::CleanupRules => ScanLane::CleanupRules,
                ScanRequest::RecycleReview { .. } | ScanRequest::RecyclePath(_) => {
                    ScanLane::Recycle
                }
            };
            let app = Arc::clone(state);
            let id = state.scan_jobs.start(lane, move |progress| match payload {
                ScanRequest::DiskAnalysis {
                    path,
                    max_depth,
                    measure_allocation,
                } => {
                    let report = analyze_path_with_options(
                        Path::new(path.trim()),
                        max_depth.unwrap_or(4),
                        measure_allocation,
                        Some(progress),
                    )?;
                    Ok(serde_json::to_value(report)?)
                }
                ScanRequest::DuplicateFiles {
                    path,
                    min_size_bytes,
                } => {
                    let report = find_duplicate_files_with_progress(
                        Path::new(path.trim()),
                        min_size_bytes.unwrap_or(512 * 1024),
                        progress,
                    )?;
                    Ok(serde_json::to_value(report)?)
                }
                ScanRequest::CleanupRules => {
                    let started = Instant::now();
                    let sys = SystemInfo::collect();
                    let rules = app.catalog.read().unwrap().cleanup_rules.clone();
                    let results = scan_all_with_progress(&rules, &sys, progress)?;
                    if progress.is_cancelled() {
                        return Err(dism_core::scan_progress::ScanCancelled.into());
                    }
                    Ok(app.publish_cleanup_scan(results, started))
                }
                ScanRequest::RecycleReview {
                    path,
                    is_dir,
                    scan_started_at,
                } => {
                    let review = review_recycle_path_with_progress(
                        &path,
                        is_dir,
                        scan_started_at.parse()?,
                        progress,
                    )?;
                    Ok(serde_json::to_value(review)?)
                }
                ScanRequest::RecyclePath(target) => {
                    let _operation = app.begin_cleanup_mutation()?;
                    let item = recycle_reviewed_path_with_progress(&target, progress)?;
                    Ok(serde_json::json!({ "item": item, "system": SystemInfo::collect() }))
                }
            })?;
            Ok(serde_json::json!({ "id": id }))
        }
        (&Method::Get, "/api/scans/status") => {
            let id = parse_query_param(query, "id")
                .ok_or_else(|| anyhow::anyhow!("Missing ?id= parameter"))?
                .parse()?;
            state.scan_jobs.status(id)
        }
        (&Method::Post, "/api/scans/cancel") => {
            let payload: CancelScanReq = read_json_body(req)?;
            state.scan_jobs.cancel(payload.id)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        _ => unknown_endpoint(path),
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum MaintenanceRequest {
    Feature { name: String, enabled: bool },
    SystemTask { task_id: String },
    Rescue { action_id: String },
}

pub(super) fn run_maintenance(
    state: &AppState,
    request: MaintenanceRequest,
    progress: &MaintenanceProgress,
) -> Result<serde_json::Value> {
    let _lease = CleanupLease::try_acquire()?;
    {
        let mut operation = state.try_cleanup()?;
        state.invalidate_cleanup(&mut operation);
        operation.maintenance_active = true;
    }
    let _guard = MaintenanceGuard(state);
    match request {
        MaintenanceRequest::Feature { name, enabled } => {
            let outcome = set_feature_enabled_with_progress(&name, enabled, progress)?;
            Ok(serde_json::json!({ "outcome": outcome, "features": list_optional_features() }))
        }
        MaintenanceRequest::SystemTask { task_id } => {
            let outcome = run_system_task_with_progress(task_id.trim(), progress)?;
            Ok(serde_json::json!({ "outcome": outcome, "system": SystemInfo::collect() }))
        }
        MaintenanceRequest::Rescue { action_id } => Ok(serde_json::to_value(
            run_rescue_action_with_progress(&action_id, progress)?,
        )?),
    }
}
