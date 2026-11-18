use anyhow::Result;
use clap::Parser;
use dism_core::catalog::Action;
use dism_core::cleaner::{execute_cleanup, scan_all, scan_rule, CleanupScanResult};
use dism_core::diskmap::{analyze_path, get_presets};
use dism_core::evaluator::eval_condition;
use dism_core::optimizer::{apply_optimization, evaluate_all_groups, ApplyValue, ScopeName};
use dism_core::sysinfo::relaunch_as_admin;
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
    if req.method() == &Method::Options {
        let mut resp = Response::empty(StatusCode(204));
        add_cors_headers(&mut resp);
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
            Ok(json_val) => respond_json(req, StatusCode(200), &json_val),
            Err(err) => {
                let body = ErrorPayload {
                    error: err.to_string(),
                };
                respond_json(req, StatusCode(400), &body);
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
        add_cors_headers(&mut resp);
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
                        let is_sys_task = scan.actions.iter().any(|a| {
                            matches!(
                                a,
                                Action::Custom { proc_name, .. }
                                if matches!(
                                    proc_name.as_str(),
                                    "DismComponentCleanup"
                                        | "DismCompactOs"
                                        | "DismHardLinkMerge"
                                        | "DismDriverCleanup"
                                        | "DismAppxsCleanup"
                                        | "NCCorruptedAppXOnlineCleanup"
                                        | "NCSRPointOnlineCleanup"
                                        | "NCInstallerFolderCleanup"
                                )
                            )
                        });
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
            let res = scan_rule(rule, &sys);
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
                payload.max_depth.unwrap_or(3),
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
        _ => Err(anyhow::anyhow!("Unknown API endpoint: {}", path)),
    }
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

fn respond_json<T: Serialize>(req: Request, status: StatusCode, val: &T) {
    let text = serde_json::to_string(val).unwrap_or_else(|_| "{}".to_string());
    let mut resp = Response::from_string(text).with_status_code(status);
    if let Ok(h) = Header::from_bytes("Content-Type", "application/json; charset=utf-8") {
        resp.add_header(h);
    }
    add_cors_headers(&mut resp);
    let _ = req.respond(resp);
}

fn add_cors_headers<R: std::io::Read>(resp: &mut Response<R>) {
    if let Ok(h) = Header::from_bytes("Access-Control-Allow-Origin", "*") {
        resp.add_header(h);
    }
    if let Ok(h) = Header::from_bytes("Access-Control-Allow-Methods", "GET, POST, OPTIONS") {
        resp.add_header(h);
    }
    if let Ok(h) = Header::from_bytes("Access-Control-Allow-Headers", "Content-Type") {
        resp.add_header(h);
    }
}
