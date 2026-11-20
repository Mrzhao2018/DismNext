use super::{read_json_body, unknown_endpoint};
use crate::state::AppState;
use anyhow::Result;
use dism_core::appx::{list_appx_packages, remove_appx_package};
use dism_core::uninstaller::{
    launch_win32_uninstaller, list_win32_apps, recycle_app_leftovers,
    scan_reviewable_app_leftovers, LeftoverRecycleTarget, UninstallState, Win32Uninstall,
};
use serde::Deserialize;
use tiny_http::{Method, Request};

#[derive(Deserialize)]
struct AppxRemoveReq {
    package_full_name: String,
}

#[derive(Deserialize)]
struct LeftoversReq {
    display_name: String,
    publisher: String,
    install_location: String,
}

#[derive(Deserialize)]
struct RecycleLeftoversReq {
    targets: Vec<LeftoverRecycleTarget>,
}

#[derive(Deserialize)]
struct UninstallWin32Req {
    app_id: String,
    use_quiet: Option<bool>,
}

#[derive(Deserialize)]
struct UninstallTargetReq {
    app_id: String,
}

pub(super) fn handle(req: &mut Request, path: &str, state: &AppState) -> Result<serde_json::Value> {
    match (req.method(), path) {
        (&Method::Get, "/api/appx/list") => {
            let packages = list_appx_packages();
            Ok(serde_json::json!({ "packages": packages }))
        }
        (&Method::Post, "/api/appx/remove") => {
            let payload: AppxRemoveReq = read_json_body(req)?;
            let _operation = state.begin_cleanup_mutation()?;
            remove_appx_package(&payload.package_full_name)?;
            let packages = list_appx_packages();
            Ok(serde_json::json!({
                "removed": payload.package_full_name,
                "packages": packages,
            }))
        }
        (&Method::Get, "/api/uninstaller/list") => {
            let apps = list_win32_apps();
            Ok(serde_json::json!({ "apps": apps }))
        }
        (&Method::Post, "/api/uninstaller/leftovers") => {
            let payload: LeftoversReq = read_json_body(req)?;
            let leftovers = scan_reviewable_app_leftovers(
                &payload.display_name,
                &payload.publisher,
                &payload.install_location,
            );
            Ok(serde_json::json!({ "leftovers": leftovers }))
        }
        (&Method::Post, "/api/uninstaller/leftovers/recycle") => {
            let payload: RecycleLeftoversReq = read_json_body(req)?;
            let _operation = state.begin_cleanup_mutation()?;
            let items = recycle_app_leftovers(&payload.targets);
            Ok(serde_json::json!({ "items": items }))
        }
        (&Method::Post, "/api/uninstaller/uninstall") => {
            let payload: UninstallWin32Req = read_json_body(req)?;
            let mut current = state.uninstall.lock().unwrap();
            if let Some(job) = current.as_mut() {
                anyhow::ensure!(
                    !matches!(
                        job.status()?.state,
                        UninstallState::Running | UninstallState::AwaitingVerification
                    ),
                    "An uninstallation is still in progress or awaiting verification"
                );
            }
            let _operation = state.begin_cleanup_mutation()?;
            let job = current.insert(launch_win32_uninstaller(
                payload.app_id.trim(),
                payload.use_quiet.unwrap_or(false),
            )?);
            Ok(serde_json::to_value(job.status()?)?)
        }
        (&Method::Get, "/api/uninstaller/uninstall/status") => {
            let mut current = state.uninstall.lock().unwrap();
            let status = current.as_mut().map(Win32Uninstall::status).transpose()?;
            Ok(serde_json::json!({ "uninstall": status }))
        }
        (&Method::Post, "/api/uninstaller/uninstall/leftovers") => {
            let payload: UninstallTargetReq = read_json_body(req)?;
            let app = state
                .uninstall
                .lock()
                .unwrap()
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("No uninstallation is being tracked"))?
                .completed_app(&payload.app_id)?;
            let leftovers = scan_reviewable_app_leftovers(
                &app.display_name,
                &app.publisher,
                &app.install_location,
            );
            Ok(serde_json::json!({ "leftovers": leftovers }))
        }
        (&Method::Post, "/api/uninstaller/uninstall/dismiss") => {
            let payload: UninstallTargetReq = read_json_body(req)?;
            let mut current = state.uninstall.lock().unwrap();
            if let Some(job) = current.as_mut() {
                let status = job.status()?;
                if status.app_id == payload.app_id && status.state != UninstallState::Running {
                    *current = None;
                }
            }
            Ok(serde_json::json!({}))
        }
        _ => unknown_endpoint(path),
    }
}
