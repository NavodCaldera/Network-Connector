//! Generate Rust types from `proto/` (the single source of truth, AGENT.md §1) with a vendored `protoc`,
//! so no system install is needed.

use std::io;
use std::path::PathBuf;

const FILES: &[&str] = &[
    "nc/v1/common.proto",
    "nc/v1/manifest.proto",
    "nc/v1/identity.proto",
];

fn main() -> io::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("proto");
    let protoc =
        protoc_bin_vendored::protoc_bin_path().map_err(|e| io::Error::other(e.to_string()))?;
    let mut cfg = prost_build::Config::new();
    cfg.protoc_executable(protoc);
    let files: Vec<PathBuf> = FILES.iter().map(|f| root.join(f)).collect();
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
    }
    cfg.compile_protos(&files, &[root])
}
