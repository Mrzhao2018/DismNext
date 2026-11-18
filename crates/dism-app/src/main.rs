use anyhow::Result;
use clap::Parser;
use dism_core::appx::{list_appx_packages, remove_appx_package};
use dism_core::cleaner::{
    execute_cleanup, is_deferred_system_task_proc, scan_all, scan_custom_proc_name, scan_rule,
    scan_rule_preview, CleanupScanResult,
};
use dism_core::diskmap::{
    analyze_path, find_duplicate_files, find_orphaned_app_dirs, get_presets, recycle_path,
};
use dism_core::drivers::{default_backup_dir, delete_driver, export_drivers, list_drivers};
use dism_core::evaluator::eval_condition;
use dism_core::features::{list_optional_features, set_feature_enabled};
use dism_core::lockcheck::{find_locking_processes, terminate_locking_process};
use dism_core::optimizer::{
    apply_optimization, apply_optimization_batch, evaluate_all_groups, export_optimization_profile,
    get_optimization_presets, ApplyValue, OptimizationProfileEntry, ScopeName,
};
use dism_core::rescue::{create_restore_point, list_restore_points, run_rescue_action};
use dism_core::startup::{
    list_scheduled_tasks, list_startup_items, list_third_party_services,
    set_scheduled_task_enabled, set_service_start_mode, set_startup_enabled,
};
use dism_core::sysinfo::relaunch_as_admin;
use dism_core::systasks::{get_system_task_status, run_system_task};
use dism_core::{Catalog, SnapshotManager, SystemInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, RwLock};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

#[derive(Parser)]
#[command(name = "dism-app", about = "DismNext Desktop Host Server & GUI Launcher")]
struct Args {
    #[arg(long, default_value_t = 9527)]
    port: u16,

    #[arg(long)]
    no_window: bool,

    #[arg(long)]
    catalog: Option<PathBuf>,

    #[arg(long)]
    ui_dir: Option<PathBuf>,
}

struct AppState {
    port: u16,
    catalog: Catalog,
    snap_mgr: SnapshotManager,
    scan_cache: RwLock<HashMap<String, CleanupScanResult>>,
    ui_dir: Option<PathBuf>,
}

#[derive(Deserialize)]
struct ExecuteReq {
    rule_ids: Vec<String>,
    dry_run: bool,
}

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
struct DiskAnalyzeReq {
    path: String,
    max_depth: Option<usize>,
}

#[derive(Deserialize)]
struct RevealReq {
    path: String,
}

#[derive(Deserialize)]
struct StartupToggleReq {
    id: String,
    enabled: bool,
}

#[derive(Deserialize)]
struct AppxRemoveReq {
    package_full_name: String,
}

#[derive(Deserialize)]
struct DriverExportReq {
    target_dir: String,
    published_names: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct DriverDeleteReq {
    published_names: Vec<String>,
}

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
struct TerminatePidReq {
    pid: u32,
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

#[derive(Deserialize)]
struct OptBatchReq {
    entries: Vec<OptimizationProfileEntry>,
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
struct DuplicatesReq {
    path: String,
    min_size_bytes: Option<u64>,
}

#[derive(Serialize)]
struct ErrorPayload {
    error: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let catalog = Catalog::load(args.catalog.as_deref())?;
    let snap_mgr = SnapshotManager::new(SnapshotManager::default_dir());
    let ui_dir = resolve_ui_dir(args.ui_dir);

    let state = Arc::new(AppState {
        port: args.port,
        catalog,
        snap_mgr,
        scan_cache: RwLock::new(HashMap::new()),
        ui_dir,
    });

    let addr = format!("127.0.0.1:{}", args.port);
    let server = bind_with_retry(&addr)?;
    let url = format!("http://{}", addr);
    println!("DismNext server listening on {}", url);
    if let Some(ref d) = state.ui_dir {
        println!("Serving UI from {}", d.display());
    } else {
        println!("UI dist directory not found; serving API endpoints only.");
    }

