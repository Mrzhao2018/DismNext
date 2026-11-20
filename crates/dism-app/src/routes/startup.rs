use super::{read_json_body, unknown_endpoint};
use anyhow::Result;
use dism_core::startup::{
    inspect_startup_item, list_scheduled_tasks, list_startup_items, list_third_party_services,
    set_scheduled_task_enabled, set_service_start_mode, set_startup_enabled,
};
use serde::Deserialize;
use tiny_http::{Method, Request};

#[derive(Deserialize)]
struct StartupInspectReq {
    id: String,
}

#[derive(Deserialize)]
struct StartupToggleReq {
    id: String,
    enabled: bool,
}

#[derive(Deserialize)]
struct ServiceModeReq {
    name: String,
    start_mode: String,
}

#[derive(Deserialize)]
struct TaskToggleReq {
    path: String,
    enabled: bool,
}

pub(super) fn handle(req: &mut Request, path: &str) -> Result<serde_json::Value> {
    match (req.method(), path) {
        (&Method::Get, "/api/startup/list") => {
            let items = list_startup_items();
            Ok(serde_json::json!({ "items": items }))
        }
        (&Method::Post, "/api/startup/inspect") => {
            let payload: StartupInspectReq = read_json_body(req)?;
            Ok(serde_json::to_value(inspect_startup_item(&payload.id)?)?)
        }
        (&Method::Post, "/api/startup/toggle") => {
            let payload: StartupToggleReq = read_json_body(req)?;
            let item = set_startup_enabled(&payload.id, payload.enabled)?;
            let items = list_startup_items();
            Ok(serde_json::json!({ "item": item, "items": items }))
        }
        (&Method::Get, "/api/startup/services") => {
            let services = list_third_party_services();
            Ok(serde_json::json!({ "services": services }))
        }
        (&Method::Post, "/api/startup/services/mode") => {
            let payload: ServiceModeReq = read_json_body(req)?;
            let service = set_service_start_mode(&payload.name, &payload.start_mode)?;
            let services = list_third_party_services();
            Ok(serde_json::json!({ "service": service, "services": services }))
        }
        (&Method::Get, "/api/startup/tasks") => {
            let tasks = list_scheduled_tasks();
            Ok(serde_json::json!({ "tasks": tasks }))
        }
        (&Method::Post, "/api/startup/tasks/toggle") => {
            let payload: TaskToggleReq = read_json_body(req)?;
            set_scheduled_task_enabled(&payload.path, payload.enabled)?;
            let tasks = list_scheduled_tasks();
            Ok(serde_json::json!({ "tasks": tasks }))
        }
        _ => unknown_endpoint(path),
    }
}
