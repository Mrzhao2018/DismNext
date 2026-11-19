use anyhow::Result;
use clap::Parser;
use dism_core::appx::{list_appx_packages, remove_appx_package};
use dism_core::cleaner::{
    execute_cleanup, is_deferred_system_task_proc, scan_all_with_progress, scan_all_with_summary, scan_custom_proc_name,
    scan_rule_preview, CleanupBatchScan, CleanupScanResult,
};
use dism_core::diskmap::{
    analyze_path, analyze_path_with_progress, find_duplicate_files,
    find_duplicate_files_with_progress, find_orphaned_app_dirs, get_presets, recycle_path,
};
use dism_core::drivers::{default_backup_dir, delete_driver, export_drivers, list_drivers};
use dism_core::duplicate_recycle::{recycle_duplicates, DuplicateRecycleTarget};
use dism_core::evaluator::eval_condition;
use dism_core::features::{list_optional_features, set_feature_enabled};
use dism_core::lockcheck::{find_locking_processes, terminate_locking_process};
use dism_core::optimizer::{
    apply_optimization, apply_optimization_batch, evaluate_all_groups, export_optimization_profile,
    get_optimization_presets, ApplyValue, OptimizationProfileEntry, ScopeName,
};
use dism_core::report::generate_health_report;
use dism_core::rescue::{create_restore_point, list_restore_points, run_rescue_action};
use dism_core::startup::{
    list_scheduled_tasks, list_startup_items, list_third_party_services,
    set_scheduled_task_enabled, set_service_start_mode, set_startup_enabled,
};
use dism_core::sysinfo::relaunch_as_admin;
use dism_core::systasks::{get_system_task_status, run_system_task};
use dism_core::uninstaller::{
    launch_win32_uninstaller, list_win32_apps, scan_app_leftovers, UninstallState, Win32Uninstall,
};
use dism_core::{Catalog, CustomRuleInput, SnapshotManager, SystemInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

include!(concat!(env!("OUT_DIR"), "/embedded_ui.rs"));

mod scan_jobs;
use scan_jobs::{ScanBusy, ScanJobs};

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
    catalog: RwLock<Catalog>,
    snap_mgr: SnapshotManager,
    scan_cache: RwLock<CleanupScanCache>,
    cleanup_operation: Mutex<()>,
    scan_jobs: Arc<ScanJobs>,
    uninstall: Mutex<Option<Win32Uninstall>>,
    ui_dir: Option<PathBuf>,
}

const FULL_SCAN_TTL: Duration = Duration::from_secs(60);

#[derive(Default)]
struct CleanupScanCache {
    rules: HashMap<String, CleanupScanResult>,
    full_scan: Option<(Instant, Arc<CleanupBatchScan>)>,
}

impl CleanupScanCache {
    fn store_full_scan(
        &mut self,
        results: CleanupBatchScan,
        started: Instant,
    ) -> Arc<CleanupBatchScan> {
        let results = Arc::new(results);
        self.full_scan = Some((started, Arc::clone(&results)));
        results
    }

    fn fresh_full_scan(&self, now: Instant) -> Option<Arc<CleanupBatchScan>> {
        let (started, results) = self.full_scan.as_ref()?;
        (now.saturating_duration_since(*started) < FULL_SCAN_TTL).then(|| Arc::clone(results))
    }
}

impl AppState {
    fn begin_cleanup_mutation(&self) -> MutexGuard<'_, ()> {
        let operation = self.cleanup_operation.lock().unwrap();
        *self.scan_cache.write().unwrap() = CleanupScanCache::default();
        operation
    }

    fn publish_cleanup_scan(&self, results: CleanupBatchScan, started: Instant) -> serde_json::Value {
        let results = {
            let mut cache = self.scan_cache.write().unwrap();
            cache.rules.clear();
            cache.store_full_scan(results, started)
        };
        serde_json::json!({ "items": results.items, "summary": results.summary })
    }
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
#[serde(tag = "kind", rename_all = "snake_case")]
enum ScanRequest {
    DiskAnalysis { path: String, max_depth: Option<usize> },
    DuplicateFiles { path: String, min_size_bytes: Option<u64> },
    CleanupRules,
}

#[derive(Deserialize)]
struct CancelScanReq {
    id: u64,
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

#[derive(Deserialize)]
struct RecycleDuplicatesReq {
    keeper: DuplicateRecycleTarget,
    candidates: Vec<DuplicateRecycleTarget>,
}

#[derive(Deserialize)]
struct LeftoversReq {
    display_name: String,
    publisher: String,
    install_location: String,
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

#[derive(Deserialize)]
struct DeleteCustomRuleReq {
    rule_id: String,
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
        catalog: RwLock::new(catalog),
        snap_mgr,
        scan_cache: RwLock::new(CleanupScanCache::default()),
        cleanup_operation: Mutex::new(()),
        scan_jobs: Arc::new(ScanJobs::default()),
        uninstall: Mutex::new(None),
        ui_dir,
    });