    if !args.no_window {
        launch_desktop_window(&url);
    }

    // Pre-warm static system metadata, driver cache, system task status, and scheduled tasks
    std::thread::spawn(|| {
        let _ = SystemInfo::collect();
        let _ = list_drivers();
        let _ = get_system_task_status();
        let _ = list_scheduled_tasks();
    });

    for req in server.incoming_requests() {
        let st = Arc::clone(&state);
        std::thread::spawn(move || {
            handle_request(req, &st);
        });
    }

    Ok(())
}

fn bind_with_retry(addr: &str) -> Result<Server> {
    let mut last_err = String::new();
    for attempt in 0..20 {
        match Server::http(addr) {
            Ok(srv) => return Ok(srv),
            Err(e) => {
                last_err = e.to_string();
                if attempt < 19 {
                    std::thread::sleep(std::time::Duration::from_millis(150));
                }
            }
        }
    }
    Err(anyhow::anyhow!("Failed to bind {}: {}", addr, last_err))
}

fn resolve_ui_dir(explicit: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        if p.join("index.html").exists() {
            return Some(p);
        }
    }
    let mut candidates = vec![PathBuf::from("ui/dist")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("ui/dist"));
            candidates.push(dir.join("../../ui/dist"));
            candidates.push(dir.join("../../../ui/dist"));
        }
    }
    candidates.into_iter().find(|p| p.join("index.html").exists())
}

fn launch_desktop_window(url: &str) {
    let edge_candidates = [
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ];
    for edge in edge_candidates {
        if Path::new(edge).exists() {
            let _ = Command::new(edge)
                .arg(format!("--app={}", url))
                .arg("--window-size=1320,860")
                .spawn();
            return;
        }
    }
    let _ = Command::new("cmd").args(["/C", "start", "", url]).spawn();
}

fn handle_request(mut req: Request, state: &AppState) {
    let origin_header = header_value(&req, "origin");
    if let Err(reason) = verify_request_security(&req, state.port) {
        let mut resp = Response::from_string(
            serde_json::json!({ "error": reason }).to_string(),
        )
        .with_status_code(StatusCode(403));
        if let Ok(h) = Header::from_bytes("Content-Type", "application/json; charset=utf-8") {
            resp.add_header(h);
        }
        let _ = req.respond(resp);
        return;
    }

    if req.method() == &Method::Options {
        let mut resp = Response::empty(StatusCode(204));
        add_cors_headers(&mut resp, state.port, origin_header.as_deref());
        let _ = req.respond(resp);
        return;
    }

    let raw_url = req.url().to_string();
    let (path, query) = match raw_url.split_once('?') {
        Some((p, q)) => (p, q),
        None => (raw_url.as_str(), ""),
    };

    if path.starts_with("/api/") {
        let result = dispatch_api(&mut req, path, query, state);
        match result {
            Ok(json_val) => respond_json(
                req,
                StatusCode(200),
                &json_val,
                state.port,
                origin_header.as_deref(),
            ),
            Err(err) => {
                let body = ErrorPayload {
                    error: err.to_string(),
                };
                respond_json(
                    req,
                    StatusCode(400),
                    &body,
                    state.port,
                    origin_header.as_deref(),
                );
            }
        }
        return;
    }

    if let Some(ref ui_dir) = state.ui_dir {
        serve_static(req, ui_dir, path);
    } else {
        let mut resp = Response::from_string(
            "DismNext API Server is running. Run `npm run build` in ui/ to enable GUI.",
        )
        .with_status_code(StatusCode(200));
        add_cors_headers(&mut resp, state.port, origin_header.as_deref());
        let _ = req.respond(resp);
    }
}

