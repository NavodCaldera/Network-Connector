//! Phase 0 task 0.9a spike: embed a Wasm runtime in an Android `.so` and run a tiny
//! compute module (`src/sum.wat`). One runtime per build, chosen by Cargo feature.
//!
//! Stop: `stop()` sets a flag that is checked between chunks of work, and for Wasmtime also
//! bumps the engine epoch so a chunk that is running traps at once. This is a spike, not
//! `nc-exec`: no fuel/memory caps, no manifest checks.

use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(not(any(feature = "rt-pulley", feature = "rt-cranelift", feature = "rt-wasmi")))]
compile_error!("enable exactly one of rt-pulley, rt-cranelift, rt-wasmi");

static STOP: AtomicBool = AtomicBool::new(false);

/// Iterations per call into Wasm; the stop flag is checked between calls.
pub const CHUNK: i64 = 1_000_000;

/// Spike error, kept as text.
#[derive(Debug, PartialEq, Eq)]
pub enum SpikeError {
    /// `stop()` was called.
    Stopped,
    /// The runtime failed (load, instantiate or trap).
    Runtime(String),
}

impl std::fmt::Display for SpikeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpikeError::Stopped => write!(f, "stopped"),
            SpikeError::Runtime(e) => write!(f, "runtime error: {e}"),
        }
    }
}

impl std::error::Error for SpikeError {}

fn rt_err(e: impl std::fmt::Display) -> SpikeError {
    SpikeError::Runtime(e.to_string())
}

/// Which runtime this build contains: 1 = Wasmtime+Pulley, 2 = Wasmtime+Cranelift, 3 = wasmi.
pub fn runtime_id() -> i32 {
    if cfg!(feature = "rt-pulley") {
        1
    } else if cfg!(feature = "rt-cranelift") {
        2
    } else {
        3
    }
}

/// Human-readable runtime name.
pub fn runtime_name() -> &'static str {
    match runtime_id() {
        1 => "wasmtime-pulley",
        2 => "wasmtime-cranelift",
        _ => "wasmi",
    }
}

/// Ask a running `run_sum` to stop. Safe to call from any thread.
pub fn stop() {
    STOP.store(true, Ordering::SeqCst);
    #[cfg(any(feature = "rt-pulley", feature = "rt-cranelift"))]
    if let Some(Ok(rt)) = wt::RUNTIME.get() {
        rt.engine.increment_epoch();
    }
}

/// Sum of `0..n` computed inside Wasm, in chunks of [`CHUNK`].
pub fn run_sum(n: i64) -> Result<i64, SpikeError> {
    STOP.store(false, Ordering::SeqCst);
    let mut acc: i64 = 0;
    let mut start = 0;
    while start < n {
        if STOP.load(Ordering::SeqCst) {
            return Err(SpikeError::Stopped);
        }
        let end = start.saturating_add(CHUNK).min(n);
        acc = acc.wrapping_add(call_sum(start, end)?);
        start = end;
    }
    Ok(acc)
}

#[cfg(any(feature = "rt-pulley", feature = "rt-cranelift"))]
mod wt {
    use super::{rt_err, SpikeError, STOP};
    use std::sync::{atomic::Ordering, OnceLock};
    use wasmtime::{Config, Engine, Module, Store};

    pub(crate) struct Runtime {
        pub(crate) engine: Engine,
        module: Module,
    }

    pub(crate) static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();

    #[cfg(feature = "rt-pulley")]
    fn load() -> Result<Runtime, String> {
        static PULLEY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sum.pulley"));
        let mut cfg = Config::new();
        cfg.target("pulley64").map_err(|e| e.to_string())?;
        cfg.epoch_interruption(true);
        let engine = Engine::new(&cfg).map_err(|e| e.to_string())?;
        // SAFETY: the bytes were produced by build.rs with the same pinned Wasmtime version
        // and the same Config, and are embedded at compile time (never downloaded). Pulley
        // bytecode is interpreted, not executed as native code.
        let module = unsafe { Module::deserialize(&engine, PULLEY) }.map_err(|e| e.to_string())?;
        Ok(Runtime { engine, module })
    }

    #[cfg(all(feature = "rt-cranelift", not(feature = "rt-pulley")))]
    fn load() -> Result<Runtime, String> {
        static WASM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sum.wasm"));
        let mut cfg = Config::new();
        cfg.epoch_interruption(true);
        let engine = Engine::new(&cfg).map_err(|e| e.to_string())?;
        let module = Module::new(&engine, WASM).map_err(|e| e.to_string())?;
        Ok(Runtime { engine, module })
    }

