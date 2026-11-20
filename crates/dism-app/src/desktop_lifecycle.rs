use anyhow::{ensure, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt::Write;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    GetLastError, SetLastError, ERROR_ALREADY_EXISTS, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::{
    GetLengthSid, GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{
    CreateEventExW, GetCurrentProcess, OpenProcessToken, SetEvent, WaitForSingleObject,
    EVENT_MODIFY_STATE, SYNCHRONIZATION_SYNCHRONIZE,
};

pub(crate) const WINDOW_PATH: &str = "/api/desktop/window";
const STARTUP_GRACE: Duration = Duration::from_secs(120);
const WINDOW_TTL: Duration = Duration::from_secs(180);
const EXIT_GRACE: Duration = Duration::from_secs(5);
const MAX_WINDOWS: usize = 64;

pub(crate) struct WakeEvent(OwnedHandle);

impl WakeEvent {
    pub(crate) fn launch_or_signal(port: u16, elevated: bool) -> Result<Option<Self>> {
        // The Local namespace scopes the signal to this Windows session. A signal
        // only asks the existing host to open its UI with a new launch token.
        let scope = if elevated { "elevated" } else { "standard" };
        let name = format!(r"Local\DismNext-{}-{scope}-{port}", process_user_id()?);
        let (event, existed) = Self::create(&name)?;
        if existed {
            ensure!(
                unsafe { SetEvent(event.0.as_raw_handle()) } != 0,
                io::Error::last_os_error()
            );
            Ok(None)
        } else {
            Ok(Some(event))
        }
    }

    fn create(name: &str) -> Result<(Self, bool)> {
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let (handle, existed) = unsafe {
            SetLastError(0);
            let raw = CreateEventExW(
                null(),
                name.as_ptr(),
                0,
                EVENT_MODIFY_STATE | SYNCHRONIZATION_SYNCHRONIZE,
            );
            if raw.is_null() {
                return Err(io::Error::last_os_error().into());
            }
            (
                OwnedHandle::from_raw_handle(raw),
                GetLastError() == ERROR_ALREADY_EXISTS,
            )
        };
        Ok((Self(handle), existed))
    }

    pub(crate) fn requested(&self) -> Result<bool> {
        match unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(io::Error::last_os_error().into()),
        }
    }
}

fn process_user_id() -> Result<String> {
    let mut handle = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(handle) };
    // TOKEN_USER and its SID fit in this aligned buffer.
    let mut info = [0usize; 32];
    let mut required = 0;
    let read = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            info.as_mut_ptr().cast(),
            std::mem::size_of_val(&info) as u32,
            &mut required,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let user = unsafe { &*info.as_ptr().cast::<TOKEN_USER>() };
    let sid = unsafe {
        std::slice::from_raw_parts(
            user.User.Sid.cast::<u8>(),
            GetLengthSid(user.User.Sid) as usize,
        )
    };
    let mut id = String::with_capacity(sid.len() * 2);
    for byte in sid {
        write!(id, "{byte:02x}")?;
    }
    Ok(id)
}

#[derive(Deserialize)]
pub(crate) struct WindowUpdate {
    id: String,
    open: bool,
    sequence: u64,
}

pub(crate) struct DesktopLifecycle(Mutex<WindowLeases>);

#[derive(Default)]
struct WindowLeases {
    launch_until: Option<Instant>,
    windows: HashMap<String, WindowLease>,
    idle_since: Option<Instant>,
}

struct WindowLease {
    updated: Instant,
    sequence: u64,
    open: bool,
}

impl DesktopLifecycle {
    pub(crate) fn new() -> Self {
        Self(Mutex::new(WindowLeases::default()))
    }

    pub(crate) fn launching(&self) {
        self.0.lock().unwrap().launch_until = Some(Instant::now() + STARTUP_GRACE);
    }