fn dispatch_api(
    req: &mut Request,
    path: &str,
    query: &str,
    state: &AppState,
) -> Result<serde_json::Value> {
    let sys = SystemInfo::collect();

    match (req.method(), path) {
        (&Method::Get, "/api/sysinfo") => Ok(serde_json::json!({
            "system": sys,
            "catalog": {
                "base_version": state.catalog.base_version,
                "cleanup_rules_total": state.catalog.cleanup_rules.len(),
                "optimization_groups_total": state.catalog.optimization_groups.len(),
                "optimization_items_total": state.catalog.optimization_groups.iter().map(|g| g.items.len()).sum::<usize>(),
            }
        })),
        (&Method::Post, "/api/sys/elevate") => {
            if sys.is_admin {
                return Ok(serde_json::json!({ "already_admin": true, "relaunching": false }));
            }
            let mut extra_args = vec![
                "--port".to_string(),
                state.port.to_string(),
                "--no-window".to_string(),
            ];
            if let Some(ref ui) = state.ui_dir {
                extra_args.push("--ui-dir".to_string());
                extra_args.push(ui.to_string_lossy().into_owned());
            }
            relaunch_as_admin(&extra_args)?;
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(200));
                std::process::exit(0);
            });
            Ok(serde_json::json!({ "already_admin": false, "relaunching": true }))
        }
        (&Method::Get, "/api/cleanup/rules") => {
            let cache = state.scan_cache.read().unwrap();
            let mut list = Vec::new();
            for rule in &state.catalog.cleanup_rules {
                if let Some(cached) = cache.get(&rule.id) {
                    list.push(serde_json::json!({
                        "scanned": true,
                        "rule": cached,
                    }));
                } else {
                    let active_scan = rule
                        .scans
                        .iter()
                        .find(|s| eval_condition(&s.applicable, &sys));
                    if let Some(scan) = active_scan {
                        let system_task_proc = scan_custom_proc_name(scan);
                        let is_sys_task = system_task_proc
                            .as_deref()
                            .map(is_deferred_system_task_proc)
                            .unwrap_or(false);
                        let stub = CleanupScanResult {
                            id: rule.id.clone(),
                            name: scan.name_override.clone().unwrap_or_else(|| rule.name.clone()),
                            group: rule.group.clone(),
                            description: scan
                                .description_override
                                .clone()
                                .unwrap_or_else(|| rule.description.clone()),
                            warning: scan
                                .warning_override
                                .clone()
                                .or_else(|| rule.warning.clone()),
                            level: rule.level,
                            source: rule.source.clone(),
                            applicable: true,
                            is_system_task: is_sys_task,
                            system_task_proc,
                            system_task_hint: None,
                            cleanable_bytes: 0,
                            cleanable_count: 0,
                            skipped_locked_bytes: 0,
                            skipped_locked_count: 0,
                            skipped_protected_bytes: 0,
                            skipped_protected_count: 0,
                            preview_files: Vec::new(),
                        };
                        list.push(serde_json::json!({
                            "scanned": false,
                            "rule": stub,
                        }));
                    }
                }
            }
            Ok(serde_json::json!({ "items": list }))
        }
        (&Method::Post, "/api/cleanup/scan") => {
            let results = scan_all(&state.catalog.cleanup_rules, &sys);
            {
                let mut cache = state.scan_cache.write().unwrap();
                cache.clear();
                for r in &results {
                    cache.insert(r.id.clone(), r.clone());
                }
            }
            Ok(serde_json::json!({ "items": results }))
        }
        (&Method::Get, "/api/cleanup/preview") => {
            let id = parse_query_param(query, "id")
                .ok_or_else(|| anyhow::anyhow!("Missing ?id= parameter"))?;
            let rule = state
                .catalog
                .cleanup_rules
                .iter()
                .find(|r| r.id == id)
                .ok_or_else(|| anyhow::anyhow!("Rule '{}' not found", id))?;
            let res = scan_rule_preview(rule, &sys);
            {
                let mut cache = state.scan_cache.write().unwrap();
                cache.insert(res.id.clone(), res.clone());
            }
            Ok(serde_json::to_value(res)?)
        }
        (&Method::Post, "/api/cleanup/execute") => {
            let payload: ExecuteReq = read_json_body(req)?;
            let report = execute_cleanup(
                &state.catalog.cleanup_rules,
                &payload.rule_ids,
                &sys,
                payload.dry_run,
            );
            if !payload.dry_run {
                let mut cache = state.scan_cache.write().unwrap();
                for id in &payload.rule_ids {
                    if let Some(rule) = state.catalog.cleanup_rules.iter().find(|r| r.id == *id) {
                        cache.insert(id.clone(), scan_rule(rule, &sys));
                    }
                }
            }
            let updated_sys = SystemInfo::collect();
            Ok(serde_json::json!({
                "report": report,
                "system": updated_sys,
            }))
        }
        (&Method::Get, "/api/optimizer/groups") => {
            let groups = evaluate_all_groups(&state.catalog.optimization_groups, &sys);
            Ok(serde_json::json!({ "groups": groups }))
        }
        (&Method::Post, "/api/optimizer/apply") => {
            let payload: OptApplyReq = read_json_body(req)?;
            let outcome = apply_optimization(
                &state.catalog.optimization_groups,
                &payload.item_id,
                payload.scope,
                payload.value,
                &state.snap_mgr,
            )?;
            let refreshed = evaluate_all_groups(&state.catalog.optimization_groups, &sys);
            Ok(serde_json::json!({
                "outcome": outcome,
                "groups": refreshed,
            }))
        }
        (&Method::Get, "/api/snapshots") => {
            let list = state.snap_mgr.list()?;
            Ok(serde_json::json!({ "snapshots": list }))
        }
        (&Method::Post, "/api/snapshots/restore") => {
            let payload: RestoreReq = read_json_body(req)?;
            let restored = state.snap_mgr.restore(&payload.snapshot_id)?;
            let refreshed_groups = evaluate_all_groups(&state.catalog.optimization_groups, &sys);
            Ok(serde_json::json!({
                "restored": restored,
                "groups": refreshed_groups,
            }))
        }
        (&Method::Get, "/api/diskmap/presets") => {
            let presets = get_presets(&sys);
            Ok(serde_json::json!({ "presets": presets }))
        }
        (&Method::Post, "/api/diskmap/analyze") => {
            let payload: DiskAnalyzeReq = read_json_body(req)?;
            let report = analyze_path(
                Path::new(payload.path.trim()),
                payload.max_depth.unwrap_or(4),
            )?;
            Ok(serde_json::to_value(report)?)
        }
        (&Method::Post, "/api/diskmap/reveal") => {
            let payload: RevealReq = read_json_body(req)?;
            let target = Path::new(payload.path.trim());
            if !target.exists() {
                return Err(anyhow::anyhow!("Path does not exist: {}", payload.path));
            }
            let _ = Command::new("explorer.exe")
                .arg(format!("/select,{}", target.to_string_lossy()))
                .spawn();
            Ok(serde_json::json!({ "ok": true }))
        }
        (&Method::Post, "/api/diskmap/recycle") => {
            let payload: RevealReq = read_json_body(req)?;
            let freed = recycle_path(Path::new(payload.path.trim()))?;
            let updated_sys = SystemInfo::collect();
            Ok(serde_json::json!({
                "freed_bytes": freed,
                "system": updated_sys,
            }))
        }
        (&Method::Get, "/api/startup/list") => {
            let items = list_startup_items();
            Ok(serde_json::json!({ "items": items }))
        }
        (&Method::Post, "/api/startup/toggle") => {
            let payload: StartupToggleReq = read_json_body(req)?;
            let item = set_startup_enabled(&payload.id, payload.enabled)?;
            let items = list_startup_items();
            Ok(serde_json::json!({ "item": item, "items": items }))
        }
        (&Method::Get, "/api/appx/list") => {
            let packages = list_appx_packages();
            Ok(serde_json::json!({ "packages": packages }))
        }
        (&Method::Post, "/api/appx/remove") => {
            let payload: AppxRemoveReq = read_json_body(req)?;
            remove_appx_package(&payload.package_full_name)?;
            let packages = list_appx_packages();
            Ok(serde_json::json!({
                "removed": payload.package_full_name,
                "packages": packages,
            }))
        }
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
        (&Method::Get, "/api/features/list") => {
            let features = list_optional_features();
            Ok(serde_json::json!({ "features": features }))
        }
        (&Method::Post, "/api/features/toggle") => {
            let payload: FeatureToggleReq = read_json_body(req)?;
            let outcome = set_feature_enabled(&payload.name, payload.enabled)?;
            let features = list_optional_features();
            Ok(serde_json::json!({
                "outcome": outcome,
                "features": features,
            }))
        }
        (&Method::Get, "/api/systasks/status") => {
            let status = get_system_task_status();
            Ok(serde_json::to_value(status)?)
        }
        (&Method::Post, "/api/systasks/run") => {
            let payload: SysTaskRunReq = read_json_body(req)?;
            let outcome = run_system_task(payload.task_id.trim())?;
            let updated_sys = SystemInfo::collect();
            Ok(serde_json::json!({
                "outcome": outcome,
                "system": updated_sys,
            }))
        }
        (&Method::Post, "/api/lockcheck/inspect") => {
            let payload: RevealReq = read_json_body(req)?;
            let processes = find_locking_processes(Path::new(payload.path.trim()));
            Ok(serde_json::json!({ "processes": processes }))
        }
        (&Method::Post, "/api/lockcheck/terminate") => {
            let payload: TerminatePidReq = read_json_body(req)?;
            let message = terminate_locking_process(payload.pid)?;
            Ok(serde_json::json!({ "ok": true, "message": message }))
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
        (&Method::Get, "/api/optimizer/presets") => {
            let presets = get_optimization_presets();
            Ok(serde_json::json!({ "presets": presets }))
        }
        (&Method::Post, "/api/optimizer/batch") => {
            let payload: OptBatchReq = read_json_body(req)?;
            let outcome = apply_optimization_batch(
                &state.catalog.optimization_groups,
                &payload.entries,
                &state.snap_mgr,
            )?;
            let groups = evaluate_all_groups(&state.catalog.optimization_groups, &sys);
            Ok(serde_json::json!({
                "outcome": outcome,
                "groups": groups,
            }))
        }
        (&Method::Get, "/api/optimizer/profile/export") => {
            let profile = export_optimization_profile(&state.catalog.optimization_groups, &sys);
            Ok(serde_json::to_value(profile)?)
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
            let outcome = run_rescue_action(&payload.action_id)?;
            Ok(serde_json::to_value(outcome)?)
        }
        (&Method::Post, "/api/diskmap/duplicates") => {
            let payload: DuplicatesReq = read_json_body(req)?;
            let report = find_duplicate_files(
                Path::new(payload.path.trim()),
                payload.min_size_bytes.unwrap_or(512 * 1024),
            )?;
            Ok(serde_json::to_value(report)?)
        }
        (&Method::Get, "/api/diskmap/orphans") => {
            let orphans = find_orphaned_app_dirs();
            Ok(serde_json::json!({ "orphans": orphans }))
        }
        _ => Err(anyhow::anyhow!("Unknown API endpoint: {}", path)),
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

fn serve_static(req: Request, ui_dir: &Path, url_path: &str) {
    let rel = url_path.trim_start_matches('/');
    let rel_clean = if rel.is_empty() { "index.html" } else { rel };
    // Reject path traversal
    if rel_clean.contains("..") {
        let _ = req.respond(Response::empty(StatusCode(403)));
        return;
    }

    let mut target = ui_dir.join(rel_clean);
    if !target.exists() || target.is_dir() {
        target = ui_dir.join("index.html");
    }

    match fs::read(&target) {
        Ok(bytes) => {
            let mime = content_type_for(&target);
            let mut resp = Response::from_data(bytes).with_status_code(StatusCode(200));
            if let Ok(h) = Header::from_bytes("Content-Type", mime) {
                resp.add_header(h);
            }
            let _ = req.respond(resp);
        }
        Err(_) => {
            let _ = req.respond(Response::empty(StatusCode(404)));
        }
    }
}

fn content_type_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn read_json_body<T: serde::de::DeserializeOwned>(req: &mut Request) -> Result<T> {
    let mut body = String::new();
    req.as_reader().read_to_string(&mut body)?;
    Ok(serde_json::from_str(&body)?)
}

fn parse_query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn respond_json<T: Serialize>(
    req: Request,
    status: StatusCode,
    val: &T,
    port: u16,
    origin: Option<&str>,
) {
    let text = serde_json::to_string(val).unwrap_or_else(|_| "{}".to_string());
    let mut resp = Response::from_string(text).with_status_code(status);
    if let Ok(h) = Header::from_bytes("Content-Type", "application/json; charset=utf-8") {
        resp.add_header(h);
    }
    add_cors_headers(&mut resp, port, origin);
    let _ = req.respond(resp);
}

fn header_value(req: &Request, name_lower: &str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name_lower))
        .map(|h| h.value.as_str().trim().to_string())
}