    let addr = format!("127.0.0.1:{}", args.port);
    let server = bind_with_retry(&addr)?;
    let url = format!("http://{}", addr);
    println!("DismNext server listening on {}", url);
    if let Some(ref d) = state.ui_dir {
        println!("Serving external UI from {}", d.display());
    } else if !EMBEDDED_UI_ASSETS.is_empty() {
        println!(
            "Serving embedded portable UI ({} assets compiled into binary)",
            EMBEDDED_UI_ASSETS.len()
        );
    } else {
        println!("UI assets not found; serving API endpoints only.");
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
    if !EMBEDDED_UI_ASSETS.is_empty() {
        return None;
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

fn handle_request(mut req: Request, state: &Arc<AppState>) {
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
                let code = if err.is::<ScanBusy>() { 409 } else { 400 };
                let body = ErrorPayload {
                    error: err.to_string(),
                };
                respond_json(
                    req,
                    StatusCode(code),
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
    } else if !EMBEDDED_UI_ASSETS.is_empty() {
        serve_embedded(req, path);
    } else {
        let mut resp = Response::from_string(
            "DismNext API Server is running. Run `npm run build` in ui/ to enable GUI.",
        )
        .with_status_code(StatusCode(200));
        add_cors_headers(&mut resp, state.port, origin_header.as_deref());
        let _ = req.respond(resp);
    }
}

fn cleanup_rules_stub_list(
    catalog: &Catalog,
    cache: &HashMap<String, CleanupScanResult>,
    sys: &SystemInfo,
) -> Vec<serde_json::Value> {
    let mut list = Vec::new();
    for rule in &catalog.cleanup_rules {
        if let Some(cached) = cache.get(&rule.id) {
            list.push(serde_json::json!({
                "scanned": true,
                "rule": cached,
            }));
        } else {
            let active_scan = rule
                .scans
                .iter()
                .find(|s| eval_condition(&s.applicable, sys));
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
    list
}

fn dispatch_api(
    req: &mut Request,
    path: &str,
    query: &str,
    state: &Arc<AppState>,
) -> Result<serde_json::Value> {
    match (req.method(), path) {
        (&Method::Post, "/api/scans/start") => {
            let payload: ScanRequest = read_json_body(req)?;
            let app = Arc::clone(state);
            let id = state.scan_jobs.start(move |progress| match payload {
                ScanRequest::DiskAnalysis { path, max_depth } => {
                    let report = analyze_path_with_progress(Path::new(path.trim()), max_depth.unwrap_or(4), progress)?;
                    Ok(serde_json::to_value(report)?)
                }
                ScanRequest::DuplicateFiles { path, min_size_bytes } => {
                    let report = find_duplicate_files_with_progress(Path::new(path.trim()), min_size_bytes.unwrap_or(512 * 1024), progress)?;
                    Ok(serde_json::to_value(report)?)
                }
                ScanRequest::CleanupRules => {
                    let _cleanup = app.cleanup_operation.try_lock()
                        .map_err(|_| anyhow::anyhow!("Another cleanup operation is already running"))?;
                    let started = Instant::now();
                    let sys = SystemInfo::collect();
                    let results = scan_all_with_progress(&app.catalog.read().unwrap().cleanup_rules, &sys, progress)?;
                    if progress.is_cancelled() { return Err(dism_core::scan_progress::ScanCancelled.into()); }
                    Ok(app.publish_cleanup_scan(results, started))
                }
            })?;
            return Ok(serde_json::json!({ "id": id }));
        }
        (&Method::Get, "/api/scans/status") => {
            let id = parse_query_param(query, "id").ok_or_else(|| anyhow::anyhow!("Missing ?id= parameter"))?.parse()?;
            return state.scan_jobs.status(id);
        }
        (&Method::Post, "/api/scans/cancel") => {
            let payload: CancelScanReq = read_json_body(req)?;
            state.scan_jobs.cancel(payload.id)?;
            return Ok(serde_json::json!({ "ok": true }));
        }
        _ => {}
    }
    let sys = SystemInfo::collect();

    match (req.method(), path) {
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
            let cat = state.catalog.read().unwrap();
            let cache = state.scan_cache.read().unwrap();
            if let Some(scan) = cache.fresh_full_scan(Instant::now()) {
                let list: Vec<_> = scan.items.iter().map(|rule| {
                    serde_json::json!({ "scanned": true, "rule": rule })
                }).collect();
                return Ok(serde_json::json!({ "items": list, "summary": scan.summary }));
            }
            let list = cleanup_rules_stub_list(&cat, &cache.rules, &sys);
            Ok(serde_json::json!({ "items": list, "summary": null }))
        }
        (&Method::Post, "/api/cleanup/scan") => {
            let _operation = state.cleanup_operation.lock().unwrap();
            let started = Instant::now();
            let results = state.scan_jobs.run_inline(|| {
                Ok(scan_all_with_summary(&state.catalog.read().unwrap().cleanup_rules, &sys))
            })?;
            Ok(state.publish_cleanup_scan(results, started))
        }
        (&Method::Get, "/api/cleanup/preview") => {
            let id = parse_query_param(query, "id")
                .ok_or_else(|| anyhow::anyhow!("Missing ?id= parameter"))?;
            let cat = state.catalog.read().unwrap();
            let rule = cat
                .cleanup_rules
                .iter()
                .find(|r| r.id == id)
                .ok_or_else(|| anyhow::anyhow!("Rule '{}' not found", id))?;
            let res = scan_rule_preview(rule, &sys);
            state
                .scan_cache
                .write()
                .unwrap()
                .rules
                .insert(res.id.clone(), res.clone());
            Ok(serde_json::to_value(res)?)
        }
        (&Method::Post, "/api/cleanup/execute") => {
            let payload: ExecuteReq = read_json_body(req)?;
            let _operation = if payload.dry_run {
                state.cleanup_operation.lock().unwrap()
            } else {
                state.begin_cleanup_mutation()
            };
            let cat = state.catalog.read().unwrap();
            let report = execute_cleanup(
                &cat.cleanup_rules,
                &payload.rule_ids,
                &sys,
                payload.dry_run,
            );
            let updated_sys = SystemInfo::collect();
            Ok(serde_json::json!({
                "report": report,
                "system": updated_sys,
            }))
        }
        (&Method::Post, "/api/cleanup/custom/test") => {
            let payload: CustomRuleInput = read_json_body(req)?;
            let rule = Catalog::build_custom_rule(&payload)?;
            let res = scan_rule_preview(&rule, &sys);
            Ok(serde_json::to_value(res)?)
        }
        (&Method::Post, "/api/cleanup/custom/save") => {
            let payload: CustomRuleInput = read_json_body(req)?;
            let _operation = state.begin_cleanup_mutation();
            let saved = Catalog::save_custom_rule(payload)?;
            let scanned = scan_rule_preview(&saved, &sys);
            let mut cat = state.catalog.write().unwrap();
            if let Some(pos) = cat.cleanup_rules.iter().position(|r| r.id == saved.id) {
                cat.cleanup_rules[pos] = saved.clone();
            } else {
                cat.cleanup_rules.push(saved.clone());
            }
            let mut cache = state.scan_cache.write().unwrap();
            cache.rules.insert(scanned.id.clone(), scanned.clone());
            let items = cleanup_rules_stub_list(&cat, &cache.rules, &sys);
            Ok(serde_json::json!({
                "saved": scanned,
                "items": items,
            }))
        }
        (&Method::Post, "/api/cleanup/custom/delete") => {
            let payload: DeleteCustomRuleReq = read_json_body(req)?;
            let _operation = state.begin_cleanup_mutation();
            Catalog::delete_custom_rule(&payload.rule_id)?;
            let mut cat = state.catalog.write().unwrap();
            cat.cleanup_rules.retain(|r| r.id != payload.rule_id);
            let mut cache = state.scan_cache.write().unwrap();
            cache.rules.remove(&payload.rule_id);
            let items = cleanup_rules_stub_list(&cat, &cache.rules, &sys);
            Ok(serde_json::json!({ "items": items }))
        }
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
        (&Method::Get, "/api/diskmap/presets") => {
            let presets = get_presets(&sys);
            Ok(serde_json::json!({ "presets": presets }))
        }
        (&Method::Post, "/api/diskmap/analyze") => {
            let payload: DiskAnalyzeReq = read_json_body(req)?;
            let report = state.scan_jobs.run_inline(|| analyze_path(
                Path::new(payload.path.trim()),
                payload.max_depth.unwrap_or(4),
            ))?;
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
            let _operation = state.begin_cleanup_mutation();
            let freed = recycle_path(Path::new(payload.path.trim()))?;
            let updated_sys = SystemInfo::collect();
            Ok(serde_json::json!({
                "freed_bytes": freed,
                "system": updated_sys,
            }))
        }
        (&Method::Post, "/api/diskmap/recycle-duplicates") => {
            let payload: RecycleDuplicatesReq = read_json_body(req)?;
            let _operation = state.begin_cleanup_mutation();
            let items = state.scan_jobs.run_inline(|| Ok(recycle_duplicates(&payload.keeper, &payload.candidates)))?;
            Ok(serde_json::json!({ "items": items, "system": SystemInfo::collect() }))
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
            let _operation = state.begin_cleanup_mutation();
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
            let _operation = state.begin_cleanup_mutation();
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
            let _operation = state.begin_cleanup_mutation();
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
            let _operation = state.begin_cleanup_mutation();
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
            let _operation = state.begin_cleanup_mutation();
            let outcome = run_rescue_action(&payload.action_id)?;
            Ok(serde_json::to_value(outcome)?)
        }
        (&Method::Post, "/api/diskmap/duplicates") => {
            let payload: DuplicatesReq = read_json_body(req)?;
            let report = state.scan_jobs.run_inline(|| find_duplicate_files(
                Path::new(payload.path.trim()),
                payload.min_size_bytes.unwrap_or(512 * 1024),
            ))?;
            Ok(serde_json::to_value(report)?)
        }
        (&Method::Get, "/api/diskmap/orphans") => {
            let orphans = find_orphaned_app_dirs();
            Ok(serde_json::json!({ "orphans": orphans }))
        }
        (&Method::Get, "/api/uninstaller/list") => {
            let apps = list_win32_apps();
            Ok(serde_json::json!({ "apps": apps }))
        }
        (&Method::Post, "/api/uninstaller/leftovers") => {
            let payload: LeftoversReq = read_json_body(req)?;
            let leftovers = scan_app_leftovers(
                &payload.display_name,
                &payload.publisher,
                &payload.install_location,
            );
            Ok(serde_json::json!({ "leftovers": leftovers }))
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
            let _operation = state.begin_cleanup_mutation();
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
            let leftovers = scan_app_leftovers(
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
        (&Method::Get, "/api/report/export") => {
            let (cat, scans) = {
                let _operation = state.cleanup_operation.lock().unwrap();
                let cat = state.catalog.read().unwrap().clone();
                let cached = state
                    .scan_cache
                    .read()
                    .unwrap()
                    .fresh_full_scan(Instant::now());
                let scans = if let Some(scans) = cached { scans } else {
                    let started = Instant::now();
                    let results = state.scan_jobs.run_inline(|| Ok(scan_all_with_summary(&cat.cleanup_rules, &sys)))?;
                    state
                        .scan_cache
                        .write()
                        .unwrap()
                        .store_full_scan(results, started)
                };
                (cat, scans)
            };
            let report = generate_health_report(&cat, Some(&scans));
            Ok(serde_json::to_value(report)?)
        }
        _ => Err(anyhow::anyhow!("Unknown API endpoint: {}", path)),
    }
}

fn serve_embedded(req: Request, url_path: &str) {
    let rel = url_path.trim_start_matches('/');
    if rel.contains("..") {
        let _ = req.respond(Response::empty(StatusCode(403)));
        return;
    }
    let normalized = if rel.is_empty() {
        "/index.html".to_string()
    } else {
        format!("/{}", rel)
    };

    let matched = EMBEDDED_UI_ASSETS
        .iter()
        .find(|(p, _, _)| *p == normalized)
        .or_else(|| {
            EMBEDDED_UI_ASSETS
                .iter()
                .find(|(p, _, _)| *p == "/index.html")
        });

    match matched {
        Some((route, mime, bytes)) => {
            let mut resp = Response::from_data(*bytes).with_status_code(StatusCode(200));
            if let Ok(h) = Header::from_bytes("Content-Type", *mime) {
                resp.add_header(h);
            }
            let cache_ctl = if route.starts_with("/assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            if let Ok(h) = Header::from_bytes("Cache-Control", cache_ctl) {
                resp.add_header(h);
            }
            let _ = req.respond(resp);
        }
        None => {
            let _ = req.respond(Response::empty(StatusCode(404)));
        }
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
    fn report_cache_requires_a_fresh_full_scan() {
        let catalog = Catalog::load(None).unwrap();
        let sys = SystemInfo::collect();
        let mut cache = CleanupScanCache::default();
        let entries = cleanup_rules_stub_list(&catalog, &cache.rules, &sys);
        let preview: CleanupScanResult =
            serde_json::from_value(entries[0]["rule"].clone()).unwrap();
        let started = Instant::now();

        cache.rules.insert(preview.id.clone(), preview.clone());
        assert!(cache.fresh_full_scan(started).is_none());

        cache.store_full_scan(CleanupBatchScan { items: vec![preview.clone()], ..CleanupBatchScan::default() }, started);
        cache.rules.get_mut(&preview.id).unwrap().cleanable_bytes = 50;
        let cached = cache.fresh_full_scan(started).unwrap();
        assert_eq!(cached.items[0].cleanable_bytes, preview.cleanable_bytes);
        assert!(cache.fresh_full_scan(started + FULL_SCAN_TTL).is_none());

        cache.store_full_scan(CleanupBatchScan::default(), started);
        assert!(cache.fresh_full_scan(started).unwrap().items.is_empty());
    }

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
