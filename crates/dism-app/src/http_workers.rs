use crate::desktop_lifecycle::WINDOW_PATH;
use crate::request_security::{header_value, SESSION_EXCHANGE_PATH};
use crate::state::AppState;
use crate::{handle_request, respond_json, ErrorPayload};
use anyhow::Result;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use tiny_http::{Request, StatusCode};

pub(crate) struct HttpWorkers {
    regular: SyncSender<Request>,
    control: SyncSender<Request>,
    pending: Arc<AtomicUsize>,
}

impl HttpWorkers {
    pub(crate) fn new(state: &Arc<AppState>) -> Result<Self> {
        let pending = Arc::new(AtomicUsize::new(0));
        Ok(Self {
            regular: start_workers("http", 8, 32, state, &pending)?,
            control: start_workers("http-control", 2, 16, state, &pending)?,
            pending,
        })
    }

    pub(crate) fn submit(&self, request: Request, state: &AppState) {
        let queue = if is_control_request(request.url()) {
            &self.control
        } else {
            &self.regular
        };
        self.pending.fetch_add(1, Ordering::Relaxed);
        if let Err(mpsc::TrySendError::Full(request) | mpsc::TrySendError::Disconnected(request)) =
            queue.try_send(request)
        {
            self.pending.fetch_sub(1, Ordering::Release);
            let origin = header_value(&request, "origin").map(str::to_owned);
            respond_json(
                request,
                StatusCode(503),
                &ErrorPayload {
                    error: "DismNext is busy. Try again shortly.".into(),
                },
                state,
                origin.as_deref(),
            );
        }
    }

    pub(crate) fn has_work(&self) -> bool {
        self.pending.load(Ordering::Acquire) != 0
    }
}

fn start_workers(
    name: &str,
    count: usize,
    queue_size: usize,
    state: &Arc<AppState>,
    pending: &Arc<AtomicUsize>,
) -> Result<SyncSender<Request>> {
    let (sender, receiver) = mpsc::sync_channel(queue_size);
    let receiver = Arc::new(Mutex::new(receiver));
    for index in 0..count {
        let state = Arc::clone(state);
        let receiver = Arc::clone(&receiver);
        let pending = Arc::clone(pending);
        std::thread::Builder::new()
            .name(format!("{name}-{index}"))
            .spawn(move || loop {
                let next = receiver.lock().unwrap().recv();
                let Ok(request) = next else {
                    break;
                };
                if catch_unwind(AssertUnwindSafe(|| handle_request(request, &state))).is_err() {
                    eprintln!("A desktop HTTP request stopped unexpectedly");
                }
                pending.fetch_sub(1, Ordering::Release);
            })?;
    }
    Ok(sender)
}

fn is_control_request(url: &str) -> bool {
    let path = url.split('?').next().unwrap_or_default();
    matches!(
        path,
        SESSION_EXCHANGE_PATH
            | WINDOW_PATH
            | "/api/scans/status"
            | "/api/scans/cancel"
            | "/api/maintenance/status"
            | "/api/maintenance/cancel"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_status_and_cancellation_have_a_separate_queue() {
        assert!(is_control_request("/api/scans/status?id=42"));
        assert!(is_control_request("/api/scans/cancel"));
        assert!(is_control_request(WINDOW_PATH));
        assert!(!is_control_request("/api/scans/start"));
        assert!(!is_control_request("/api/report/export"));
    }
}
