use crate::state::AppState;
use anyhow::Result;
use std::io::Read;
use std::sync::Arc;
use tiny_http::Request;

mod applications;
pub(crate) mod cleanup;
mod disk;
mod drivers;
mod jobs;
mod optimization;
mod startup;
mod system;

pub(crate) const MAX_HTTP_BODY: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct HttpBodyTooLarge;

impl std::fmt::Display for HttpBodyTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP request body exceeds {MAX_HTTP_BODY} bytes")
    }
}

impl std::error::Error for HttpBodyTooLarge {}

pub(crate) fn dispatch(
    req: &mut Request,
    path: &str,
    query: &str,
    state: &Arc<AppState>,
) -> Result<serde_json::Value> {
    let route = path
        .trim_start_matches("/api/")
        .split('/')
        .next()
        .unwrap_or_default();
    match route {
        "maintenance" | "scans" => jobs::handle(req, path, query, state),
        "cleanup" | "report" => cleanup::handle(req, path, query, state),
        "optimizer" | "snapshots" => optimization::handle(req, path, state),
        "diskmap" | "lockcheck" => disk::handle(req, path, state),
        "startup" => startup::handle(req, path),
        "sys" | "sysinfo" | "features" | "systasks" | "rescue" => system::handle(req, path, state),
        "drivers" => drivers::handle(req, path, state),
        "appx" | "uninstaller" => applications::handle(req, path, state),
        _ => unknown_endpoint(path),
    }
}

fn unknown_endpoint(path: &str) -> Result<serde_json::Value> {
    Err(anyhow::anyhow!("Unknown API endpoint: {}", path))
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
