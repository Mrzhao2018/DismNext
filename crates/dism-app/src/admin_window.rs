use anyhow::{anyhow, ensure, Context, Result};
use std::cell::RefCell;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::{
    AddScriptToExecuteOnDocumentCreatedCompletedHandler, CoTaskMemPWSTR,
    CoreWebView2EnvironmentOptions, CreateCoreWebView2ControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, NavigationStartingEventHandler,
    NewWindowRequestedEventHandler,
};
use windows::core::{w, Interface, GUID, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_sys::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenIntegrityLevel,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
use windows_sys::Win32::System::Threading::{
    OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};

const ACTIVATE: u32 = WM_APP + 1;
const REPARSE_POINT: u32 = 0x400;
const HIGH_INTEGRITY: u32 = 0x3000;
const PROFILE_ACL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)S:(ML;OICI;NRNW;;;HI)";

thread_local! {
    static CONTROLLER: RefCell<Option<ICoreWebView2Controller>> = const { RefCell::new(None) };
}

pub(crate) struct AdminDesktop {
    profile: PathBuf,
    runtime: PathBuf,
    window: Option<(usize, JoinHandle<()>)>,
}

impl AdminDesktop {
    pub(crate) fn new() -> Result<Self> {
        let program_files = known_folder(&FOLDERID_ProgramFiles)?;
        let runtime = machine_runtime()?;
        let nonce = crate::request_security::new_session_token()?;
        let profile = program_files.join(format!("DismNext-WebView2-{nonce}"));
        check_directory(&program_files)?;
        let wide = wide_path(&profile);
        crate::desktop_security::with_security(PROFILE_ACL, |attributes| {
            if unsafe { CreateDirectoryW(wide.as_ptr(), attributes) } == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        })
        .context("Could not create the protected WebView2 profile")?;

        // Clear inherited debugger and runtime options for older Runtime builds.
        // Supply the profile and runtime through the native API.
        for (name, _) in std::env::vars_os() {
            let key = name.to_string_lossy().to_ascii_uppercase();
            if key.starts_with("WEBVIEW2_") || key.starts_with("COREWEBVIEW2_") {
                std::env::remove_var(name);
            }
        }
        Ok(Self {
            profile,
            runtime,
            window: None,
        })
    }

    pub(crate) fn open(&mut self, url: &str, token: &str) -> Result<()> {
        if let Some((handle, thread)) = self.window.take() {
            if !thread.is_finished() {
                let window = HWND(handle as _);
                let mut pid = 0;
                if unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) } != 0
                    && pid == std::process::id()
                {
                    let activated =
                        unsafe { PostMessageW(Some(window), ACTIVATE, WPARAM(0), LPARAM(0)) };
                    self.window = Some((handle, thread));
                    activated?;
                    return Ok(());
                }
            }
            let _ = thread.join();
        }
        let (ready, receiver) = mpsc::sync_channel(1);
        let url = url.to_owned();
        let token = token.to_owned();
        let profile = self.profile.clone();
        let runtime = self.runtime.clone();
        let thread = std::thread::Builder::new()
            .name("dismnext-webview2".into())
            .spawn(move || {
                if let Err(error) =
                    window_loop(&url, &token, &profile, &runtime, HIGH_INTEGRITY, &ready)
                {
                    let _ = ready.try_send(Err(error));
                }
            })?;
        let handle = receiver
            .recv_timeout(std::time::Duration::from_secs(30))
            .context("The administrator window stopped during startup")??;
        self.window = Some((handle, thread));
        Ok(())
    }

    pub(crate) fn window_open(&self) -> bool {
        self.window
            .as_ref()
            .is_some_and(|(_, thread)| !thread.is_finished())
    }
}

impl Drop for AdminDesktop {
    fn drop(&mut self) {
        if let Some((handle, thread)) = self.window.take() {
            let window = HWND(handle as _);
            let mut pid = 0;
            if !thread.is_finished()
                && unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) } != 0
                && pid == std::process::id()
            {
                let _ = unsafe { PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0)) };
            }
            let _ = thread.join();
        }
        let _ = fs::remove_dir_all(&self.profile);
    }
}

fn known_folder(id: &GUID) -> Result<PathBuf> {
    let path = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None)? };
    let value = PathBuf::from(OsString::from_wide(unsafe { path.as_wide() }));
    unsafe { CoTaskMemFree(Some(path.0.cast())) };
    Ok(value)
}

pub(crate) fn machine_runtime() -> Result<PathBuf> {
    trusted_runtime(
        &known_folder(&FOLDERID_ProgramFiles)?,
        &known_folder(&FOLDERID_ProgramFilesX86)?,
    )
}

fn check_directory(path: &Path) -> Result<()> {
    for folder in path.ancestors() {
        let metadata = folder.symlink_metadata()?;
        ensure!(
            metadata.is_dir() && metadata.file_attributes() & REPARSE_POINT == 0,
            "WebView2 requires a local directory without redirects: {}",
            folder.display()
        );
    }
    Ok(())
}

