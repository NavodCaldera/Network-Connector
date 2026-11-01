//! Turns `src/sum.wat` into `sum.wasm` and a precompiled Pulley artifact `sum.pulley`.
//! WAT parsing and compilation run on the build host, so neither ships in the `.so`.
use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=src/sum.wat");
    let out = PathBuf::from(env::var("OUT_DIR")?);
    let wasm = wat::parse_file("src/sum.wat")?;
    fs::write(out.join("sum.wasm"), &wasm)?;

    // Pulley bytecode depends only on pointer width and endianness: the x86_64 build host
    // and Android arm64 are both 64-bit little-endian, so `pulley64` serves both.
    // The engine config must match the runtime config in src/lib.rs exactly.
    let mut cfg = wasmtime::Config::new();
    cfg.target("pulley64")?;
    cfg.epoch_interruption(true);
    let engine = wasmtime::Engine::new(&cfg)?;
    fs::write(out.join("sum.pulley"), engine.precompile_module(&wasm)?)?;
    Ok(())
}
