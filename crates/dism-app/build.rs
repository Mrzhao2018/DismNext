use flate2::{write::GzEncoder, Compression};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_files(&entry.path(), files)?;
        } else if kind.is_file() {
            files.push(entry.path());
        }
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let dist = manifest_dir.join("../../ui/dist");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let packed = out.join("assets");
    println!("cargo:rerun-if-changed={}", dist.display());
    if env::var("PROFILE").as_deref() == Ok("release") && !dist.join("index.html").is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Build the desktop UI first with npm run build in ui/",
        ));
    }
    let mut source = fs::File::create(out.join("embedded_ui.rs"))?;
    writeln!(source, "static EMBEDDED_UI_ASSETS: &[EmbeddedAsset] = &[")?;

    let mut files = Vec::new();
    if dist.is_dir() {
        collect_files(&dist, &mut files)?;
    }
    files.sort_unstable();
    for file in files {
        println!("cargo:rerun-if-changed={}", file.display());
        let relative = file.strip_prefix(&dist).unwrap();
        let route = format!("/{}", relative.to_string_lossy().replace('\\', "/"));
        let bytes = fs::read(&file)?;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&bytes)?;
        let gzip = encoder.finish()?;
        let compressed = gzip.len() < bytes.len();
        let bundled = packed.join(relative);
        fs::create_dir_all(bundled.parent().unwrap())?;
        fs::write(&bundled, if compressed { &gzip } else { &bytes })?;
        writeln!(source,
            "    EmbeddedAsset {{ route: {route:?}, bytes: include_bytes!({:?}), decoded_length: {:?} }},",
            bundled.to_string_lossy(), compressed.then_some(bytes.len()),
        )?;
    }
    writeln!(source, "];")
}
