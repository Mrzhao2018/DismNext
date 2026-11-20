use super::handle_request;
use crate::{cleaner::CleanupReviews, scan_progress::ScanProgress, Catalog};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::sync::{mpsc, Arc, Mutex};

pub(super) const MAX_MCP_BODY: usize = 16 * 1024 * 1024;
const MAX_MCP_HEADERS: usize = 8 * 1024;

pub(super) enum FramingMode {
    Newline,
    ContentLength,
}

/// Starts the Model Context Protocol (MCP) JSON-RPC 2.0 server over stdio.
/// Supports both newline-delimited JSON-RPC messages and `Content-Length` framed payloads.
pub fn run_mcp_stdio(catalog: &Catalog) -> Result<()> {
    let stdin = std::io::stdin();
    serve_messages(
        &mut BufReader::new(stdin.lock()),
        BufWriter::new(std::io::stdout()),
        catalog,
    )
}

struct PendingRequest {
    request: Value,
    framing: FramingMode,
    key: String,
    progress: Arc<ScanProgress>,
}

fn serve_messages(
    reader: &mut impl BufRead,
    writer: impl Write + Send,
    catalog: &Catalog,
) -> Result<()> {
    let writer = Mutex::new(writer);
    let active = Mutex::new(HashMap::<String, Arc<ScanProgress>>::new());
    std::thread::scope(|scope| {
        let (send, receive) = mpsc::sync_channel::<PendingRequest>(16);
        let worker = scope.spawn(|| -> Result<()> {
            let mut reviews = CleanupReviews::default();
            for message in receive {
                let reply =
                    handle_request(&message.request, catalog, &mut reviews, &message.progress);
                let cancelled = {
                    let mut active = active.lock().unwrap();
                    active.remove(&message.key);
                    message.progress.is_cancelled()
                };
                if let Some(reply) = reply.filter(|_| !cancelled) {
                    write_message(&mut *writer.lock().unwrap(), &reply, message.framing)?;
                }
            }
            Ok(())
        });
        let read_result = (|| -> Result<()> {
            while let Some((payload, framing)) = read_next_message(reader)? {
                let request: Value = match serde_json::from_str(&payload) {
                    Ok(request) => request,
                    Err(error) => {
                        let reply = rpc_error(Value::Null, -32700, format!("Parse error: {error}"));
                        write_message(&mut *writer.lock().unwrap(), &reply, framing)?;
                        continue;
                    }
                };
                if request["method"] == "notifications/cancelled" {
                    let key = request["params"]["requestId"].to_string();
                    if let Some(progress) = active.lock().unwrap().get(&key) {
                        progress.cancel();
                    }
                    continue;
                }
                let Some(id) = request.get("id") else {
                    continue;
                };
                let key = id.to_string();
                let progress = Arc::new(ScanProgress::default());
                {
                    let mut active = active.lock().unwrap();
                    if active.contains_key(&key) {
                        continue;
                    }
                    active.insert(key.clone(), Arc::clone(&progress));
                }
                let message = PendingRequest {
                    request,
                    framing,
                    key,
                    progress,
                };
                if let Err(
                    mpsc::TrySendError::Full(message) | mpsc::TrySendError::Disconnected(message),
                ) = send.try_send(message)
                {
                    active.lock().unwrap().remove(&message.key);
                    let reply =
                        rpc_error(message.request["id"].clone(), -32000, "DismNext is busy");
                    write_message(&mut *writer.lock().unwrap(), &reply, message.framing)?;
                }
            }
            Ok(())
        })();
        drop(send);
        let write_result = worker
            .join()
            .map_err(|_| anyhow!("MCP worker stopped unexpectedly"))?;
        read_result.and(write_result)
    })
}

fn rpc_error(id: Value, code: i32, message: impl std::fmt::Display) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.to_string() } })
}

pub(super) fn read_next_message<R: BufRead>(
    reader: &mut R,
) -> Result<Option<(String, FramingMode)>> {
    let mut first_line = String::new();
    loop {
        first_line.clear();
        let n = (&mut *reader)
            .take((MAX_MCP_BODY + 3) as u64)
            .read_line(&mut first_line)?;
        if n == 0 {
            return Ok(None);
        }
        let line = first_line.strip_suffix('\n').unwrap_or(&first_line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if n == MAX_MCP_BODY + 3 || line.len() > MAX_MCP_BODY {
            return Err(anyhow!("MCP message exceeds {MAX_MCP_BODY} bytes"));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if strip_ascii_prefix_ci(trimmed, "content-length:").is_some()
            || strip_ascii_prefix_ci(trimmed, "content-type:").is_some()
        {
            let mut content_len = None;
            let mut header_line = trimmed.to_owned();
            let mut remaining_headers = MAX_MCP_HEADERS
                .checked_sub(n)
                .ok_or_else(|| anyhow!("MCP headers exceed {MAX_MCP_HEADERS} bytes"))?;
            loop {
                if let Some(rest) = strip_ascii_prefix_ci(header_line.trim(), "content-length:") {
                    anyhow::ensure!(content_len.is_none(), "Duplicate MCP Content-Length header");
                    let length: usize = rest
                        .trim()
                        .parse()
                        .map_err(|_| anyhow!("Invalid MCP Content-Length header"))?;
                    anyhow::ensure!(
                        length <= MAX_MCP_BODY,
                        "MCP message exceeds {MAX_MCP_BODY} bytes"
                    );
                    content_len = Some(length);
                }
                header_line.clear();
                let hn = (&mut *reader)
                    .take(remaining_headers as u64 + 1)
                    .read_line(&mut header_line)?;
                if hn == 0 {
                    return Err(anyhow!("Unexpected EOF while reading MCP headers"));
                }
                remaining_headers = remaining_headers
                    .checked_sub(hn)
                    .ok_or_else(|| anyhow!("MCP headers exceed {MAX_MCP_HEADERS} bytes"))?;
                if header_line.trim().is_empty() {
                    break;
                }
            }

            let content_len =
                content_len.ok_or_else(|| anyhow!("Missing MCP Content-Length header"))?;
            let mut body = vec![0u8; content_len];
            reader.read_exact(&mut body)?;
            let text = String::from_utf8(body)?;
            return Ok(Some((text, FramingMode::ContentLength)));
        }

        return Ok(Some((trimmed.to_string(), FramingMode::Newline)));
    }
}

fn strip_ascii_prefix_ci<'a>(s: &'a str, prefix_lower: &str) -> Option<&'a str> {
    if s.len() < prefix_lower.len() {
        return None;
    }
    let head = s.get(..prefix_lower.len())?;
    if head.eq_ignore_ascii_case(prefix_lower) {
        Some(&s[prefix_lower.len()..])
    } else {
        None
    }
}

fn write_message<W: Write>(writer: &mut W, value: &Value, framing: FramingMode) -> Result<()> {
    let body = serde_json::to_string(value)?;
    match framing {
        FramingMode::Newline => {
            writer.write_all(body.as_bytes())?;
            writer.write_all(b"\n")?;
        }
        FramingMode::ContentLength => {
            write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
        }
    }
    writer.flush()?;
    Ok(())
}
