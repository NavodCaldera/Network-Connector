#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Malicious-job tests against the Wasmtime layer alone (in-process, every platform).
//! The same modules are run through the full sandbox in `sandbox_windows.rs`.

mod common;

use std::time::{Duration, Instant};

use common::*;
use nc_exec::ipc::{WorkerRequest, WorkerStatus};
use nc_exec::runtime::execute_on_big_stack;
use nc_exec::{sha256, Limits};

fn req(module: Vec<u8>, input: &[u8], limits: Limits) -> WorkerRequest {
    WorkerRequest {
        limits,
        module_sha256: sha256(&module),
        module,
        input: input.to_vec(),
    }
}

#[test]
fn echo_runs_and_returns_output_and_checkpoint() {
    let r = execute_on_big_stack(req(echo(), b"hello", Limits::default()));
    assert_eq!(r.status, WorkerStatus::Exited, "{}", r.message);
    assert_eq!(r.exit_code, 0);
    assert_eq!(r.output, b"hello");
    assert_eq!(r.checkpoint, b"hello");
    assert!(r.fuel_used > 0);
}

#[test]
fn infinite_loop_stops_on_fuel() {
    let lim = Limits {
        fuel: 50_000_000,
        ..Limits::default()
    };
    let r = execute_on_big_stack(req(infinite_loop(), b"", lim));
    assert_eq!(r.status, WorkerStatus::OutOfFuel);
}

#[test]
fn infinite_loop_stops_on_epoch_when_fuel_is_huge() {
    let lim = Limits {
        fuel: u64::MAX / 2,
        wall: Duration::from_millis(300),
        ..Limits::default()
    };
    let t = Instant::now();
    let r = execute_on_big_stack(req(infinite_loop(), b"", lim));
    let took = t.elapsed();
    assert_eq!(r.status, WorkerStatus::EpochDeadline);
    assert!(took < Duration::from_secs(2), "took {took:?}");
    eprintln!("epoch stop for 300 ms budget: {took:?}");
}

#[test]
fn memory_growth_is_capped() {
    let lim = Limits {
        memory_bytes: 16 * 1024 * 1024,
        ..Limits::default()
    };
    let r = execute_on_big_stack(req(memory_growth(), b"", lim));
    assert_eq!(r.status, WorkerStatus::MemoryLimit, "{}", r.message);
}

#[test]
fn huge_initial_memory_is_refused_at_instantiation() {
    let lim = Limits {
        memory_bytes: 16 * 1024 * 1024,
        ..Limits::default()
    };
    let r = execute_on_big_stack(req(huge_initial_memory(), b"", lim));
    assert_eq!(r.status, WorkerStatus::MemoryLimit, "{}", r.message);
}

#[test]
fn huge_output_is_capped() {
    let lim = Limits {
        output_bytes: 1024 * 1024,
        ..Limits::default()
    };
    let r = execute_on_big_stack(req(huge_output(), b"", lim));
    assert_eq!(r.status, WorkerStatus::OutputLimit);
    assert!(r.output.len() as u64 <= lim.output_bytes);
}

#[test]
fn deep_recursion_hits_stack_limit() {
    let r = execute_on_big_stack(req(deep_recursion(), b"", Limits::default()));
    assert_eq!(r.status, WorkerStatus::StackOverflow, "{}", r.message);
}

#[test]
fn wrong_hash_is_refused_in_worker_too() {
    let mut rq = req(echo(), b"", Limits::default());
    rq.module_sha256[0] ^= 1;
    let r = execute_on_big_stack(rq);
    assert_eq!(r.status, WorkerStatus::Refused);
    assert_eq!(r.message, "module hash mismatch");
}

#[test]
fn wasi_import_is_refused() {
    let r = execute_on_big_stack(req(wasi_import(), b"", Limits::default()));
    assert_eq!(r.status, WorkerStatus::Refused);
    assert!(r.message.contains("wasi_snapshot_preview1.fd_write"));
}

#[test]
fn wrong_import_signature_is_refused() {
    let r = execute_on_big_stack(req(wrong_import_type(), b"", Limits::default()));
    assert_eq!(r.status, WorkerStatus::Refused);
}

#[test]
fn garbage_module_is_refused() {
    let r = execute_on_big_stack(req(vec![0, 97, 115, 109, 1], b"", Limits::default()));
    assert_eq!(r.status, WorkerStatus::Refused);
}

#[test]
fn input_read_out_of_bounds_returns_error_not_crash() {
    let m = wasm(
        r#"(memory (export "memory") 1)
        (func (export "run") (result i32)
          (call $read (i64.const 0) (i32.const 65530) (i32.const 100)))"#,
    );
    let r = execute_on_big_stack(req(m, &[1u8; 50], Limits::default()));
    assert_eq!(r.status, WorkerStatus::Exited);
    assert_eq!(r.exit_code, -1);
}
