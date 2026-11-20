use anyhow::Result;
use clap::Parser;
use dism_core::appx::{list_appx_packages, remove_appx_package};
use dism_core::cleaner::{
    execute_cleanup, is_deferred_system_task_proc, scan_all_with_progress, scan_all_with_summary, scan_custom_proc_name,
    scan_rule_preview, CleanupBatchScan, CleanupScanResult, ReviewedCleanup,
};
use dism_core::diskmap::{
    analyze_path_with_options, find_duplicate_files,
    find_duplicate_files_with_progress, find_orphaned_app_dirs, get_presets,
};
use dism_core::drivers::{default_backup_dir, delete_driver, export_drivers, list_drivers};
use dism_core::duplicate_recycle::{recycle_duplicates, DuplicateRecycleTarget};
use dism_core::evaluator::eval_condition;
use dism_core::features::{list_optional_features, set_feature_enabled_with_progress};
use dism_core::lockcheck::{find_locking_processes, terminate_locking_process};
use dism_core::maintenance::MaintenanceProgress;
use dism_core::offline_wim::inspect_wim;
use dism_core::optimizer::{
    apply_optimization, apply_optimization_batch, evaluate_all_groups, export_optimization_profile,
    get_optimization_presets, ApplyValue, OptimizationProfileEntry, ScopeName,
};
use dism_core::path_recycle::{recycle_reviewed_path, review_recycle_path, RecycleTarget};
use dism_core::report::generate_health_report;
use dism_core::rescue::{create_restore_point, list_restore_points, run_rescue_action_with_progress};
use dism_core::startup::{
    inspect_startup_item, list_scheduled_tasks, list_startup_items, list_third_party_services,
    set_scheduled_task_enabled, set_service_start_mode, set_startup_enabled,
};
use dism_core::sysinfo::{is_administrator, open_in_browser, relaunch_as_admin};
use dism_core::systasks::{
    get_system_task_status, invalidate_systasks_cache, run_system_task_with_progress,
};
use dism_core::uninstaller::{
    launch_win32_uninstaller, list_win32_apps, recycle_app_leftovers, scan_reviewable_app_leftovers,
    LeftoverRecycleTarget, UninstallState, Win32Uninstall,
};
use dism_core::{Catalog, CustomRuleInput, SnapshotManager, SystemInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

include!(concat!(env!("OUT_DIR"), "/embedded_ui.rs"));

mod request_security;
mod scan_jobs;
mod static_files;
use request_security::{
    add_cors_headers, header_value, new_session_token, verify_request, Session,
    SESSION_EXCHANGE_PATH, SESSION_HEADER,
};
use scan_jobs::{ScanBusy, ScanJobs};

#[derive(Parser)]
#[command(name = "dism-app", version, about = "DismNext Desktop Host Server & GUI Launcher")]
struct Args {
    #[arg(long, default_value_t = 9527)]
    port: u16,

    #[arg(long)]
    no_window: bool,

    #[arg(long)]
    catalog: Option<PathBuf>,

    #[arg(long)]
    ui_dir: Option<PathBuf>,

    /// Allow the Vite origin when running a non-elevated debug host.
    #[arg(long)]
    dev_origin: bool,
}

struct AppState {
    port: u16,
    session: Session,
    dev_origin: bool,
    catalog_path: Option<PathBuf>,
    catalog: RwLock<Catalog>,
    snap_mgr: SnapshotManager,
    scan_cache: RwLock<CleanupScanCache>,
    cleanup_operation: Mutex<CleanupReviewSlot>,
    scan_jobs: Arc<ScanJobs>,
    maintenance_jobs: Arc<ScanJobs<MaintenanceProgress>>,
    uninstall: Mutex<Option<Win32Uninstall>>,
    ui_dir: Option<PathBuf>,
}

const FULL_SCAN_TTL: Duration = Duration::from_secs(60);
const CLEANUP_REVIEW_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_HTTP_BODY: usize = 4 * 1024 * 1024;

#[derive(Debug)]
struct HttpBodyTooLarge;

impl std::fmt::Display for HttpBodyTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP request body exceeds {MAX_HTTP_BODY} bytes")
    }
}

