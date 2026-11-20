use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read};
use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
use std::path::{Component, Path, PathBuf};
use tiny_http::{Header, Request, Response, StatusCode};
use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;

pub fn serve(req: Request, ui_dir: &Path, url_path: &str) {
    let rel = match relative_asset_path(url_path) {
        Some(rel) => rel,
        None => {
            let _ = req.respond(Response::empty(StatusCode(403)));
            return;
        }
    };
    match open_asset(ui_dir, rel) {
        Ok((mut file, path)) => {
            let mut bytes = Vec::new();
            if file.read_to_end(&mut bytes).is_err() {
                let _ = req.respond(Response::empty(StatusCode(404)));
                return;
            }
            let mut resp = Response::from_data(bytes);
            if let Ok(header) = Header::from_bytes("Content-Type", super::content_type_for(&path)) {
                resp.add_header(header);
            }
            let _ = req.respond(resp);
        }
        Err(error) => {
            let status = if error.kind() == io::ErrorKind::PermissionDenied {
                403
            } else {
                404
            };
            let _ = req.respond(Response::empty(StatusCode(status)));
        }
    }
}

fn relative_asset_path(url_path: &str) -> Option<&Path> {
    let rel = url_path.strip_prefix('/')?;
    if rel.is_empty() {
        return Some(Path::new("index.html"));
    }
    let path = Path::new(rel);
    (!rel.contains(['\\', ':', '\0'])
        && rel
            .split('/')
            .all(|part| !part.is_empty() && !part.ends_with(['.', ' ']))
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_))))
    .then_some(path)
}

fn open_asset(ui_dir: &Path, rel: &Path) -> io::Result<(File, PathBuf)> {
    let index = Path::new("index.html");
    let resolve = |rel: &Path| -> io::Result<PathBuf> {
        let canonical = ui_dir.join(rel).canonicalize()?;
        if !canonical.starts_with(ui_dir) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(canonical)
    };
    let canonical = match resolve(rel) {
        Ok(path) if path.is_dir() && rel != index => resolve(index)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound && rel != index => resolve(index)?,
        result => result?,
    };
    let file = File::open(canonical)?;
    // Verify the same open handle used for the response, including changes after canonicalization.
    let actual = final_path(&file)?;
    if !actual.starts_with(ui_dir) || !file.metadata()?.is_file() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok((file, actual))
}

fn final_path(file: &File) -> io::Result<PathBuf> {
    let mut buffer = vec![0; 260];
    loop {
        let len = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                0,
            )
        } as usize;
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        if len < buffer.len() {
            return Ok(PathBuf::from(OsString::from_wide(&buffer[..len])));
        }
        buffer.resize(len + 1, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_accepts_relative_asset_components() {
        for invalid in [
            "//assets/app.js",
            "/C:/outside.txt",
            "/assets\\app.js",
            "/../index.html",
            "/assets/. /index.html",
            "/index.html:stream",
        ] {
            assert!(relative_asset_path(invalid).is_none(), "{invalid}");
        }
        assert_eq!(relative_asset_path("/"), Some(Path::new("index.html")));
        assert_eq!(
            relative_asset_path("/assets/app.js"),
            Some(Path::new("assets/app.js"))
        );
    }
}
