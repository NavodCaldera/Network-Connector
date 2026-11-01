//! Wasmtime runtime for profile `cpu-core-v1`. Runs **inside** the `nc-exec-worker` process only.
//!
//! Profile `cpu-core-v1`:
//! - core Wasm module (no components, no WASI, no threads);
//! - exports `memory` and `run: () -> i32` (0 = success);
//! - may import only these functions from module `nc` (anything else is refused before linking):
//!   - `input_size() -> i64`
//!   - `input_read(offset: i64, ptr: i32, len: i32) -> i32` (bytes copied, or -1 on a bad range)
//!   - `output_write(ptr: i32, len: i32) -> i32` (0, or traps when the output cap is hit)
//!   - `checkpoint(ptr: i32, len: i32) -> i32` (0; replaces the previous checkpoint; traps over cap)
//!
//! Limits: fuel, epoch deadline (ticked by a thread here), store limiter for memory/tables/instances,
//! `max_wasm_stack`, and output/checkpoint caps in the host functions. The parent adds a wall-clock watchdog and
//! the OS sandbox on top; this module is never the only layer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use wasmtime::{
    Caller, Config, Engine, ExternType, Linker, Module, ResourceLimiter, Store, Trap, ValType,
};

use crate::ipc::{WorkerRequest, WorkerResponse, WorkerStatus};

/// Name of the only import module a `cpu-core-v1` job may use.
pub const HOST_MODULE: &str = "nc";
/// Epoch tick period used for the worker-side wall clock.
pub const EPOCH_TICK: Duration = Duration::from_millis(10);

/// Allowed imports: (name, params, results).
const ALLOWED: &[(&str, &[ValType], &[ValType])] = &[
    ("input_size", &[], &[ValType::I64]),
    (
        "input_read",
        &[ValType::I64, ValType::I32, ValType::I32],
        &[ValType::I32],
    ),
    (
        "output_write",
        &[ValType::I32, ValType::I32],
        &[ValType::I32],
    ),
    ("checkpoint", &[ValType::I32, ValType::I32], &[ValType::I32]),
];

/// Error raised from a host function to stop the job with a specific status.
#[derive(Debug)]
struct HostStop(WorkerStatus, &'static str);

impl std::fmt::Display for HostStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.1)
    }
}

impl std::error::Error for HostStop {}

struct Limiter {
    memory_bytes: usize,
    hit: bool,
}

impl ResourceLimiter for Limiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > self.memory_bytes {
            self.hit = true;
            return Err(wasmtime::Error::new(HostStop(
                WorkerStatus::MemoryLimit,
                "memory limit",
            )));
        }
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > 100_000 {
            self.hit = true;
            return Err(wasmtime::Error::new(HostStop(
                WorkerStatus::MemoryLimit,
                "table limit",
            )));
        }
        Ok(true)
    }

    fn instances(&self) -> usize {
        1
    }

    fn tables(&self) -> usize {
        1
    }

    fn memories(&self) -> usize {
        1
    }
}

struct State {
    limiter: Limiter,
    input: Vec<u8>,
    output: Vec<u8>,
    checkpoint: Vec<u8>,
    output_cap: usize,
    checkpoint_cap: usize,
}

fn refuse(message: impl Into<String>) -> WorkerResponse {
    WorkerResponse {
        status: WorkerStatus::Refused,
        exit_code: -1,
        fuel_used: 0,
        compile_us: 0,
        run_us: 0,
        output: Vec::new(),
        checkpoint: Vec::new(),
        message: message.into(),
    }
}

fn same_types(got: impl ExactSizeIterator<Item = ValType>, want: &[ValType]) -> bool {
    got.len() == want.len() && got.zip(want).all(|(g, w)| ValType::eq(&g, w))
}

/// Check every import against the `cpu-core-v1` allowlist. Returns the first offending import's name.
pub fn check_imports(module: &Module) -> Result<(), String> {
    for imp in module.imports() {
        let ok = imp.module() == HOST_MODULE
            && match imp.ty() {
                ExternType::Func(f) => ALLOWED.iter().any(|(n, p, r)| {
                    *n == imp.name() && same_types(f.params(), p) && same_types(f.results(), r)
                }),
                _ => false,
            };
        if !ok {
            return Err(format!(
                "import not allowed: {}.{}",
                imp.module(),
                imp.name()
            ));
        }
    }
    Ok(())
}

