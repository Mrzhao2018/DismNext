use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use windows_sys::Win32::System::SystemInformation::{
    GetSystemDirectoryW, GetSystemWindowsDirectoryW,
};

pub fn windows_dir() -> io::Result<PathBuf> {
    system_path(GetSystemWindowsDirectoryW)
}

pub fn system32_dir() -> io::Result<PathBuf> {
    system_path(GetSystemDirectoryW)
}

fn system_path(query: unsafe extern "system" fn(*mut u16, u32) -> u32) -> io::Result<PathBuf> {
    let mut buffer = vec![0; 260];
    loop {
        let len = unsafe { query(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        if len < buffer.len() {
            return Ok(PathBuf::from(OsString::from_wide(&buffer[..len])));
        }
        buffer.resize(len + 1, 0);
    }
}

pub fn system32_command(exe: &str) -> io::Result<Command> {
    if exe.is_empty()
        || exe.contains(':')
        || !Path::new(exe)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Expected a relative System32 executable path",
        ));
    }
    let system32 = system32_dir()?;
    let mut command = Command::new(system32.join(exe));
    command.current_dir(&system32).env("PATH", &system32);
    Ok(command)
}

pub(crate) fn powershell_command() -> io::Result<Command> {
    let powershell = system32_dir()?.join(r"WindowsPowerShell\v1.0");
    let mut command = system32_command(r"WindowsPowerShell\v1.0\powershell.exe")?;
    command.env("PSModulePath", powershell.join("Modules"));
    Ok(command)
}