fn is_allowed_host(host: &str, port: u16) -> bool {
    let lower = host.trim().to_ascii_lowercase();
    lower == format!("127.0.0.1:{}", port)
        || lower == format!("localhost:{}", port)
        || lower == "127.0.0.1"
        || lower == "localhost"
}

fn is_allowed_origin(origin: &str, port: u16) -> bool {
    let lower = origin.trim().trim_end_matches('/').to_ascii_lowercase();
    lower == format!("http://127.0.0.1:{}", port)
        || lower == format!("http://localhost:{}", port)
        || lower == "http://127.0.0.1:5173"
        || lower == "http://localhost:5173"
}

fn verify_request_security(req: &Request, port: u16) -> std::result::Result<(), String> {
    if let Some(host) = header_value(req, "host") {
        if !is_allowed_host(&host, port) {
            return Err(format!("Blocked untrusted Host header: {}", host));
        }
    }
    if let Some(origin) = header_value(req, "origin") {
        if !is_allowed_origin(&origin, port) {
            return Err(format!("Blocked untrusted Origin header: {}", origin));
        }
    }
    if req.method() == &Method::Post && req.url().starts_with("/api/") {
        let token_ok = header_value(req, "x-dismnext-client")
            .map(|v| v == "1")
            .unwrap_or(false);
        if !token_ok {
            return Err("Missing required X-DismNext-Client CSRF protection header".to_string());
        }
    }
    Ok(())
}

