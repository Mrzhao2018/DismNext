use super::{read_json_body, unknown_endpoint};
use crate::state::AppState;
use anyhow::Result;
use dism_core::drivers::{default_backup_dir, delete_driver, export_drivers, list_drivers};
use serde::Deserialize;
use std::path::Path;
use tiny_http::{Method, Request};

#[derive(Deserialize)]
struct DriverExportReq {
    target_dir: String,
    published_names: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct DriverDeleteReq {
    published_names: Vec<String>,
}

pub(super) fn handle(req: &mut Request, path: &str, state: &AppState) -> Result<serde_json::Value> {
    match (req.method(), path) {
        (&Method::Get, "/api/drivers/list") => Ok(drivers_list_payload()),
        (&Method::Post, "/api/drivers/export") => {
            let payload: DriverExportReq = read_json_body(req)?;
            let report = export_drivers(
                Path::new(payload.target_dir.trim()),
                payload.published_names.as_deref(),
            )?;
            Ok(serde_json::to_value(report)?)
        }
        (&Method::Post, "/api/drivers/delete") => {
            let payload: DriverDeleteReq = read_json_body(req)?;
            if payload.published_names.is_empty() {
                return Err(anyhow::anyhow!("No drivers specified for deletion"));
            }
            let _operation = state.begin_cleanup_mutation()?;
            let mut deleted = Vec::new();
            let mut errors = Vec::new();
            for name in &payload.published_names {
                match delete_driver(name) {
                    Ok(_) => deleted.push(name.clone()),
                    Err(e) => errors.push(format!("{}: {}", name, e)),
                }
            }
            if deleted.is_empty() && !errors.is_empty() {
                return Err(anyhow::anyhow!("{}", errors.join("; ")));
            }
            let mut body = drivers_list_payload();
            if let Some(obj) = body.as_object_mut() {
                obj.insert("deleted".to_string(), serde_json::json!(deleted));
                obj.insert("errors".to_string(), serde_json::json!(errors));
            }
            Ok(body)
        }
        _ => unknown_endpoint(path),
    }
}

fn drivers_list_payload() -> serde_json::Value {
    let drivers = list_drivers();
    let total_bytes: u64 = drivers.iter().map(|d| d.size_bytes).sum();
    let outdated_count = drivers.iter().filter(|d| d.is_outdated_duplicate).count();
    let outdated_bytes: u64 = drivers
        .iter()
        .filter(|d| d.is_outdated_duplicate)
        .map(|d| d.size_bytes)
        .sum();
    serde_json::json!({
        "drivers": drivers,
        "total_bytes": total_bytes,
        "outdated_count": outdated_count,
        "outdated_bytes": outdated_bytes,
        "default_backup_dir": default_backup_dir(),
    })
}