impl std::error::Error for HttpBodyTooLarge {}

#[derive(Default)]
struct CleanupReviewSlot {
    ids: RandomState,
    sequence: u64,
    pending: Option<(String, Instant, ReviewedCleanup)>,
    maintenance_active: bool,
}

impl CleanupReviewSlot {
    fn store(&mut self, plan: ReviewedCleanup, now: Instant) -> String {
        self.sequence += 1;
        let id = format!("{:016x}", self.ids.hash_one(self.sequence));
        self.pending = Some((id.clone(), now, plan));
        id
    }

    fn take(&mut self, id: &str, now: Instant) -> Option<ReviewedCleanup> {
        let (pending_id, created, _) = self.pending.as_ref()?;
        if now.saturating_duration_since(*created) >= CLEANUP_REVIEW_TTL {
            self.pending = None;
            return None;
        }
        if id != pending_id {
            return None;
        }
        self.pending.take().map(|(_, _, plan)| plan)
    }
}

#[derive(Debug)]
struct CleanupReviewUnavailable;

impl std::fmt::Display for CleanupReviewUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cleanup review is no longer available. Review the selected rules again.")
    }
}

impl std::error::Error for CleanupReviewUnavailable {}

#[derive(Default)]
struct CleanupScanCache {
    rules: HashMap<String, CleanupScanResult>,
    full_scan: Option<(Instant, Arc<CleanupBatchScan>)>,
    invalidated_at: Option<Instant>,
}

impl CleanupScanCache {
    fn invalidate(&mut self) {
        self.full_scan = None;
        self.rules.clear();
        self.invalidated_at = Some(Instant::now());
    }

    fn can_store(&self, started: Instant) -> bool {
        self.invalidated_at
            .is_none_or(|invalidated| started >= invalidated)
    }

    fn store_full_scan(
        &mut self,
        results: CleanupBatchScan,
        started: Instant,
    ) -> Arc<CleanupBatchScan> {
        let results = Arc::new(results);
        if self.can_store(started) {
            self.full_scan = Some((started, Arc::clone(&results)));
        }
        results
    }

    fn fresh_full_scan(&self, now: Instant) -> Option<Arc<CleanupBatchScan>> {
        let (started, results) = self.full_scan.as_ref()?;
        (now.saturating_duration_since(*started) < FULL_SCAN_TTL).then(|| Arc::clone(results))
    }
}

impl AppState {
    fn begin_cleanup_mutation(&self) -> MutexGuard<'_, CleanupReviewSlot> {
        let mut operation = self.cleanup_operation.lock().unwrap();
        operation.pending = None;
        self.scan_cache.write().unwrap().invalidate();
        operation
    }

    fn publish_cleanup_scan(&self, results: CleanupBatchScan, started: Instant) -> serde_json::Value {
        let results = {
            let mut cache = self.scan_cache.write().unwrap();
            if cache.can_store(started) {
                cache.rules.clear();
            }
            cache.store_full_scan(results, started)
        };
        serde_json::json!({ "items": results.items, "summary": results.summary })
    }
}

struct MaintenanceGuard<'a>(&'a AppState);