    pub(crate) fn update(&self, update: WindowUpdate) -> Result<()> {
        ensure!(
            update.id.len() == 36
                && update
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() || byte == b'-'),
            "Invalid desktop window ID"
        );
        self.0.lock().unwrap().update(update, Instant::now());
        Ok(())
    }

    pub(crate) fn idle(&self, has_work: bool) -> bool {
        self.0.lock().unwrap().idle(Instant::now(), has_work)
    }
}

impl WindowLeases {
    fn prune(&mut self, now: Instant) {
        self.windows
            .retain(|_, lease| now.saturating_duration_since(lease.updated) < WINDOW_TTL);
    }

    fn update(&mut self, update: WindowUpdate, now: Instant) {
        self.prune(now);
        let previous = self.windows.get(&update.id);
        if previous.is_some_and(|lease| lease.sequence >= update.sequence)
            || (previous.is_none() && self.windows.len() >= MAX_WINDOWS)
        {
            return;
        }
        if update.open {
            if previous.is_none_or(|lease| !lease.open) {
                self.launch_until = None;
            }
            self.idle_since = None;
        }
        self.windows.insert(
            update.id,
            WindowLease {
                updated: now,
                sequence: update.sequence,
                open: update.open,
            },
        );
    }

    fn idle(&mut self, now: Instant, has_work: bool) -> bool {
        self.prune(now);
        if has_work
            || self.windows.values().any(|lease| lease.open)
            || self.launch_until.is_some_and(|until| now < until)
        {
            self.idle_since = None;
            return false;
        }
        now.saturating_duration_since(*self.idle_since.get_or_insert(now)) >= EXIT_GRACE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_instance_signals_the_existing_host_once() {
        let name = format!(r"Local\DismNext-test-{}", std::process::id());
        let (first, existed) = WakeEvent::create(&name).unwrap();
        assert!(!existed);
        let (second, existed) = WakeEvent::create(&name).unwrap();
        assert!(existed);
        assert!(!first.requested().unwrap());
        assert_ne!(unsafe { SetEvent(second.0.as_raw_handle()) }, 0);
        assert!(first.requested().unwrap());
        assert!(!first.requested().unwrap());
    }

    #[test]
    fn closing_windows_and_finished_jobs_allow_shutdown() {
        let now = Instant::now();
        let mut leases = WindowLeases {
            launch_until: Some(now + STARTUP_GRACE),
            ..WindowLeases::default()
        };
        assert!(!leases.idle(now + EXIT_GRACE, false));
        for id in ["first", "second"] {
            leases.update(
                WindowUpdate {
                    id: id.into(),
                    open: true,
                    sequence: 1,
                },
                now + EXIT_GRACE,
            );
        }
        leases.update(
            WindowUpdate {
                id: "first".into(),
                open: false,
                sequence: 2,
            },
            now + EXIT_GRACE,
        );
        assert!(!leases.idle(now + EXIT_GRACE * 2, false));
        leases.update(
            WindowUpdate {
                id: "second".into(),
                open: false,
                sequence: 2,
            },
            now + EXIT_GRACE * 2,
        );
        leases.update(
            WindowUpdate {
                id: "second".into(),
                open: true,
                sequence: 1,
            },
            now + EXIT_GRACE * 2,
        );
        assert!(!leases.idle(now + EXIT_GRACE * 3, true));
        assert!(!leases.idle(now + EXIT_GRACE * 4, false));
        assert!(leases.idle(now + EXIT_GRACE * 5, false));
    }

    #[test]
    fn a_disconnected_window_expires_without_a_close_notification() {
        let now = Instant::now();
        let mut leases = WindowLeases::default();
        leases.update(
            WindowUpdate {
                id: "window".into(),
                open: true,
                sequence: 1,
            },
            now,
        );
        assert!(!leases.idle(now + WINDOW_TTL - Duration::from_secs(1), false));
        assert!(!leases.idle(now + WINDOW_TTL, false));
        assert!(leases.idle(now + WINDOW_TTL + EXIT_GRACE, false));
    }
}
