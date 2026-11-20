use anyhow::Result;
use clap::Parser;
use dism_core::cleaner::{CleanupBusy, CleanupReviewUnavailable};
use dism_core::drivers::list_drivers;
use dism_core::startup::list_scheduled_tasks;
use dism_core::sysinfo::{is_administrator, open_in_browser};
use dism_core::systasks::get_system_task_status;
use dism_core::uninstaller::UninstallState;
use dism_core::{Catalog, SnapshotManager, SystemInfo};
use serde::Serialize;
use std::io;
use std::net::TcpListener;
use std::os::windows::io::AsRawSocket;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use windows_sys::Win32::Networking::WinSock::{
    setsockopt, WSAGetLastError, SOL_SOCKET, SO_RCVTIMEO, SO_SNDTIMEO,
};

mod admin_window;
mod desktop_lifecycle;
mod desktop_security;
mod http_workers;
mod request_security;
mod routes;
mod scan_jobs;
mod state;
mod static_files;

use desktop_lifecycle::{DesktopLifecycle, WakeEvent, WINDOW_PATH};
use http_workers::HttpWorkers;
use request_security::{
    add_cors_headers, header_value, verify_request, Session, SESSION_EXCHANGE_PATH, SESSION_HEADER,
};
use routes::{HttpBodyTooLarge, MAX_HTTP_BODY};
use scan_jobs::{ScanBusy, ScanJobs};
use state::{AppState, CleanupScanCache};

#[derive(Parser)]
#[command(
    name = "dism-app",
    version,
    about = "DismNext Desktop Host Server & GUI Launcher"
)]
struct Args {
    #[arg(long, default_value_t = 9527, value_parser = clap::value_parser!(u16).range(1..))]
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
    let wake_event = if args.no_window {
        None
    } else {
        let Some(event) = WakeEvent::launch_or_signal(args.port, is_admin)? else {
            println!("Requested a window from the running DismNext instance.");
            return Ok(());
        };
        Some(event)
    };
    let mut admin_desktop = (is_admin && wake_event.is_some())
        .then(admin_window::AdminDesktop::new)
        .transpose()?;
    let catalog_path = args
        .catalog
        .as_deref()
        .map(std::fs::canonicalize)
        .transpose()?;
    let catalog = Catalog::load(catalog_path.as_deref())?;
    let snap_mgr = SnapshotManager::user_default();
    let ui_dir = resolve_ui_dir(args.ui_dir, args.dev_origin)?;
    let state = Arc::new(AppState {
        port: args.port,
        session: Session::new()?,
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
        desktop: wake_event.as_ref().map(|_| DesktopLifecycle::new()),
        requests: RwLock::new(()),
        relaunching: AtomicBool::new(false),
    });

    let addr = format!("127.0.0.1:{}", args.port);
    let server = bind_with_retry(&addr)?;
    let url = format!("http://{}", addr);
    println!("DismNext server listening on {}", url);
    if let Some(ref d) = state.ui_dir {
        println!("Serving external UI from {}", d.display());
    } else if static_files::embedded_asset_count() != 0 {
        println!(
            "Serving embedded portable UI ({} assets compiled into binary)",
            static_files::embedded_asset_count()
        );
    } else {
        println!("UI assets not found; serving API endpoints only.");
    }

    if state.desktop.is_some() {
        open_desktop(&state, &url, admin_desktop.as_mut())?;
    } else {
        println!(
            "DismNext session URL: {url}/#token={}",
            state.session.api_token()
        );
    }

    // Pre-warm static system metadata, driver cache, system task status, and scheduled tasks
    std::thread::spawn(|| {
        let _ = SystemInfo::collect();
        let _ = list_drivers();
        let _ = get_system_task_status();
        let _ = list_scheduled_tasks();
    });

    let workers = Arc::new(HttpWorkers::new(&state)?);
    let dispatcher = {
        let workers = Arc::clone(&workers);
        let state = Arc::clone(&state);
        std::thread::Builder::new()
            .name("dismnext-http-dispatch".into())
            .spawn(move || -> Result<()> {
                while !state.relaunching.load(Ordering::Acquire) {
                    if let Some(req) = server.recv_timeout(Duration::from_millis(200))? {
                        workers.submit(req, &state);
                    }
                }
                Ok(())
            })?
    };
    let mut last_window_check = Instant::now();
    loop {
        if state.relaunching.load(Ordering::Acquire) {
            if !workers.has_work() {
                break;
            }
            std::thread::park_timeout(Duration::from_millis(25));
            continue;
        }
        if dispatcher.is_finished() {
            dispatcher
                .join()
                .map_err(|_| anyhow::anyhow!("HTTP dispatcher stopped unexpectedly"))??;
            break;
        }
        if let (Some(event), Some(desktop)) = (&wake_event, &state.desktop) {
            if event.requested()? {
                if let Err(error) = open_desktop(&state, &url, admin_desktop.as_mut()) {
                    eprintln!("Could not reopen DismNext: {error:#}");
                }
            }
            if last_window_check.elapsed() >= Duration::from_millis(250) {
                last_window_check = Instant::now();
                let has_work = workers.has_work()
                    || state.scan_jobs.is_active()
                    || state.maintenance_jobs.is_active()
                    || uninstall_running(&state);
                let native_window_open =
                    admin_desktop.as_ref().map(|desktop| desktop.window_open());
                if desktop.idle(has_work, native_window_open) {
                    break;
                }
            }
        }
        std::thread::park_timeout(Duration::from_millis(100));
    }