impl Drop for MaintenanceGuard<'_> {
    fn drop(&mut self) {
        invalidate_systasks_cache();
        let mut operation = self.0.cleanup_operation.lock().unwrap();
        operation.maintenance_active = false;
        operation.pending = None;
        self.0.scan_cache.write().unwrap().invalidate();
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum MaintenanceRequest {
    Feature { name: String, enabled: bool },
    SystemTask { task_id: String },
    Rescue { action_id: String },
}

fn run_maintenance(
    state: &AppState,
    request: MaintenanceRequest,
    progress: &MaintenanceProgress,
) -> Result<serde_json::Value> {
    {
        let mut operation = state.begin_cleanup_mutation();
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

#[derive(Deserialize)]
struct ExecuteReq {
    rule_ids: Vec<String>,
    dry_run: bool,
}

#[derive(Deserialize)]
struct CleanupReviewReq {
    rule_ids: Vec<String>,
}

#[derive(Deserialize)]
struct CleanupPlanReq {
    plan_id: String,
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
    #[serde(default)]
    measure_allocation: bool,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ScanRequest {
    DiskAnalysis {
        path: String,
        max_depth: Option<usize>,
        #[serde(default)]
        measure_allocation: bool,
    },
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
struct StartupInspectReq {
    id: String,
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
    path: String,
    pid: u32,
    process_start_time: String,
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
struct WimInspectReq {
    path: String,
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
    let is_admin = is_administrator();
    anyhow::ensure!(
        !args.dev_origin || (cfg!(debug_assertions) && !is_admin),
        "--dev-origin requires a non-elevated debug build"
    );
    let catalog_path = args
        .catalog
        .as_deref()
        .map(std::fs::canonicalize)
        .transpose()?;
    let catalog = Catalog::load(catalog_path.as_deref())?;
    let snap_mgr = SnapshotManager::new(SnapshotManager::default_dir());
    let ui_dir = resolve_ui_dir(args.ui_dir, args.dev_origin)?;
    let launch_token = if args.no_window {
        None
    } else {
        Some(new_session_token()?)
    };

    let state = Arc::new(AppState {
        port: args.port,
        session: Session::new(launch_token.clone())?,
        dev_origin: args.dev_origin,
        catalog_path,
        catalog: RwLock::new(catalog),
        snap_mgr,
        scan_cache: RwLock::new(CleanupScanCache::default()),
        cleanup_operation: Mutex::default(),
        scan_jobs: Arc::new(ScanJobs::default()),
        maintenance_jobs: Arc::new(ScanJobs::default()),
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

    match launch_token {
        Some(token) => launch_desktop_window(&format!("{url}/#launch={token}"))?,
        None => println!(
            "DismNext session URL: {url}/#token={}",
            state.session.api_token()
        ),
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

fn resolve_ui_dir(explicit: Option<PathBuf>, dev_origin: bool) -> Result<Option<PathBuf>> {
    if let Some(p) = explicit {
        let dir = p.canonicalize()?;
        anyhow::ensure!(
            dir.join("index.html").is_file(),
            "UI directory has no index.html"
        );
        return Ok(Some(dir));
    }
    let local_dist = Path::new("ui/dist");
    if dev_origin && EMBEDDED_UI_ASSETS.is_empty() && local_dist.join("index.html").is_file() {
        return Ok(Some(local_dist.canonicalize()?));
    }
    Ok(None)
}

fn launch_desktop_window(url: &str) -> Result<()> {
    let edge_candidates = [
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ];
    for edge in edge_candidates {
        if Path::new(edge).exists()
            && Command::new(edge)
                .current_dir(Path::new(edge).parent().unwrap())
                .arg(format!("--app={}", url))
                .arg("--window-size=1320,860")
                .spawn()
                .is_ok()
        {
            return Ok(());
        }
    }
    open_in_browser(url)
}

fn handle_request(mut req: Request, state: &Arc<AppState>) {
    let origin_header = header_value(&req, "origin").map(str::to_owned);
    let raw_url = req.url().to_string();
    let (path, query) = match raw_url.split_once('?') {
        Some((p, q)) => (p, q),
        None => (raw_url.as_str(), ""),
    };
    let origin = origin_header.as_deref();
    if let Err(reason) = verify_request(
        &req,
        path,
        state.port,
        state.dev_origin,
        state.session.api_token(),
    ) {
        respond_json(
            req,
            StatusCode(403),
            &ErrorPayload {
                error: reason.into(),
            },
            state,
            origin,
        );
        return;
    }

    if req.method() == &Method::Options {
        let mut resp = Response::empty(StatusCode(204));
        add_cors_headers(&mut resp, state.port, state.dev_origin, origin);
        let _ = req.respond(resp);
        return;
    }

    if req.method() == &Method::Post && path == SESSION_EXCHANGE_PATH {
        if req.body_length().unwrap_or(0) != 0
            || req
                .headers()
                .iter()
                .any(|header| header.field.equiv("Transfer-Encoding"))
        {
            let body = ErrorPayload {
                error: "Session exchange requires an empty request body".into(),
            };
            respond_json(req, StatusCode(400), &body, state, origin);
        } else if let Some(token) = state.session.exchange(header_value(&req, SESSION_HEADER)) {
            respond_json(
                req,
                StatusCode(200),
                &serde_json::json!({ "token": token }),
                state,
                origin,
            );
        } else {
            let body = ErrorPayload {
                error: "Invalid or expired DismNext launch token".into(),
            };
            respond_json(req, StatusCode(403), &body, state, origin);
        }
        return;
    }

    if path.starts_with("/api/") {
        let result = if req.body_length().is_some_and(|len| len > MAX_HTTP_BODY) {
            Err(HttpBodyTooLarge.into())
        } else {
            dispatch_api(&mut req, path, query, state)
        };
        match result {
            Ok(json_val) => respond_json(req, StatusCode(200), &json_val, state, origin),
            Err(err) => {
                let code = if err.is::<HttpBodyTooLarge>() {
                    413
                } else if err.is::<ScanBusy>() || err.is::<CleanupReviewUnavailable>() {
                    409
                } else {
                    400
                };
                let body = ErrorPayload {
                    error: err.to_string(),
                };
                respond_json(req, StatusCode(code), &body, state, origin);
            }
        }
        return;
    }

    if let Some(ref ui_dir) = state.ui_dir {
        static_files::serve(req, ui_dir, path);
    } else if !EMBEDDED_UI_ASSETS.is_empty() {
        serve_embedded(req, path);
    } else {
        let mut resp = Response::from_string(
            "DismNext API Server is running. Run `npm run build` in ui/ to enable GUI.",
        )
        .with_status_code(StatusCode(200));
        add_cors_headers(&mut resp, state.port, state.dev_origin, origin);
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
        (&Method::Post, "/api/maintenance/start") => {
            let request: MaintenanceRequest = read_json_body(req)?;
            let app = Arc::clone(state);
            let id = state
                .maintenance_jobs
                .start(move |progress| run_maintenance(&app, request, progress))?;
            return Ok(serde_json::json!({ "id": id }));
        }
        (&Method::Get, "/api/maintenance/status") => {
            let id = parse_query_param(query, "id")
                .ok_or_else(|| anyhow::anyhow!("Missing ?id= parameter"))?
                .parse()?;
            return state.maintenance_jobs.status(id);
        }
        (&Method::Post, "/api/maintenance/cancel") => {
            let payload: CancelScanReq = read_json_body(req)?;
            state.maintenance_jobs.cancel(payload.id)?;
            return Ok(serde_json::json!({ "ok": true }));
        }
        (&Method::Post, "/api/scans/start") => {
            let payload: ScanRequest = read_json_body(req)?;
            let app = Arc::clone(state);
            let id = state.scan_jobs.start(move |progress| match payload {
                ScanRequest::DiskAnalysis { path, max_depth, measure_allocation } => {
                    let report = analyze_path_with_options(
                        Path::new(path.trim()),
                        max_depth.unwrap_or(4),
                        measure_allocation,
                        Some(progress),
                    )?;
                    Ok(serde_json::to_value(report)?)
                }
                ScanRequest::DuplicateFiles { path, min_size_bytes } => {
                    let report = find_duplicate_files_with_progress(Path::new(path.trim()), min_size_bytes.unwrap_or(512 * 1024), progress)?;
                    Ok(serde_json::to_value(report)?)
                }
                ScanRequest::CleanupRules => {
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
            if state.maintenance_jobs.is_active() {
                return Err(ScanBusy("maintenance task").into());
            }
            let mut extra_args = vec!["--port".to_string(), state.port.to_string()];
            if let Some(ref catalog) = state.catalog_path {
                extra_args.push("--catalog".to_string());
                extra_args.push(catalog.to_string_lossy().into_owned());
            }
            if !state.dev_origin {
                if let Some(ref ui) = state.ui_dir {
                    extra_args.push("--ui-dir".to_string());
                    extra_args.push(ui.to_string_lossy().into_owned());
                }
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
            let started = Instant::now();
            let results = state.scan_jobs.run_inline(|| {
                Ok(scan_all_with_summary(&state.catalog.read().unwrap().cleanup_rules, &sys))
            })?;
            Ok(state.publish_cleanup_scan(results, started))
        }
        (&Method::Get, "/api/cleanup/preview") => {
            let started = Instant::now();
            let id = parse_query_param(query, "id")
                .ok_or_else(|| anyhow::anyhow!("Missing ?id= parameter"))?;
            let cat = state.catalog.read().unwrap();
            let rule = cat
                .cleanup_rules
                .iter()
                .find(|r| r.id == id)
                .ok_or_else(|| anyhow::anyhow!("Rule '{}' not found", id))?;
            let res = scan_rule_preview(rule, &sys);
            let mut cache = state.scan_cache.write().unwrap();
            if cache.can_store(started) {
                cache.rules.insert(res.id.clone(), res.clone());
            }
            Ok(serde_json::to_value(res)?)
        }
        (&Method::Post, "/api/cleanup/execute") => {
            let payload: ExecuteReq = read_json_body(req)?;
            anyhow::ensure!(payload.dry_run, "Desktop cleanup requires a reviewed plan");
            let _operation = state.cleanup_operation.lock().unwrap();
            let cat = state.catalog.read().unwrap();
            let report = execute_cleanup(&cat.cleanup_rules, &payload.rule_ids, &sys, true);
            Ok(serde_json::json!({
                "report": report,
                "system": sys,
            }))
        }
        (&Method::Post, "/api/cleanup/review") => {
            let payload: CleanupReviewReq = read_json_body(req)?;
            let mut operation = state.cleanup_operation.lock().unwrap();
            if operation.maintenance_active {
                return Err(ScanBusy("maintenance task").into());
            }
            operation.pending = None;
            let cat = state.catalog.read().unwrap();
            let (plan, report) = ReviewedCleanup::prepare(&cat.cleanup_rules, &payload.rule_ids, &sys);
            let plan_id = (!report.outcomes.is_empty()).then(|| operation.store(plan, Instant::now()));
            Ok(serde_json::json!({ "plan_id": plan_id, "report": report }))
        }
        (&Method::Post, "/api/cleanup/confirm") => {
            let payload: CleanupPlanReq = read_json_body(req)?;
            let mut operation = state.cleanup_operation.lock().unwrap();
            if operation.maintenance_active {
                return Err(ScanBusy("maintenance task").into());
            }
            let plan = operation
                .take(&payload.plan_id, Instant::now())
                .ok_or(CleanupReviewUnavailable)?;
            state.scan_cache.write().unwrap().invalidate();
            let report = plan.execute();
            Ok(serde_json::json!({ "report": report, "system": SystemInfo::collect() }))
        }
        (&Method::Post, "/api/cleanup/discard-review") => {
            let payload: CleanupPlanReq = read_json_body(req)?;
            let mut operation = state.cleanup_operation.lock().unwrap();
            operation.take(&payload.plan_id, Instant::now());
            Ok(serde_json::json!({ "ok": true }))
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
            if let Some(id) = payload.id.as_deref().map(str::trim) {
                anyhow::ensure!(
                    !state
                        .catalog
                        .read()
                        .unwrap()
                        .cleanup_rules
                        .iter()
                        .any(|rule| rule.id == id && rule.source != "custom"),
                    "Cannot replace a catalog cleanup rule"
                );
            }
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
            cat.cleanup_rules
                .retain(|r| r.id != payload.rule_id || r.source != "custom");
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
            let report = state.scan_jobs.run_inline(|| analyze_path_with_options(
                Path::new(payload.path.trim()),
                payload.max_depth.unwrap_or(4),
                payload.measure_allocation,
                None,
            ))?;
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
            let item = state.scan_jobs.run_inline(|| Ok(recycle_reviewed_path(&payload)))?;
            Ok(serde_json::json!({
                "item": item,
                "system": SystemInfo::collect(),
            }))
        }
        (&Method::Post, "/api/diskmap/recycle/review") => {
            let payload: RecycleReviewReq = read_json_body(req)?;
            let cutoff = payload.scan_started_at.parse()?;
            let review = state.scan_jobs.run_inline(|| {
                Ok(review_recycle_path(&payload.path, payload.is_dir, cutoff))
            })?;
            Ok(serde_json::to_value(review)?)
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
            let operation = state.begin_cleanup_mutation();
            if operation.maintenance_active {
                return Err(ScanBusy("maintenance task").into());
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
            let leftovers = scan_reviewable_app_leftovers(
                &payload.display_name,
                &payload.publisher,
                &payload.install_location,
            );
            Ok(serde_json::json!({ "leftovers": leftovers }))
        }
        (&Method::Post, "/api/uninstaller/leftovers/recycle") => {
            let payload: RecycleLeftoversReq = read_json_body(req)?;
            let _operation = state.begin_cleanup_mutation();
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
        (&Method::Get, "/api/report/export") => {
            let (cat, scans) = {
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
    let mut body = Vec::new();
    req.as_reader()
        .take(MAX_HTTP_BODY as u64 + 1)
        .read_to_end(&mut body)?;
    if body.len() > MAX_HTTP_BODY {
        return Err(HttpBodyTooLarge.into());
    }
    Ok(serde_json::from_slice(&body)?)
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
    state: &AppState,
    origin: Option<&str>,
) {
    let text = serde_json::to_string(val).unwrap_or_else(|_| "{}".to_string());
    let mut resp = Response::from_string(text).with_status_code(status);
    if let Ok(h) = Header::from_bytes("Content-Type", "application/json; charset=utf-8") {
        resp.add_header(h);
    }
    if let Ok(header) = Header::from_bytes("Cache-Control", "no-store") {
        resp.add_header(header);
    }
    add_cors_headers(&mut resp, state.port, state.dev_origin, origin);
    let _ = req.respond(resp);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request_security::{is_allowed_host, is_allowed_origin};

    #[test]
    fn cleanup_reviews_are_single_use_and_expire() {
        let sys = SystemInfo::collect();
        let new_plan = || ReviewedCleanup::prepare(&[], &[], &sys).0;
        let mut slot = CleanupReviewSlot::default();
        let now = Instant::now();
        let first = slot.store(new_plan(), now);
        let current = slot.store(new_plan(), now);
        assert_ne!(first, current);
        assert!(slot.take(&first, now).is_none());
        assert!(slot.take(&current, now).is_some());
        assert!(slot.take(&current, now).is_none());

        let last_valid = slot.store(new_plan(), now);
        assert!(slot.take(&last_valid, now + CLEANUP_REVIEW_TTL - Duration::from_millis(1)).is_some());
        let expiring = slot.store(new_plan(), now);
        assert!(slot.take(&expiring, now + CLEANUP_REVIEW_TTL).is_none());
        assert!(slot.pending.is_none());
    }

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

        cache.invalidate();
        cache.store_full_scan(CleanupBatchScan::default(), started);
        assert!(cache.fresh_full_scan(Instant::now()).is_none());
    }

    #[test]
    fn test_host_and_origin_security_checks() {
        assert!(is_allowed_host("127.0.0.1:9527", 9527));
        assert!(is_allowed_host("localhost:9527", 9527));
        assert!(!is_allowed_host("evil.example.com:9527", 9527));
        assert!(!is_allowed_host("127.0.0.1:9999", 9527));

        assert!(is_allowed_origin("http://127.0.0.1:9527", 9527, false));
        assert!(is_allowed_origin("http://localhost:9527", 9527, false));
        assert!(!is_allowed_origin("http://localhost:5173", 9527, false));
        assert_eq!(
            is_allowed_origin("http://localhost:5173", 9527, true),
            cfg!(debug_assertions)
        );
        assert!(!is_allowed_origin("https://evil.example.com", 9527, false));
        assert!(!is_allowed_origin("null", 9527, false));
    }
}
