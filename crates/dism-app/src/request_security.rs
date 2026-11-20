use anyhow::{ensure, Result};
use std::collections::VecDeque;
use std::fmt::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Request, Response};
use windows_sys::Win32::Security::Cryptography::{
    BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};

pub const SESSION_HEADER: &str = "X-DismNext-Token";
pub const SESSION_EXCHANGE_PATH: &str = "/api/session";
const BROWSER_LAUNCH_TTL: Duration = Duration::from_secs(120);

pub struct Session {
    api_token: String,
    browser_launch: Mutex<VecDeque<(String, Instant)>>,
}

impl Session {
    pub fn new() -> Result<Self> {
        Ok(Self {
            api_token: new_session_token()?,
            browser_launch: Mutex::default(),
        })
    }

    pub fn api_token(&self) -> &str {
        &self.api_token
    }

    pub fn exchange(&self, candidate: Option<&str>) -> Option<&str> {
        let mut launches = self.browser_launch.lock().ok()?;
        launches.retain(|(_, started)| started.elapsed() <= BROWSER_LAUNCH_TTL);
        let index = launches
            .iter()
            .position(|(expected, _)| token_matches(candidate, expected))?;
        launches.remove(index);
        Some(&self.api_token)
    }

    pub fn issue_browser_launch(&self) -> Result<String> {
        let token = new_session_token()?;
        let mut launches = self
            .browser_launch
            .lock()
            .map_err(|_| anyhow::anyhow!("Browser session state became unavailable"))?;
        launches.retain(|(_, started)| started.elapsed() <= BROWSER_LAUNCH_TTL);
        if launches.len() == 8 {
            launches.pop_front();
        }
        launches.push_back((token.clone(), Instant::now()));
        Ok(token)
    }
}

pub fn new_session_token() -> Result<String> {
    let mut random = [0; 32];
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            random.as_mut_ptr(),
            random.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    ensure!(
        status == 0,
        "Could not generate a session token (NTSTATUS {status:#x})"
    );
    let mut token = String::with_capacity(random.len() * 2);
    for byte in random {
        write!(token, "{byte:02x}")?;
    }
    Ok(token)
}

pub fn header_value<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    let mut values = req
        .headers()
        .iter()
        .filter(|header| header.field.as_str().as_str().eq_ignore_ascii_case(name));
    let value = values.next()?.value.as_str().trim();
    values.next().is_none().then_some(value)
}

pub fn is_allowed_host(host: &str, port: u16) -> bool {
    host == format!("127.0.0.1:{port}")
        || host.eq_ignore_ascii_case(&format!("localhost:{port}"))
        || (port == 80 && (host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost")))
}

pub fn is_allowed_origin(origin: &str, port: u16, dev_origin: bool) -> bool {
    origin.strip_prefix("http://").is_some_and(|host| {
        is_allowed_host(host, port)
            || (cfg!(debug_assertions) && dev_origin && is_allowed_host(host, 5173))
    })
}

pub fn verify_request(
    req: &Request,
    path: &str,
    port: u16,
    dev_origin: bool,
    session_token: &str,
) -> std::result::Result<(), &'static str> {
    if !header_value(req, "host").is_some_and(|host| is_allowed_host(host, port)) {
        return Err("Missing or untrusted Host header");
    }
    if req
        .headers()
        .iter()
        .any(|header| header.field.equiv("Origin"))
        && !header_value(req, "origin")
            .is_some_and(|origin| is_allowed_origin(origin, port, dev_origin))
    {
        return Err("Untrusted Origin header");
    }
    if req.method() == &Method::Options
        || !path.starts_with("/api/")
        || (req.method() == &Method::Post && path == SESSION_EXCHANGE_PATH)
    {
        return Ok(());
    }
    if !token_matches(header_value(req, SESSION_HEADER), session_token) {
        return Err("Missing or invalid DismNext session token");
    }
    Ok(())
}

fn token_matches(candidate: Option<&str>, expected: &str) -> bool {
    candidate.is_some_and(|candidate| {
        candidate.len() == expected.len()
            && candidate
                .bytes()
                .zip(expected.bytes())
                .fold(0, |diff, (a, b)| diff | (a ^ b))
                == 0
    })
}

pub fn add_cors_headers<R: std::io::Read>(
    response: &mut Response<R>,
    port: u16,
    dev_origin: bool,
    origin: Option<&str>,
) {
    let Some(origin) = origin.filter(|origin| is_allowed_origin(origin, port, dev_origin)) else {
        return;
    };
    for (name, value) in [
        ("Access-Control-Allow-Origin", origin),
        ("Vary", "Origin"),
        ("Access-Control-Allow-Methods", "GET, POST, OPTIONS"),
        (
            "Access-Control-Allow-Headers",
            "Content-Type, X-DismNext-Token",
        ),
    ] {
        if let Ok(header) = Header::from_bytes(name, value) {
            response.add_header(header);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_launch_can_be_exchanged_once() {
        let session = Session::new().unwrap();
        let launch = session.issue_browser_launch().unwrap();
        let reopen = session.issue_browser_launch().unwrap();
        assert!(session.exchange(Some(session.api_token())).is_none());
        assert_eq!(session.exchange(Some(&launch)), Some(session.api_token()));
        assert!(session.exchange(Some(&launch)).is_none());
        assert_eq!(session.exchange(Some(&reopen)), Some(session.api_token()));
    }
}
