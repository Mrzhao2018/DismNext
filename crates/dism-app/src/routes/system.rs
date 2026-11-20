use super::jobs::{run_maintenance, MaintenanceRequest};
use super::{read_json_body, unknown_endpoint};
use crate::scan_jobs::ScanBusy;
use crate::state::AppState;
use anyhow::Result;
use dism_core::features::list_optional_features;
use dism_core::offline_wim::inspect_wim;
use dism_core::rescue::{create_restore_point, list_restore_points};
use dism_core::sysinfo::relaunch_as_admin;
use dism_core::systasks::get_system_task_status;
use dism_core::SystemInfo;
use serde::Deserialize;
use std::path::Path;
use std::sync::atomic::Ordering;
use tiny_http::{Method, Request};

#[derive(Deserialize)]
struct FeatureToggleReq {
    name: String,
    enabled: bool,
}

#[derive(Deserialize)]
struct SysTaskRunReq {
    task_id: String,
}

#[derive(Deserialize)]
struct CreateRestorePointReq {
    description: String,
}

#[derive(Deserialize)]
struct RescueRunReq {
    action_id: String,
}

#[derive(Deserialize)]
struct WimInspectReq {
    path: String,
}

pub(super) fn handle(req: &mut Request, path: &str, state: &AppState) -> Result<serde_json::Value> {
    let sys = SystemInfo::collect();
    match (req.method(), path) {
        (&Method::Post, "/api/sys/elevate") => {
            if sys.is_admin {
                return Ok(serde_json::json!({ "already_admin": true, "relaunching": false }));
            }
            let _exclusive = state
                .requests
                .try_write()
                .map_err(|_| ScanBusy("desktop request"))?;
            if state.relaunching.load(Ordering::Acquire) {
                return Err(ScanBusy("desktop relaunch").into());
            }
            if state.maintenance_jobs.is_active()
                || state.scan_jobs.is_active()
                || crate::uninstall_running(state)
            {
                return Err(ScanBusy("background task").into());
            }
            anyhow::ensure!(
                state.catalog_path.is_none() && state.ui_dir.is_none() && !state.dev_origin,
                "Restart DismNext directly as administrator to use an external catalog or UI"
            );
            relaunch_as_admin(&["--port".to_string(), state.port.to_string()])?;
            state.relaunching.store(true, Ordering::Release);
            Ok(serde_json::json!({ "already_admin": false, "relaunching": true }))
        }
        (&Method::Get, "/api/sysinfo") => {
            let cat = state.catalog.read().unwrap();
            Ok(serde_json::json!({
                "system": sys,
                "catalog": {
                    "base_version": cat.base_version,
                    "cleanup_rules_total": cat.cleanup_rules.len(),
                    "optimization_groups_total": cat.optimization_groups.len(),
                    "optimization_items_total": cat.optimization_groups.iter().map(|g| g.items.len()).sum::<usize>(),
                }
            }))
        }
        (&Method::Get, "/api/features/list") => {
            let features = list_optional_features();
            Ok(serde_json::json!({ "features": features }))
        }
        (&Method::Post, "/api/features/toggle") => {
            let payload: FeatureToggleReq = read_json_body(req)?;
            let request = MaintenanceRequest::Feature {
                name: payload.name,
                enabled: payload.enabled,
            };
            state
                .maintenance_jobs
                .run_inline_with_progress(|progress| run_maintenance(state, request, progress))
        }
        (&Method::Get, "/api/systasks/status") => {
            let status = get_system_task_status();
            Ok(serde_json::to_value(status)?)
        }
        (&Method::Post, "/api/systasks/run") => {
            let payload: SysTaskRunReq = read_json_body(req)?;
            let request = MaintenanceRequest::SystemTask {
                task_id: payload.task_id,
            };
            state
                .maintenance_jobs
                .run_inline_with_progress(|progress| run_maintenance(state, request, progress))
        }
        (&Method::Get, "/api/rescue/restore-points") => {
            let points = list_restore_points();
            Ok(serde_json::json!({ "points": points }))
        }
        (&Method::Post, "/api/rescue/restore-points/create") => {
            let payload: CreateRestorePointReq = read_json_body(req)?;
            let message = create_restore_point(&payload.description)?;
            let points = list_restore_points();
            Ok(serde_json::json!({ "message": message, "points": points }))
        }
        (&Method::Post, "/api/rescue/run") => {
            let payload: RescueRunReq = read_json_body(req)?;
            let request = MaintenanceRequest::Rescue {
                action_id: payload.action_id,
            };
            state
                .maintenance_jobs
                .run_inline_with_progress(|progress| run_maintenance(state, request, progress))
        }
        (&Method::Post, "/api/rescue/wim/inspect") => {
            let payload: WimInspectReq = read_json_body(req)?;
            let path = payload.path.trim().trim_matches('"');
            Ok(serde_json::to_value(inspect_wim(Path::new(path))?)?)
        }
        _ => unknown_endpoint(path),
    }
}