fn engine(stack_bytes: u64) -> wasmtime::Result<Engine> {
    let mut cfg = Config::new();
    cfg.consume_fuel(true)
        .epoch_interruption(true)
        .max_wasm_stack(usize::try_from(stack_bytes).unwrap_or(usize::MAX))
        .wasm_multi_memory(false)
        .wasm_memory64(false);
    Engine::new(&cfg)
}

fn memory_of<'a>(caller: &mut Caller<'a, State>) -> wasmtime::Result<wasmtime::Memory> {
    caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| wasmtime::Error::new(HostStop(WorkerStatus::Trap, "no memory export")))
}

fn link(linker: &mut Linker<State>) -> wasmtime::Result<()> {
    linker.func_wrap(HOST_MODULE, "input_size", |c: Caller<'_, State>| {
        i64::try_from(c.data().input.len()).unwrap_or(i64::MAX)
    })?;
    linker.func_wrap(
        HOST_MODULE,
        "input_read",
        |mut c: Caller<'_, State>, offset: i64, ptr: i32, len: i32| -> wasmtime::Result<i32> {
            let mem = memory_of(&mut c)?;
            let (Ok(off), Ok(ptr), Ok(len)) = (
                usize::try_from(offset),
                usize::try_from(ptr as u32),
                usize::try_from(len),
            ) else {
                return Ok(-1);
            };
            let (data, st) = mem.data_and_store_mut(&mut c);
            if off > st.input.len() {
                return Ok(-1);
            }
            let n = len.min(st.input.len() - off);
            let Some(dst) = ptr.checked_add(n).and_then(|end| data.get_mut(ptr..end)) else {
                return Ok(-1);
            };
            dst.copy_from_slice(&st.input[off..off + n]);
            Ok(i32::try_from(n).unwrap_or(-1))
        },
    )?;
    linker.func_wrap(
        HOST_MODULE,
        "output_write",
        |mut c: Caller<'_, State>, ptr: i32, len: i32| -> wasmtime::Result<i32> {
            let mem = memory_of(&mut c)?;
            let (ptr, Ok(len)) = (ptr as u32 as usize, usize::try_from(len)) else {
                return Ok(-1);
            };
            let (data, st) = mem.data_and_store_mut(&mut c);
            if st.output.len().saturating_add(len) > st.output_cap {
                return Err(wasmtime::Error::new(HostStop(
                    WorkerStatus::OutputLimit,
                    "output limit",
                )));
            }
            let Some(src) = ptr.checked_add(len).and_then(|end| data.get(ptr..end)) else {
                return Ok(-1);
            };
            st.output.extend_from_slice(src);
            Ok(0)
        },
    )?;
    linker.func_wrap(
        HOST_MODULE,
        "checkpoint",
        |mut c: Caller<'_, State>, ptr: i32, len: i32| -> wasmtime::Result<i32> {
            let mem = memory_of(&mut c)?;
            let (ptr, Ok(len)) = (ptr as u32 as usize, usize::try_from(len)) else {
                return Ok(-1);
            };
            let (data, st) = mem.data_and_store_mut(&mut c);
            if len > st.checkpoint_cap {
                return Err(wasmtime::Error::new(HostStop(
                    WorkerStatus::OutputLimit,
                    "checkpoint limit",
                )));
            }
            let Some(src) = ptr.checked_add(len).and_then(|end| data.get(ptr..end)) else {
                return Ok(-1);
            };
            st.checkpoint.clear();
            st.checkpoint.extend_from_slice(src);
            Ok(0)
        },
    )?;
    Ok(())
}

fn classify(err: &wasmtime::Error, limiter_hit: bool) -> (WorkerStatus, String) {
    if let Some(stop) = err.downcast_ref::<HostStop>() {
        return (stop.0, stop.1.to_string());
    }
    if let Some(trap) = err.downcast_ref::<Trap>() {
        let st = match trap {
            Trap::OutOfFuel => WorkerStatus::OutOfFuel,
            Trap::Interrupt => WorkerStatus::EpochDeadline,
            Trap::StackOverflow => WorkerStatus::StackOverflow,
            _ if limiter_hit => WorkerStatus::MemoryLimit,
            _ => WorkerStatus::Trap,
        };
        return (st, format!("trap: {trap}"));
    }
    if limiter_hit {
        return (WorkerStatus::MemoryLimit, "memory limit".into());
    }
    (WorkerStatus::Trap, "error during run".into())
}