    Ok(())
}

fn open_desktop(
    state: &AppState,
    url: &str,
    admin: Option<&mut admin_window::AdminDesktop>,
) -> Result<()> {
    if let Some(admin) = admin {
        admin.open(url, state.session.api_token())?;
    } else {
        let token = state.session.issue_browser_launch()?;
        launch_desktop_window(&format!("{url}/#launch={token}"))?;
    }
    if let Some(desktop) = &state.desktop {
        desktop.launching();
    }
    Ok(())
}

fn uninstall_running(state: &AppState) -> bool {
    state.uninstall.try_lock().map_or(true, |mut tracked| {
        tracked.as_mut().is_some_and(|job| {
            job.status()
                .is_ok_and(|status| status.state == UninstallState::Running)
        })
    })
}

fn bind_with_retry(addr: &str) -> Result<Server> {
    let mut last_err = String::new();
    for attempt in 0..20 {
        match TcpListener::bind(addr) {
            Ok(listener) => {
                set_socket_timeouts(&listener, 10_000)?;
                return Server::from_listener(listener, None).map_err(|error| {
                    anyhow::anyhow!("Could not start desktop HTTP server: {error}")
                });
            }
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

fn set_socket_timeouts(listener: &TcpListener, millis: u32) -> io::Result<()> {
    for option in [SO_RCVTIMEO, SO_SNDTIMEO] {
        let result = unsafe {
            setsockopt(
                listener.as_raw_socket() as usize,
                SOL_SOCKET,
                option,
                (&millis as *const u32).cast(),
                std::mem::size_of_val(&millis) as i32,
            )
        };
        if result != 0 {
            return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
        }
    }
    Ok(())
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
    if dev_origin
        && static_files::embedded_asset_count() == 0
        && local_dist.join("index.html").is_file()
    {
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
        } else if req.method() == &Method::Post && path == WINDOW_PATH {
            (|| {
                let update = routes::read_json_body(&mut req)?;
                if let Some(desktop) = &state.desktop {
                    desktop.update(update)?;
                }
                Ok(serde_json::json!({ "ok": true }))
            })()
        } else if path == "/api/sys/elevate" && req.method() == &Method::Post {
            routes::dispatch(&mut req, path, query, state)
        } else {
            state
                .requests
                .try_read()
                .map_err(|_| ScanBusy("desktop relaunch"))
                .map_err(anyhow::Error::from)
                .and_then(|_permit| {
                    if state.relaunching.load(Ordering::Acquire) {
                        return Err(ScanBusy("desktop relaunch").into());
                    }
                    routes::dispatch(&mut req, path, query, state)
                })
        };
        match result {
            Ok(json_val) => respond_json(req, StatusCode(200), &json_val, state, origin),
            Err(err) => {
                let code = if err.is::<HttpBodyTooLarge>() {
                    413
                } else if err.is::<ScanBusy>()
                    || err.is::<CleanupReviewUnavailable>()
                    || err.is::<CleanupBusy>()
                {
                    409
                } else if err.is::<routes::UnknownEndpoint>() {
                    404
                } else if let Some(io) = err.downcast_ref::<std::io::Error>() {
                    match io.kind() {
                        std::io::ErrorKind::NotFound => 404,
                        std::io::ErrorKind::PermissionDenied => 403,
                        std::io::ErrorKind::InvalidData | std::io::ErrorKind::InvalidInput => 400,
                        _ => 500,
                    }
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
    } else if static_files::embedded_asset_count() != 0 {
        static_files::serve_embedded(req, path);
    } else {
        let mut resp = Response::from_string(
            "DismNext API Server is running. Run `npm run build` in ui/ to enable GUI.",
        )
        .with_status_code(StatusCode(200));
        add_cors_headers(&mut resp, state.port, state.dev_origin, origin);
        let _ = req.respond(resp);
    }
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
    static_files::add_security_headers(&mut resp);
    let _ = req.respond(resp);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request_security::{is_allowed_host, is_allowed_origin};

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

    #[test]
    fn accepted_connections_inherit_http_read_and_write_timeouts() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        set_socket_timeouts(&listener, 200).unwrap();
        let _client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (connection, _) = listener.accept().unwrap();
        let timeout = Some(Duration::from_millis(200));
        assert_eq!(connection.read_timeout().unwrap(), timeout);
        assert_eq!(connection.write_timeout().unwrap(), timeout);
    }
}
#[test]
fn listen_port_must_be_nonzero() {
    assert!(Args::try_parse_from(["dism-app", "--port", "0"]).is_err());
    assert_eq!(
        Args::try_parse_from(["dism-app", "--port", "65535"])
            .unwrap()
            .port,
        65535
    );
}
