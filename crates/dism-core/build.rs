use flate2::{write::GzEncoder, Compression};
use std::env;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;

fn main() -> io::Result<()> {
    let source =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../../catalog/rules.json");
    let packed = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("rules.json.gz");
    println!("cargo:rerun-if-changed={}", source.display());
    let catalog = fs::read(source)?;
    let parsed: serde_json::Value = serde_json::from_slice(&catalog)?;
    if parsed["schema_version"].as_u64() != Some(1)
        || !parsed["cleanup_rules"].is_array()
        || !parsed["optimization_groups"].is_array()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid catalog schema",
        ));
    }
    let mut encoder = GzEncoder::new(File::create(packed)?, Compression::best());
    encoder.write_all(&catalog)?;
    encoder.finish()?;
    Ok(())
}
