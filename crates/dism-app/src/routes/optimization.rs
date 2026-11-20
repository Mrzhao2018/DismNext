use super::{read_json_body, unknown_endpoint};
use crate::state::AppState;
use anyhow::Result;
use dism_core::optimizer::{
    apply_optimization, apply_optimization_batch, evaluate_all_groups, export_optimization_profile,
    get_optimization_presets, ApplyValue, OptimizationProfileEntry, ScopeName,
};
use dism_core::SystemInfo;
use serde::Deserialize;
use tiny_http::{Method, Request};

#[derive(Deserialize)]
struct OptApplyReq {
    item_id: String,
    scope: ScopeName,
    value: ApplyValue,
}

#[derive(Deserialize)]
struct RestoreReq {
    snapshot_id: String,
}

#[derive(Deserialize)]
struct OptBatchReq {
    entries: Vec<OptimizationProfileEntry>,
}

pub(super) fn handle(req: &mut Request, path: &str, state: &AppState) -> Result<serde_json::Value> {
    let sys = SystemInfo::collect();
    match (req.method(), path) {
        (&Method::Get, "/api/optimizer/groups") => {
            let cat = state.catalog.read().unwrap();
            let groups = evaluate_all_groups(&cat.optimization_groups, &sys);
            Ok(serde_json::json!({ "groups": groups }))
        }
        (&Method::Post, "/api/optimizer/apply") => {
            let payload: OptApplyReq = read_json_body(req)?;
            let _operation = state.begin_cleanup_mutation();
            let cat = state.catalog.read().unwrap();
            let outcome = apply_optimization(
                &cat.optimization_groups,
                &payload.item_id,
                payload.scope,
                payload.value,
                &state.snap_mgr,
            )?;
            let refreshed = evaluate_all_groups(&cat.optimization_groups, &sys);
            Ok(serde_json::json!({
                "outcome": outcome,
                "groups": refreshed,
            }))
        }
        (&Method::Get, "/api/optimizer/presets") => {
            let presets = get_optimization_presets();
            Ok(serde_json::json!({ "presets": presets }))
        }
        (&Method::Post, "/api/optimizer/batch") => {
            let payload: OptBatchReq = read_json_body(req)?;
            let _operation = state.begin_cleanup_mutation();
            let cat = state.catalog.read().unwrap();
            let outcome = apply_optimization_batch(
                &cat.optimization_groups,
                &payload.entries,
                &state.snap_mgr,
            )?;
            let groups = evaluate_all_groups(&cat.optimization_groups, &sys);
            Ok(serde_json::json!({
                "outcome": outcome,
                "groups": groups,
            }))
        }
        (&Method::Get, "/api/optimizer/profile/export") => {
            let cat = state.catalog.read().unwrap();
            let profile = export_optimization_profile(&cat.optimization_groups, &sys);
            Ok(serde_json::to_value(profile)?)
        }
        (&Method::Get, "/api/snapshots") => {
            let list = state.snap_mgr.list()?;
            Ok(serde_json::json!({ "snapshots": list }))
        }
        (&Method::Post, "/api/snapshots/restore") => {
            let payload: RestoreReq = read_json_body(req)?;
            let _operation = state.begin_cleanup_mutation();
            let restored = state.snap_mgr.restore(&payload.snapshot_id)?;
            let cat = state.catalog.read().unwrap();
            let refreshed_groups = evaluate_all_groups(&cat.optimization_groups, &sys);
            Ok(serde_json::json!({
                "restored": restored,
                "groups": refreshed_groups,
            }))
        }
        _ => unknown_endpoint(path),
    }
}