fn trusted_runtime(program_files: &Path, program_files_x86: &Path) -> Result<PathBuf> {
    let mut candidates = Vec::new();
    for root in [program_files_x86, program_files] {
        let base = root.join(r"Microsoft\EdgeWebView\Application");
        if check_directory(&base).is_err() {
            continue;
        }
        for entry in fs::read_dir(base)?.flatten() {
            let version = entry
                .file_name()
                .to_string_lossy()
                .split('.')
                .map(str::parse::<u32>)
                .collect::<std::result::Result<Vec<_>, _>>();
            let Ok(version) = version else { continue };
            if version.len() != 4 {
                continue;
            }
            let folder = entry.path();
            let executable = folder.join("msedgewebview2.exe");
            if check_directory(&folder).is_ok()
                && executable.symlink_metadata().is_ok_and(|entry| {
                    entry.is_file() && entry.file_attributes() & REPARSE_POINT == 0
                })
            {
                candidates.push((version, folder));
            }
        }
    }
    if let Some((_, folder)) = candidates
        .into_iter()
        .max_by(|left, right| left.0.cmp(&right.0))
    {
        return Ok(folder);
    }
    let system = dism_core::system_tools::system32_dir()?.join("Microsoft-Edge-WebView");
    if check_directory(&system).is_ok()
        && system
            .join("msedgewebview2.exe")
            .symlink_metadata()
            .is_ok_and(|entry| entry.is_file() && entry.file_attributes() & REPARSE_POINT == 0)
    {
        return Ok(system);
    }
    Err(anyhow!(
        "Administrator mode requires the machine-wide Microsoft Edge WebView2 Runtime"
    ))
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn window_loop(
    url: &str,
    token: &str,
    profile: &Path,
    runtime: &Path,
    integrity: u32,
    ready: &mpsc::SyncSender<Result<usize>>,
) -> Result<()> {
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
    let _apartment = Apartment;
    let module = HINSTANCE(unsafe { GetModuleHandleW(None)? }.0);
    let class = w!("DismNextAdminDesktop");
    let wnd_class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: module,
        lpszClassName: class,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        ..Default::default()
    };
    if unsafe { RegisterClassW(&wnd_class) } == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(1410) {
            return Err(error.into());
        }
    }
    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("DismNext • Administrator"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1320,
            860,
            None,
            None,
            Some(module),
            None,
        )?
    };
    let result = initialize_webview(window, url, token, profile, runtime, integrity);
    if result.is_err() {
        let _ = unsafe { DestroyWindow(window) };
    }
    result?;
    if ready.send(Ok(window.0 as usize)).is_err() {
        let _ = unsafe { DestroyWindow(window) };
        return Ok(());
    }
    if integrity >= HIGH_INTEGRITY {
        let _ = unsafe { ShowWindow(window, SW_SHOW) };
    }
    let mut msg = MSG::default();
    loop {
        match unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 {
            0 => return Ok(()),
            -1 => return Err(io::Error::last_os_error().into()),
            _ => unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            },
        }
    }
}

fn initialize_webview(
    window: HWND,
    origin: &str,
    token: &str,
    profile: &Path,
    runtime: &Path,
    integrity: u32,
) -> Result<()> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let profile_arg = wide_path(profile);
    let runtime_arg = wide_path(runtime);
    let options: ICoreWebView2EnvironmentOptions = CoreWebView2EnvironmentOptions::default().into();
    unsafe { options.SetAdditionalBrowserArguments(w!("--disable-extensions"))? };
    CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| unsafe {
            CreateCoreWebView2EnvironmentWithOptions(
                PCWSTR(runtime_arg.as_ptr()),
                PCWSTR(profile_arg.as_ptr()),
                &options,
                &handler,
            )
            .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(move |status, env| {
            status?;
            let _ = sender.send(env);
            Ok(())
        }),
    )?;
    let env = receiver
        .recv()?
        .context("WebView2 did not create an environment")?;
    let mut effective = PWSTR::null();
    unsafe {
        env.cast::<ICoreWebView2Environment7>()?
            .UserDataFolder(&mut effective)?
    };
    let actual = PathBuf::from(CoTaskMemPWSTR::from(effective).to_string());
    ensure!(
        actual.canonicalize()? == profile.canonicalize()?,
        "WebView2 did not use the protected profile"
    );
    let (sender, receiver) = mpsc::sync_channel(1);
    CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| unsafe {
            env.CreateCoreWebView2Controller(window, &handler)
                .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(move |status, controller| {
            status?;
            let _ = sender.send(controller);
            Ok(())
        }),
    )?;
    let controller = receiver
        .recv()?
        .context("WebView2 did not create a controller")?;
    let view = unsafe { controller.CoreWebView2()? };
    verify_browser_process(&view, runtime, integrity)?;
    let url = format!("{origin}/");
    let allowed = url.clone();
    unsafe {
        let settings = view.Settings()?;
        settings.SetAreDefaultContextMenusEnabled(false)?;
        settings.SetAreDevToolsEnabled(false)?;
        settings
            .cast::<ICoreWebView2Settings3>()?
            .SetAreBrowserAcceleratorKeysEnabled(false)?;
        settings.SetAreHostObjectsAllowed(false)?;
        settings.SetIsWebMessageEnabled(false)?;
        view.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let mut actual = windows::core::PWSTR::null();
                    args.Uri(&mut actual)?;
                    args.SetCancel(CoTaskMemPWSTR::from(actual).to_string() != allowed)?;
                }
                Ok(())
            })),
            &mut 0,
        )?;
        view.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(|_, args| {
                if let Some(args) = args {
                    args.SetHandled(true)?;
                }
                Ok(())
            })),
            &mut 0,
        )?;
    }
    let script = HSTRING::from(format!(
        "if(window===window.top&&location.origin==={})sessionStorage.setItem('dismnext-session-token',{});",
        serde_json::to_string(origin)?, serde_json::to_string(token)?
    ));
    let script_view = view.clone();
    AddScriptToExecuteOnDocumentCreatedCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| unsafe {
            script_view
                .AddScriptToExecuteOnDocumentCreated(&script, &handler)
                .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(|status, _| status),
    )?;
    resize(window, &controller);
    unsafe {
        controller.SetIsVisible(true)?;
        view.Navigate(&HSTRING::from(url))?;
    }
    CONTROLLER.with(|slot| slot.replace(Some(controller)));
    Ok(())
}