fn add_cors_headers<R: std::io::Read>(resp: &mut Response<R>, port: u16, origin: Option<&str>) {
    let allowed_origin = match origin {
        Some(o) if is_allowed_origin(o, port) => o.to_string(),
        _ => format!("http://127.0.0.1:{}", port),
    };
    if let Ok(h) = Header::from_bytes("Access-Control-Allow-Origin", allowed_origin.as_bytes()) {
        resp.add_header(h);
    }
    if let Ok(h) = Header::from_bytes("Vary", "Origin") {
        resp.add_header(h);
    }
    if let Ok(h) = Header::from_bytes("Access-Control-Allow-Methods", "GET, POST, OPTIONS") {
        resp.add_header(h);
    }
    if let Ok(h) = Header::from_bytes(
        "Access-Control-Allow-Headers",
        "Content-Type, X-DismNext-Client",
    ) {
        resp.add_header(h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_and_origin_security_checks() {
        assert!(is_allowed_host("127.0.0.1:9527", 9527));
        assert!(is_allowed_host("localhost:9527", 9527));
        assert!(!is_allowed_host("evil.example.com:9527", 9527));
        assert!(!is_allowed_host("127.0.0.1:9999", 9527));

        assert!(is_allowed_origin("http://127.0.0.1:9527", 9527));
        assert!(is_allowed_origin("http://localhost:9527/", 9527));
        assert!(!is_allowed_origin("https://evil.example.com", 9527));
        assert!(!is_allowed_origin("null", 9527));
    }
}
