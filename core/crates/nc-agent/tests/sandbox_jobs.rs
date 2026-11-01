#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Malicious jobs through the whole helper session with the **real** nc-exec sandbox (AppContainer + Job
//! Object): infinite loop + Stop, memory growth, huge output, and a good echo job.
//!
//! Needs the worker binary (dependency binaries are not built by `cargo test`):
//! ```text
//! cargo build -p nc-exec --bin nc-exec-worker
//! set NC_EXEC_WORKER=<target>\debug\nc-exec-worker.exe
//! cargo test -p nc-agent --test sandbox_jobs -- --ignored --nocapture --test-threads 1
//! ```

mod common;

use common::*;
use nc_agent::budget::{BudgetGate, NetworkBudget};
use nc_agent::runner::SandboxRunner;
use nc_agent::transfer::BlobCache;
use nc_agent::{Activity, Agent, Backend, Tuning};
use nc_protocol::pb::v1::LeaseFailReason;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const IMPORTS: &str = r#"
 (import "nc" "input_read" (func $read (param i64 i32 i32) (result i32)))
 (import "nc" "output_write" (func $write (param i32 i32) (result i32)))"#;

fn wasm(body: &str) -> Vec<u8> {
    wat::parse_str(format!("(module {IMPORTS} {body})")).unwrap()
}

fn worker() -> PathBuf {
    static W: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    W.get_or_init(|| {
        let src = PathBuf::from(std::env::var("NC_EXEC_WORKER").expect("NC_EXEC_WORKER"));
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("nc-agent-ac-runtime");
        std::fs::create_dir_all(&dir).unwrap();
        let dst = dir.join("nc-exec-worker.exe");
        std::fs::copy(&src, &dst).unwrap();
        nc_exec::sandbox::grant_appcontainer_read(&dir).unwrap();
        dst
    })
    .clone()
}

fn agent(kit: &Kit) -> Agent {
    let runner = SandboxRunner::new(worker());
    runner.prepare().expect("sandbox ready");
    let staging = kit.dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    Agent::with_backend(Backend {
        coord: kit.coord.clone(),
        probe: kit.probe.clone(),
        clock: Arc::new(FixedClock(noon())),
        runner: Arc::new(runner),
        journal: Mutex::new(kit.journal()),
        cache: Mutex::new(BlobCache::open(&kit.dir.path().join("blobs"), 64 << 20).unwrap()),
        budget: BudgetGate::new(&NetworkBudget::Unmetered, noon()).unwrap(),
        staging,
        device_name: "sandbox-test".into(),
        platform: "windows".into(),
        tuning: Tuning {
            poll: Duration::from_millis(20),
            max_backoff: Duration::from_millis(100),
            monitor_tick: Duration::from_millis(10),
        },
    })
}

fn run_to_fail(module: Vec<u8>) -> (Option<String>, i32) {
    let kit = Kit::new(Mode::Reverse);
    kit.queue_job(&module, b"input");
    let a = agent(&kit);
    a.start_helper_session(kit.policy()).unwrap();
    until_long(|| !kit.coord.fails().is_empty() || kit.coord.count("complete") > 0);
    a.stop_helper_session();
    let f = kit.coord.fails();
    (f[0].code.clone(), f[0].reason)
}

fn until_long(mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(60), "timeout");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "needs NC_EXEC_WORKER (built nc-exec-worker.exe)"]
fn echo_job_completes_in_the_real_sandbox() {
    let kit = Kit::new(Mode::Reverse);
    let echo = wasm(
        r#"(memory (export "memory") 1)
        (func (export "run") (result i32) (local $n i32)
          (local.set $n (call $read (i64.const 0) (i32.const 0) (i32.const 65536)))
          (drop (call $write (i32.const 0) (local.get $n)))
          (i32.const 0))"#,
    );
    kit.queue_job(&echo, b"hello sandbox");
    let a = agent(&kit);
    a.start_helper_session(kit.policy()).unwrap();
    until_long(|| a.today().jobs_done == 1);
    a.stop_helper_session();
    let s = kit.coord.s.lock().unwrap();
    assert_eq!(s.puts.values().next().unwrap(), b"hello sandbox");
}

#[test]
#[ignore = "needs NC_EXEC_WORKER (built nc-exec-worker.exe)"]
fn infinite_loop_is_killed_by_stop_within_one_second() {
    let kit = Kit::new(Mode::Reverse);
    let spin = wasm(
        r#"(memory (export "memory") 1) (func (export "run") (result i32) (loop $l (br $l)) (i32.const 0))"#,
    );
    kit.queue_job(&spin, b"input");
    let a = agent(&kit);
    a.start_helper_session(kit.policy()).unwrap();
    until_long(|| a.activity() == Activity::Running);
    std::thread::sleep(Duration::from_millis(300));
    let t = Instant::now();
    a.stop_helper_session();
    let stopped = t.elapsed();
    until_long(|| !kit.coord.fails().is_empty());
    let reported = t.elapsed();
    println!("real sandbox: stop returned {stopped:?}, STOPPED_BY_OWNER sent after {reported:?}");
    assert!(stopped < Duration::from_secs(1));
    let f = &kit.coord.fails()[0];
    assert_eq!(f.reason, LeaseFailReason::StoppedByOwner as i32);
}

#[test]
#[ignore = "needs NC_EXEC_WORKER (built nc-exec-worker.exe)"]
fn memory_growth_job_is_reported_crashed() {
    let grow = wasm(
        r#"(memory (export "memory") 1)
        (func (export "run") (result i32)
          (loop $l (drop (memory.grow (i32.const 16))) (br $l)) (i32.const 0))"#,
    );
    let (code, reason) = run_to_fail(grow);
    println!("memory growth: reason {reason} code {code:?}");
    assert!(
        reason == LeaseFailReason::Crashed as i32 || reason == LeaseFailReason::Timeout as i32,
        "{reason} {code:?}"
    );
}

#[test]
#[ignore = "needs NC_EXEC_WORKER (built nc-exec-worker.exe)"]
fn huge_output_job_is_cut_and_reported() {
    let flood = wasm(
        r#"(memory (export "memory") 1)
        (func (export "run") (result i32)
          (loop $l (drop (call $write (i32.const 0) (i32.const 65536))) (br $l)) (i32.const 0))"#,
    );
    let (code, reason) = run_to_fail(flood);
    println!("huge output: reason {reason} code {code:?}");
    assert_eq!(reason, LeaseFailReason::Crashed as i32);
    assert_eq!(code.as_deref(), Some("OUTPUT_LIMIT"));
}
