use flate2::read::GzDecoder;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Cursor, Read};
use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
use std::path::{Component, Path, PathBuf};
use tiny_http::{Header, Request, Response, StatusCode};
use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;

struct EmbeddedAsset {
    route: &'static str,
    bytes: &'static [u8],
    decoded_length: Option<usize>,
}

include!(concat!(env!("OUT_DIR"), "/embedded_ui.rs"));

pub fn embedded_asset_count() -> usize {
    EMBEDDED_UI_ASSETS.len()
}

pub fn serve_embedded(req: Request, url_path: &str) {
    if url_path.contains("..") {
        let _ = req.respond(Response::empty(StatusCode(403)));
        return;
    }
    let asset = EMBEDDED_UI_ASSETS
        .iter()
        .find(|asset| asset.route == url_path)
        .or_else(|| {
            EMBEDDED_UI_ASSETS
                .iter()
                .find(|asset| asset.route == "/index.html")
        });
    let Some(asset) = asset else {
        let _ = req.respond(Response::empty(StatusCode(404)));
        return;
    };
    let encodings = req
        .headers()
        .iter()
        .filter(|header| header.field.equiv("Accept-Encoding"))
        .map(|header| header.value.as_str());
    let use_gzip = choose_encoding(encodings, asset.decoded_length.is_some());
    let Some(use_gzip) = use_gzip else {
        let response = Response::empty(StatusCode(406))
            .with_header(Header::from_bytes("Vary", "Accept-Encoding").unwrap());
        let _ = req.respond(response);
        return;
    };
    let (data, size): (Box<dyn Read + Send>, usize) = match asset.decoded_length {
        Some(length) if !use_gzip => (Box::new(GzDecoder::new(asset.bytes)), length),
        _ => (Box::new(Cursor::new(asset.bytes)), asset.bytes.len()),
    };
    let cache_control = if asset.route.starts_with("/assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mut headers = vec![
        Header::from_bytes("Content-Type", content_type_for(Path::new(asset.route))).unwrap(),
        Header::from_bytes("Cache-Control", cache_control).unwrap(),
        Header::from_bytes("Vary", "Accept-Encoding").unwrap(),
    ];
    if use_gzip {
        headers.push(Header::from_bytes("Content-Encoding", "gzip").unwrap());
    }
    let _ = req.respond(Response::new(
        StatusCode(200),
        headers,
        data,
        Some(size),
        None,
    ));
}

/// `Some(true)` selects gzip, `Some(false)` selects identity, and `None` is HTTP 406.
fn choose_encoding<'a>(values: impl IntoIterator<Item = &'a str>, has_gzip: bool) -> Option<bool> {
    let (mut gzip, mut identity, mut wildcard) = (None, None, None);
    for part in values.into_iter().flat_map(|value| value.split(',')) {
        let mut fields = part.split(';');
        let name = fields.next().unwrap_or_default().trim();
        let target = if name.eq_ignore_ascii_case("gzip") {
            &mut gzip
        } else if name.eq_ignore_ascii_case("identity") {
            &mut identity
        } else if name == "*" {
            &mut wildcard
        } else {
            continue;
        };
        let quality = fields
            .filter_map(|field| field.split_once('='))
            .find(|(key, _)| key.trim().eq_ignore_ascii_case("q"))
            .map_or(1.0, |(_, q)| {
                q.trim()
                    .parse::<f32>()
                    .ok()
                    .filter(|q| (0.0..=1.0).contains(q))
                    .unwrap_or(0.0)
            });
        *target = Some(quality);
    }
    let gzip = gzip.or(wildcard).unwrap_or(0.0);
    let identity_ok = identity.map_or(wildcard != Some(0.0), |quality| quality > 0.0);
    if has_gzip && gzip > 0.0 && identity.is_none_or(|quality| gzip >= quality) {
        Some(true)
    } else {
        identity_ok.then_some(false)
    }
}

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
            if let Ok(header) = Header::from_bytes("Content-Type", content_type_for(&path)) {
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
    fn negotiates_gzip_and_identity() {
        assert_eq!(choose_encoding([], true), Some(false));
        assert_eq!(choose_encoding(["br, GZIP"], true), Some(true));
        assert_eq!(choose_encoding(["gzip;q=0", "*;q=1"], true), Some(false));
        assert_eq!(
            choose_encoding(["gzip;q=0.4, identity;q=1"], true),
            Some(false)
        );
        assert_eq!(choose_encoding(["gzip, identity;q=0"], false), None);
    }

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
