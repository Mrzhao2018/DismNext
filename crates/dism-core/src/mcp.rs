use crate::catalog::Catalog;
use crate::cleaner::{
    execute_cleanup, scan_all_with_progress, scan_rule_preview, CleanupLease,
    CleanupReviewUnavailable, CleanupReviews, ReviewedCleanup, CLEANUP_REVIEW_TTL,
};
use crate::diskmap::{
    analyze_path_with_options, find_duplicate_files_with_progress, find_orphaned_app_dirs,
};
use crate::drivers::{default_backup_dir, export_drivers, list_drivers};
use crate::features::{list_optional_features, FeatureCategory};
use crate::lockcheck::find_locking_processes;
use crate::optimizer::{evaluate_all_groups, get_optimization_presets};
use crate::report::generate_health_report;
use crate::rescue::{create_restore_point, list_restore_points, run_rescue_action};
use crate::scan_progress::ScanProgress;
use crate::snapshot::{snapshot_summary, SnapshotManager, SnapshotSummary};
use crate::startup::{list_scheduled_tasks, list_startup_items, list_third_party_services};
use crate::sysinfo::SystemInfo;
use crate::systasks::get_system_task_status;
use crate::uninstaller::{list_win32_apps, scan_app_leftovers};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
#[cfg(test)]
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

const SERVER_NAME: &str = "dismnext-mcp";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

mod schema;
mod transport;
pub use schema::tool_definitions;
pub use transport::run_mcp_stdio;

#[cfg(test)]
use transport::{read_next_message, FramingMode, MAX_MCP_BODY};

/// Processes a single parsed JSON-RPC 2.0 MCP message and returns an optional JSON-RPC response.
/// Notifications (messages without an `id` or explicit notification methods) return `None`.
pub fn handle_mcp_request(req: &Value, catalog: &Catalog) -> Option<Value> {
    handle_request(
        req,
        catalog,
        &mut CleanupReviews::default(),
        &ScanProgress::default(),
    )
}

fn handle_request(
    req: &Value,
    catalog: &Catalog,
    reviews: &mut CleanupReviews,
    progress: &ScanProgress,
) -> Option<Value> {
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let id_val = req.get("id");

    // JSON-RPC notifications do not receive a response.
    if id_val.is_none() || method.starts_with("notifications/") || method == "initialized" {
        return None;
    }

    let id = id_val.cloned().unwrap_or(Value::Null);
    let params = req.get("params").cloned().unwrap_or_else(|| json!({}));

    match method {
        "initialize" => {
            let client_proto = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(DEFAULT_PROTOCOL_VERSION);
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": client_proto,
                    "capabilities": {
                        "tools": {
                            "listChanged": false
                        }
                    },
                    "serverInfo": {
                        "name": SERVER_NAME,
                        "version": SERVER_VERSION
                    }
                }
            }))
        }
        "ping" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {}
        })),
        "resources/list" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "resources": []
            }
        })),
        "prompts/list" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "prompts": []
            }
        })),
        "tools/list" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": tool_definitions()
            }
        })),
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));

            match dispatch_tool_call(name, &args, catalog, reviews, progress) {
                Ok(payload) => {
                    let text = serde_json::to_string_pretty(&payload)
                        .unwrap_or_else(|_| payload.to_string());
                    Some(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [
                                {
                                    "type": "text",
                                    "text": text
                                }
                            ],
                            "isError": false
                        }
                    }))
                }
                Err(err) => Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": format!("Tool '{}' error: {}", name, err)
                            }
                        ],
                        "isError": true
                    }
                })),
            }
        }
        _ => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("Method not found: {}", method)
            }
        })),
    }
}

