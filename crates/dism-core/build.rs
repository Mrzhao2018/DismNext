use flate2::{write::GzEncoder, Compression};
use std::env;
use std::fs::File;
use std::io;
use std::path::PathBuf;

fn main() -> io::Result<()> {
    let source =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../../catalog/rules.json");
    let packed = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("rules.json.gz");
    println!("cargo:rerun-if-changed={}", source.display());
    let mut encoder = GzEncoder::new(File::create(packed)?, Compression::best());
    io::copy(&mut File::open(source)?, &mut encoder)?;
    encoder.finish()?;
    Ok(())
}