fn verify_browser_process(view: &ICoreWebView2, runtime: &Path, integrity: u32) -> Result<()> {
    let mut pid = 0;
    unsafe { view.BrowserProcessId(&mut pid)? };
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return Err(io::Error::last_os_error().into());
    }
    let process = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut path = vec![0; 32768];
    let mut len = path.len() as u32;
    if unsafe {
        QueryFullProcessImageNameW(process.as_raw_handle(), 0, path.as_mut_ptr(), &mut len)
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    let executable = PathBuf::from(OsString::from_wide(&path[..len as usize]));
    ensure!(
        executable.canonicalize()? == runtime.join("msedgewebview2.exe").canonicalize()?,
        "WebView2 used a process outside the machine runtime"
    );
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut label = [0usize; 32];
    let mut required = 0;
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenIntegrityLevel,
            label.as_mut_ptr().cast(),
            size_of_val(&label) as u32,
            &mut required,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    let sid = unsafe { (*label.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()).Label.Sid };
    let count = unsafe { *GetSidSubAuthorityCount(sid) };
    ensure!(
        count > 0 && unsafe { *GetSidSubAuthority(sid, u32::from(count - 1)) } >= integrity,
        "WebView2 browser process did not retain the required integrity"
    );
    Ok(())
}

fn resize(window: HWND, controller: &ICoreWebView2Controller) {
    let mut bounds = RECT::default();
    if unsafe { GetClientRect(window, &mut bounds) }.is_ok() {
        let _ = unsafe { controller.SetBounds(bounds) };
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_SIZE => {
            let controller = CONTROLLER.with(|slot| slot.borrow().clone());
            if let Some(controller) = controller {
                resize(window, &controller);
            }
        }
        WM_DESTROY => {
            CONTROLLER.with(|slot| {
                if let Some(controller) = slot.take() {
                    let _ = unsafe { controller.Close() };
                }
            });
            unsafe { PostQuitMessage(0) };
        }
        ACTIVATE => unsafe {
            let _ = ShowWindow(window, SW_RESTORE);
            let _ = SetForegroundWindow(window);
        },
        _ => return unsafe { DefWindowProcW(window, msg, wparam, lparam) },
    }
    LRESULT(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Requires a machine-wide WebView2 Runtime and an interactive Windows session"]
    fn machine_webview_opens_with_a_separate_profile() {
        let profile = std::env::temp_dir().join(format!(
            "dismnext-native-webview-{}",
            crate::request_security::new_session_token().unwrap()
        ));
        fs::create_dir(&profile).unwrap();
        let runtime = trusted_runtime(
            &known_folder(&FOLDERID_ProgramFiles).unwrap(),
            &known_folder(&FOLDERID_ProgramFilesX86).unwrap(),
        )
        .unwrap();
        let (ready, host) = mpsc::sync_channel(1);
        let profile_arg = profile.clone();
        let thread = std::thread::spawn(move || {
            if let Err(error) = window_loop(
                "http://127.0.0.1:1",
                "webview-fixture",
                &profile_arg,
                &runtime,
                0x2000,
                &ready,
            ) {
                let _ = ready.try_send(Err(error));
            }
        });
        let result = host
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap();
        if let Ok(handle) = &result {
            unsafe {
                PostMessageW(Some(HWND(*handle as _)), WM_CLOSE, WPARAM(0), LPARAM(0)).unwrap()
            };
        }
        thread.join().unwrap();
        for _ in 0..10 {
            if fs::remove_dir_all(&profile).is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        result.unwrap();
    }
}