fn dispatch_tool_call(
    name: &str,
    args: &Value,
    catalog: &Catalog,
    reviews: &mut CleanupReviews,
    progress: &ScanProgress,
) -> Result<Value> {
    progress.check()?;
    let sys = SystemInfo::collect();

    match name {
        "dismnext_system_overview" => {
            let task_status = get_system_task_status();
            let opt_items: usize = catalog
                .optimization_groups
                .iter()
                .map(|g| g.items.len())
                .sum();
            Ok(json!({
                "system": sys,
                "catalog": {
                    "base_version": catalog.base_version,
                    "cleanup_rules": catalog.cleanup_rules.len(),
                    "optimization_groups": catalog.optimization_groups.len(),
                    "optimization_items": opt_items
                },
                "system_tasks": task_status
            }))
        }
        "dismnext_scan_cleanup" => {
            if let Some(rule_id) = args.get("rule_id").and_then(|v| v.as_str()) {
                let rule = catalog
                    .cleanup_rules
                    .iter()
                    .find(|r| r.id == rule_id)
                    .ok_or_else(|| anyhow!("Cleanup rule '{}' not found", rule_id))?;
                let res = scan_rule_preview(rule, &sys);
                Ok(serde_json::to_value(res)?)
            } else {
                let scan = scan_all_with_progress(&catalog.cleanup_rules, &sys, progress)?;
                Ok(json!({
                    "applicable_rule_count": scan.items.len(),
                    "total_cleanable_bytes": scan.summary.cleanable_bytes,
                    "total_skipped_locked_bytes": scan.summary.skipped_locked_bytes,
                    "total_skipped_protected_bytes": scan.summary.skipped_protected_bytes,
                    "rules": scan.items
                }))
            }
        }
        "dismnext_execute_cleanup" | "dismnext_review_cleanup" => {
            let rule_ids: Vec<String> = serde_json::from_value(args["rule_ids"].clone())?;
            anyhow::ensure!(
                !rule_ids.is_empty(),
                "'rule_ids' must contain at least one rule ID"
            );
            if name == "dismnext_review_cleanup" {
                let (plan, report) =
                    ReviewedCleanup::prepare(&catalog.cleanup_rules, &rule_ids, &sys);
                progress.check()?;
                let plan_id =
                    (!report.outcomes.is_empty()).then(|| reviews.store(plan, Instant::now()));
                return Ok(
                    json!({ "plan_id": plan_id, "expires_in_seconds": CLEANUP_REVIEW_TTL.as_secs(), "report": report }),
                );
            }
            anyhow::ensure!(
                args.get("dry_run").is_none_or(|value| value.as_bool() == Some(true)),
                "Review and confirm cleanup using dismnext_review_cleanup and dismnext_confirm_cleanup"
            );
            let report = execute_cleanup(&catalog.cleanup_rules, &rule_ids, &sys, true);
            Ok(serde_json::to_value(report)?)
        }
        "dismnext_confirm_cleanup" | "dismnext_discard_cleanup_review" => {
            let id = args["plan_id"]
                .as_str()
                .ok_or_else(|| anyhow!("Missing cleanup plan_id"))?;
            if name == "dismnext_discard_cleanup_review" {
                return Ok(json!({ "discarded": reviews.take(id, Instant::now()).is_some() }));
            }
            let lease = CleanupLease::try_acquire()?;
            progress.begin_recycling()?;
            let plan = reviews
                .take(id, Instant::now())
                .ok_or(CleanupReviewUnavailable)?;
            Ok(serde_json::to_value(plan.confirm(&lease))?)
        }
        "dismnext_analyze_disk" => {
            let target = args
                .get("path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(
                        std::env::var("LOCALAPPDATA").unwrap_or_else(|_| r"C:\".to_string()),
                    )
                });
            let max_depth = args
                .get("max_depth")
                .and_then(|v| v.as_u64())
                .unwrap_or(3)
                .clamp(1, 8) as usize;
            let measure_allocation = args
                .get("measure_allocation")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let report =
                analyze_path_with_options(&target, max_depth, measure_allocation, Some(progress))?;
            Ok(serde_json::to_value(report)?)
        }
        "dismnext_find_duplicates" => {
            let path_str = args
                .get("path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| anyhow!("Missing required string argument 'path'"))?;
            let min_size_bytes = args
                .get("min_size_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(1024 * 1024);
            let report =
                find_duplicate_files_with_progress(Path::new(path_str), min_size_bytes, progress)?;
            Ok(serde_json::to_value(report)?)
        }
        "dismnext_find_orphan_dirs" => {
            let orphans = find_orphaned_app_dirs();
            let total_bytes: u64 = orphans.iter().map(|o| o.size_bytes).sum();
            Ok(json!({
                "orphan_count": orphans.len(),
                "total_bytes": total_bytes,
                "items": orphans
            }))
        }
        "dismnext_inspect_file_locks" => {
            let path_str = args
                .get("path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| anyhow!("Missing required string argument 'path'"))?;
            let procs = find_locking_processes(Path::new(path_str));
            Ok(json!({
                "path": path_str,
                "locking_process_count": procs.len(),
                "processes": procs
            }))
        }
        "dismnext_list_drivers" => {
            let outdated_only = args
                .get("outdated_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mut drivers = list_drivers();
            if outdated_only {
                drivers.retain(|d| d.is_outdated_duplicate);
            }
            let total_bytes: u64 = drivers.iter().map(|d| d.size_bytes).sum();
            let outdated_count = drivers.iter().filter(|d| d.is_outdated_duplicate).count();
            Ok(json!({
                "count": drivers.len(),
                "outdated_count": outdated_count,
                "total_bytes": total_bytes,
                "drivers": drivers
            }))
        }
        "dismnext_backup_drivers" => {
            let target_dir = args
                .get("target_dir")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(default_backup_dir);
            let filter_names = args
                .get("published_names")
                .map(|value| serde_json::from_value::<Vec<String>>(value.clone()))
                .transpose()?;
            progress.begin_recycling()?;
            let report = export_drivers(Path::new(&target_dir), filter_names.as_deref())?;
            Ok(serde_json::to_value(report)?)
        }
        "dismnext_list_startup" => {
            let cat = args
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("all")
                .to_ascii_lowercase();
            let mut out = serde_json::Map::new();
            out.insert("category".to_string(), json!(cat));
            match cat.as_str() {
                "run" => {
                    out.insert(
                        "run_items".to_string(),
                        serde_json::to_value(list_startup_items())?,
                    );
                }
                "services" => {
                    out.insert(
                        "services".to_string(),
                        serde_json::to_value(list_third_party_services())?,
                    );
                }
                "tasks" => {
                    out.insert(
                        "scheduled_tasks".to_string(),
                        serde_json::to_value(list_scheduled_tasks())?,
                    );
                }
                _ => {
                    out.insert(
                        "run_items".to_string(),
                        serde_json::to_value(list_startup_items())?,
                    );
                    out.insert(
                        "services".to_string(),
                        serde_json::to_value(list_third_party_services())?,
                    );
                    out.insert(
                        "scheduled_tasks".to_string(),
                        serde_json::to_value(list_scheduled_tasks())?,
                    );
                }
            }
            Ok(Value::Object(out))
        }
        "dismnext_list_features" => {
            let mut features = list_optional_features();
            if let Some(cat) = args.get("category").and_then(|v| v.as_str()) {
                let target = cat.to_ascii_lowercase();
                features.retain(|f| match f.category {
                    FeatureCategory::Virtualization => target == "virtualization",
                    FeatureCategory::Developer => target == "developer",
                    FeatureCategory::SecurityAi => target == "security_ai" || target == "security",
                    FeatureCategory::NetworkSmb => target == "network_smb" || target == "network",
                    FeatureCategory::PrintMedia => target == "print_media" || target == "print",
                    FeatureCategory::IisWeb => target == "iis_web" || target == "iis",
                });
            }
            let enabled_count = features.iter().filter(|f| f.enabled).count();
            Ok(json!({
                "count": features.len(),
                "enabled_count": enabled_count,
                "features": features
            }))
        }
        "dismnext_list_optimizations" => {
            let mut groups = evaluate_all_groups(&catalog.optimization_groups, &sys);
            if let Some(search) = args
                .get("search")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
            {
                groups.retain_mut(|g| {
                    let group_match = g.name.zh.to_ascii_lowercase().contains(&search)
                        || g.name.en.to_ascii_lowercase().contains(&search)
                        || g.id.to_ascii_lowercase().contains(&search);
                    if !group_match {
                        g.items.retain(|it| {
                            it.name.zh.to_ascii_lowercase().contains(&search)
                                || it.name.en.to_ascii_lowercase().contains(&search)
                                || it.id.to_ascii_lowercase().contains(&search)
                        });
                    }
                    !g.items.is_empty()
                });
            }
            let include_presets = args
                .get("include_presets")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let item_count: usize = groups.iter().map(|g| g.items.len()).sum();
            let mut payload = json!({
                "group_count": groups.len(),
                "item_count": item_count,
                "groups": groups
            });
            if include_presets {
                payload["presets"] = serde_json::to_value(get_optimization_presets())?;
            }
            Ok(payload)
        }
        "dismnext_list_snapshots" => {
            let mut mgr = SnapshotManager::user_default();
            if let Some(legacy) = legacy_snapshot_dir() {
                mgr.add_legacy_directory(legacy);
            }
            let list = mgr.list()?;
            let summaries: Vec<SnapshotSummary> = list.iter().map(snapshot_summary).collect();
            Ok(json!({
                "snapshot_dir": mgr.directory().to_string_lossy(),
                "count": summaries.len(),
                "snapshots": summaries
            }))
        }
        "dismnext_system_rescue" => {
            let action = args
                .get("action")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("Missing required string argument 'action'"))?;
            match action {
                "list_restore_points" => {
                    let points = list_restore_points();
                    Ok(json!({
                        "count": points.len(),
                        "restore_points": points
                    }))
                }
                "create_restore_point" => {
                    let _lease = CleanupLease::try_acquire()?;
                    progress.begin_recycling()?;
                    let desc = args
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("DismNext MCP Restore Point");
                    let message = create_restore_point(desc)?;
                    Ok(json!({
                        "success": true,
                        "message": message
                    }))
                }
                other => {
                    let _lease = CleanupLease::try_acquire()?;
                    progress.begin_recycling()?;
                    let outcome = run_rescue_action(other)?;
                    Ok(serde_json::to_value(outcome)?)
                }
            }
        }
        "dismnext_list_installed_apps" => {
            let mut apps = list_win32_apps();
            if let Some(target_id) = args
                .get("scan_leftovers_for_id")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                let app = apps
                    .into_iter()
                    .find(|a| a.id == target_id)
                    .ok_or_else(|| anyhow!("Installed application '{}' not found", target_id))?;
                let leftovers =
                    scan_app_leftovers(&app.display_name, &app.publisher, &app.install_location);
                return Ok(json!({
                    "app": app,
                    "leftover_count": leftovers.len(),
                    "leftovers": leftovers
                }));
            }
            if let Some(search) = args
                .get("search")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
            {
                apps.retain(|a| {
                    a.display_name.to_ascii_lowercase().contains(&search)
                        || a.publisher.to_ascii_lowercase().contains(&search)
                });
            }
            Ok(json!({
                "count": apps.len(),
                "apps": apps
            }))
        }
        "dismnext_export_health_report" => {
            let fmt = args
                .get("format")
                .and_then(|v| v.as_str())
                .unwrap_or("json")
                .to_ascii_lowercase();
            let scan = scan_all_with_progress(&catalog.cleanup_rules, &sys, progress)?;
            progress.check()?;
            let report = generate_health_report(catalog, Some(&scan));
            if fmt == "markdown" {
                Ok(json!({
                    "health_score": report.health_score,
                    "generated_at": report.generated_at,
                    "markdown": report.markdown
                }))
            } else {
                Ok(serde_json::to_value(report)?)
            }
        }
        _ => Err(anyhow!("Unknown tool: {}", name)),
    }
}

fn legacy_snapshot_dir() -> Option<PathBuf> {
    if Path::new("data").join("snapshots").is_dir() {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .skip(1)
        .map(|ancestor| ancestor.join("data").join("snapshots"))
        .find(|candidate| candidate.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_content_type_before_content_length_without_losing_the_next_frame() {
        let raw = b"Content-Type: application/json\r\nContent-Length: 2\r\n\r\n{}\n{\"id\":2}\n";
        let mut input = &raw[..];
        let (first, framing) = read_next_message(&mut input).unwrap().unwrap();
        assert_eq!(first, "{}");
        assert!(matches!(framing, FramingMode::ContentLength));
        let (second, framing) = read_next_message(&mut input).unwrap().unwrap();
        assert_eq!(second, "{\"id\":2}");
        assert!(matches!(framing, FramingMode::Newline));
        assert!(read_next_message(&mut &b"Content-Type: application/json\r\n\r\n"[..]).is_err());
    }

    #[test]
    fn empty_driver_filter_requests_no_backup() {
        let error = dispatch_tool_call(
            "dismnext_backup_drivers",
            &json!({ "published_names": [] }),
            &test_catalog(),
            &mut CleanupReviews::default(),
            &ScanProgress::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("No driver packages selected"));
    }

    #[test]
    fn rejects_oversized_messages_in_both_framing_modes() {
        let header = format!("Content-Length: {}\r\n\r\n", MAX_MCP_BODY + 1);
        let framed = read_next_message(&mut header.as_bytes());
        assert!(framed
            .err()
            .unwrap()
            .to_string()
            .contains("MCP message exceeds"));

        let mut line = BufReader::new(std::io::repeat(b'x').take(MAX_MCP_BODY as u64 + 1));
        let unframed = read_next_message(&mut line);
        assert!(unframed
            .err()
            .unwrap()
            .to_string()
            .contains("MCP message exceeds"));
    }

    fn test_catalog() -> Catalog {
        Catalog::load(None).expect("embedded catalog should load")
    }

    #[test]
    fn mcp_initialize_and_notifications() {
        let catalog = test_catalog();
        let init_req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "clientInfo": { "name": "unit-test", "version": "1.0" }
            }
        });
        let resp = handle_mcp_request(&init_req, &catalog).expect("initialize returns response");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(resp["result"]["serverInfo"]["name"], "dismnext-mcp");
        assert_eq!(
            resp["result"]["serverInfo"]["version"],
            env!("CARGO_PKG_VERSION")
        );

        let notif = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        assert!(handle_mcp_request(&notif, &catalog).is_none());
    }

    #[test]
    fn mcp_tools_list_contains_all_tools_with_valid_schema() {
        let catalog = test_catalog();
        let req = json!({
            "jsonrpc": "2.0",
            "id": "list-1",
            "method": "tools/list",
            "params": {}
        });
        let resp = handle_mcp_request(&req, &catalog).expect("tools/list returns response");
        let tools = resp["result"]["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 19);
        assert!(tools
            .iter()
            .any(|tool| tool["name"] == "dismnext_confirm_cleanup"));
        for t in tools {
            assert!(t["name"].as_str().unwrap().starts_with("dismnext_"));
            assert_eq!(t["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn mcp_tools_call_overview_and_snapshots() {
        let catalog = test_catalog();

        let overview_req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "dismnext_system_overview",
                "arguments": {}
            }
        });
        let resp = handle_mcp_request(&overview_req, &catalog).expect("overview response");
        assert_eq!(resp["result"]["isError"], false);
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .expect("content text");
        let parsed: Value = serde_json::from_str(text).expect("valid JSON tool payload");
        assert!(parsed["catalog"]["cleanup_rules"].as_u64().unwrap() >= 93);
        assert!(
            parsed["system"]["system_disk"]["total_bytes"]
                .as_u64()
                .unwrap()
                > 0
        );

        let snap_req = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "dismnext_list_snapshots",
                "arguments": {}
            }
        });
        let snap_resp = handle_mcp_request(&snap_req, &catalog).expect("snapshots response");
        assert_eq!(snap_resp["result"]["isError"], false);

        let unknown_req = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "dismnext_nonexistent_tool",
                "arguments": {}
            }
        });
        let err_resp = handle_mcp_request(&unknown_req, &catalog).expect("error response");
        assert_eq!(err_resp["result"]["isError"], true);
    }
}
