use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

fn collect_files(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut sorted: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    sorted.sort();
    for path in sorted {
        if path.is_dir() {
            collect_files(base, &path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

fn mime_for(path: &Path) -> &'static str {
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

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let dist_dir = manifest_dir.join("..").join("..").join("ui").join("dist");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let out_file = out_dir.join("embedded_ui.rs");

    println!("cargo:rerun-if-changed={}", dist_dir.display());

    let mut f = fs::File::create(&out_file).expect("failed to create embedded_ui.rs");
    writeln!(f, "pub static EMBEDDED_UI_ASSETS: &[(&str, &str, &[u8])] = &[").unwrap();

    if dist_dir.is_dir() {
        let mut files = Vec::new();
        collect_files(&dist_dir, &dist_dir, &mut files);
        for file in files {
            println!("cargo:rerun-if-changed={}", file.display());
            if let Ok(rel) = file.strip_prefix(&dist_dir) {
                let rel_slash = rel.to_string_lossy().replace('\\', "/");
                let route_path = format!("/{}", rel_slash.trim_start_matches('/'));
                let abs_path = fs::canonicalize(&file).unwrap_or(file.clone());
                let abs_clean = abs_path
                    .to_string_lossy()
                    .trim_start_matches(r"\\?\")
                    .to_string();
                let mime = mime_for(&file);
                writeln!(
                    f,
                    "    ({:?}, {:?}, include_bytes!({:?})),",
                    route_path, mime, abs_clean
                )
                .unwrap();
            }
        }
    }

    writeln!(f, "];").unwrap();
}