    pub(crate) fn call_sum(start: i64, end: i64) -> Result<i64, SpikeError> {
        let rt = RUNTIME.get_or_init(load).as_ref().map_err(rt_err)?;
        let mut store = Store::new(&rt.engine, ());
        store.set_epoch_deadline(1);
        store.epoch_deadline_trap();
        let instance = wasmtime::Instance::new(&mut store, &rt.module, &[]).map_err(rt_err)?;
        let sum = instance
            .get_typed_func::<(i64, i64), i64>(&mut store, "sum")
            .map_err(rt_err)?;
        match sum.call(&mut store, (start, end)) {
            Ok(v) => Ok(v),
            Err(_) if STOP.load(Ordering::SeqCst) => Err(SpikeError::Stopped),
            Err(e) => Err(rt_err(e)),
        }
    }
}

#[cfg(any(feature = "rt-pulley", feature = "rt-cranelift"))]
use wt::call_sum;

#[cfg(all(feature = "rt-wasmi", not(any(feature = "rt-pulley", feature = "rt-cranelift"))))]
fn call_sum(start: i64, end: i64) -> Result<i64, SpikeError> {
    use std::sync::OnceLock;
    static WASM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sum.wasm"));
    static RT: OnceLock<Result<(wasmi::Engine, wasmi::Module), String>> = OnceLock::new();
    let (engine, module) = RT
        .get_or_init(|| {
            let engine = wasmi::Engine::default();
            let module = wasmi::Module::new(&engine, WASM).map_err(|e| e.to_string())?;
            Ok((engine, module))
        })
        .as_ref()
        .map_err(rt_err)?;
    let mut store = wasmi::Store::new(engine, ());
    let linker = wasmi::Linker::<()>::new(engine);
    let instance = linker
        .instantiate_and_start(&mut store, module)
        .map_err(rt_err)?;
    let sum = instance
        .get_typed_func::<(i64, i64), i64>(&store, "sum")
        .map_err(rt_err)?;
    sum.call(&mut store, (start, end)).map_err(rt_err)
}

/// JNI entry points for `lk.networkconnector.spike.NativeBridge`. Primitive arguments only,
/// so no JNIEnv calls are needed and no `unsafe` code is required here.
pub mod jni {
    use std::ffi::c_void;

    /// Status for `nativeRun` when the job was stopped.
    pub const STATUS_STOPPED: i64 = i64::MIN;
    /// Status for `nativeRun` when the runtime failed.
    pub const STATUS_ERROR: i64 = i64::MIN + 1;

    /// `static native long nativeRun(long n)`: sum of 0..n, or a STATUS_* value.
    #[no_mangle]
    pub extern "system" fn Java_lk_networkconnector_spike_NativeBridge_nativeRun(
        _env: *mut c_void,
        _class: *mut c_void,
        n: i64,
    ) -> i64 {
        match super::run_sum(n) {
            Ok(v) => v,
            Err(super::SpikeError::Stopped) => STATUS_STOPPED,
            Err(super::SpikeError::Runtime(_)) => STATUS_ERROR,
        }
    }

    /// `static native void nativeStop()`.
    #[no_mangle]
    pub extern "system" fn Java_lk_networkconnector_spike_NativeBridge_nativeStop(
        _env: *mut c_void,
        _class: *mut c_void,
    ) {
        super::stop();
    }

    /// `static native int nativeRuntime()`: 1 pulley, 2 cranelift, 3 wasmi.
    #[no_mangle]
    pub extern "system" fn Java_lk_networkconnector_spike_NativeBridge_nativeRuntime(
        _env: *mut c_void,
        _class: *mut c_void,
    ) -> i32 {
        super::runtime_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    // STOP is global; serialise tests.
    static LOCK: Mutex<()> = Mutex::new(());

    fn expected(n: i64) -> i64 {
        (0..n).fold(0i64, |a, i| a.wrapping_add(i))
    }

    #[test]
    fn sums_match_native() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        for n in [0, 1, 10, CHUNK + 7, 3 * CHUNK] {
            assert_eq!(run_sum(n), Ok(expected(n)), "{} n={n}", runtime_name());
        }
    }

    #[test]
    fn stop_halts_within_one_second() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let worker = std::thread::spawn(|| run_sum(i64::MAX));
        std::thread::sleep(Duration::from_millis(200));
        let t = Instant::now();
        stop();
        let res = worker.join().unwrap_or(Err(SpikeError::Runtime("panic".into())));
        let took = t.elapsed();
        assert_eq!(res, Err(SpikeError::Stopped));
        assert!(took < Duration::from_secs(1), "stop took {took:?}");
    }
}