/// Execute a request in this process. Only the worker binary (and in-process unit tests) call this.
pub fn execute(req: WorkerRequest) -> WorkerResponse {
    if let Err(e) = req.limits.validate() {
        return refuse(format!("bad limits: {e}"));
    }
    let digest: [u8; 32] = Sha256::digest(&req.module).into();
    if digest != req.module_sha256 {
        return refuse("module hash mismatch");
    }
    let engine = match engine(req.limits.stack_bytes) {
        Ok(e) => e,
        Err(_) => return refuse("engine setup failed"),
    };
    let t_compile = Instant::now();
    let module = match Module::new(&engine, &req.module) {
        Ok(m) => m,
        Err(_) => return refuse("invalid module"),
    };
    let compile_us = elapsed_us(t_compile);
    if let Err(e) = check_imports(&module) {
        return refuse(e);
    }

    let lim = req.limits;
    let mut store = Store::new(
        &engine,
        State {
            limiter: Limiter {
                memory_bytes: usize::try_from(lim.memory_bytes).unwrap_or(usize::MAX),
                hit: false,
            },
            input: req.input,
            output: Vec::new(),
            checkpoint: Vec::new(),
            output_cap: usize::try_from(lim.output_bytes).unwrap_or(usize::MAX),
            checkpoint_cap: usize::try_from(lim.checkpoint_bytes).unwrap_or(usize::MAX),
        },
    );
    store.limiter(|s| &mut s.limiter);
    if store.set_fuel(lim.fuel).is_err() {
        return refuse("fuel setup failed");
    }
    let ticks = u64::try_from(lim.wall.as_millis() / EPOCH_TICK.as_millis())
        .unwrap_or(u64::MAX)
        .max(1);
    store.set_epoch_deadline(ticks);
    store.epoch_deadline_trap();

    let mut linker = Linker::new(&engine);
    if link(&mut linker).is_err() {
        return refuse("host API setup failed");
    }

    let done = Arc::new(AtomicBool::new(false));
    let ticker = {
        let engine = engine.clone();
        let done = done.clone();
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                std::thread::sleep(EPOCH_TICK);
                engine.increment_epoch();
            }
        })
    };

    let t_run = Instant::now();
    let result = linker
        .instantiate(&mut store, &module)
        .and_then(|inst| inst.get_typed_func::<(), i32>(&mut store, "run"))
        .and_then(|run| {
            if module.get_export("memory").is_none() {
                return Err(wasmtime::Error::new(HostStop(
                    WorkerStatus::Refused,
                    "module must export memory",
                )));
            }
            run.call(&mut store, ())
        });
    let run_us = elapsed_us(t_run);
    done.store(true, Ordering::Relaxed);
    let _ = ticker.join();

    let fuel_used = lim.fuel.saturating_sub(store.get_fuel().unwrap_or(0));
    let hit = store.data().limiter.hit;
    let st = store.into_data();
    let (status, exit_code, message) = match result {
        Ok(code) => (WorkerStatus::Exited, code, String::new()),
        Err(e) => {
            let (s, m) = classify(&e, hit);
            (s, -1, m)
        }
    };
    WorkerResponse {
        status,
        exit_code,
        fuel_used,
        compile_us,
        run_us,
        output: st.output,
        checkpoint: st.checkpoint,
        message,
    }
}

fn elapsed_us(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// Run [`execute`] on a thread whose native stack comfortably exceeds the Wasm stack limit, so a deep
/// recursion hits Wasmtime's `StackOverflow` trap instead of the OS guard page.
pub fn execute_on_big_stack(req: WorkerRequest) -> WorkerResponse {
    let native = usize::try_from(req.limits.stack_bytes)
        .unwrap_or(usize::MAX)
        .saturating_add(8 * 1024 * 1024);
    match std::thread::Builder::new()
        .name("nc-exec-run".into())
        .stack_size(native)
        .spawn(move || execute(req))
    {
        Ok(h) => h
            .join()
            .unwrap_or_else(|_| refuse("worker thread panicked")),
        Err(_) => refuse("could not start run thread"),
    }
}
