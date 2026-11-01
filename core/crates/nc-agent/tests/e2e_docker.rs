#![allow(clippy::unwrap_used, clippy::expect_used)]
//! End to end against the real Docker coordinator (infra/docker-compose.yml): a requester submits a real Wasm
//! echo job, a helper agent leases it, runs it in the real nc-exec sandbox and completes it, and the requester
//! downloads the output.
//!
//! Needs a Windows host (the sandbox is AppContainer + Job Object; on other OSes nc-exec refuses jobs, by
//! design) with the stack running, and:
//! - `NC_TEST_URL` (default `http://127.0.0.1:8080`), `NC_ENROLL_SECRET`;
//! - `NC_EXEC_WORKER`: path to `nc-exec-worker.exe` in a folder AppContainers can read.
//!
//! ```text
//! cargo test -p nc-agent --test e2e_docker -- --ignored --nocapture
//! ```

mod common;

use common::{good_device, home, key, policy_trusting};
use nc_agent::budget::{BudgetGate, NetworkBudget};
use nc_agent::coord::{Coordinator, HttpCoordinator};
use nc_agent::journal::Journal;
use nc_agent::probe::{FakeProbe, SystemClock};
use nc_agent::requester::{self, JobSpec};
use nc_agent::runner::SandboxRunner;
use nc_agent::transfer::BlobCache;
use nc_agent::{Agent, Backend, Tuning};
use nc_protocol::pb::v1::RegisterRequest;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const ECHO: &str = r#"(module
 (import "nc" "input_read" (func $read (param i64 i32 i32) (result i32)))
 (import "nc" "output_write" (func $write (param i32 i32) (result i32)))
 (memory (export "memory") 1)
 (func (export "run") (result i32) (local $n i32)
   (local.set $n (call $read (i64.const 0) (i32.const 0) (i32.const 65536)))
   (drop (call $write (i32.const 0) (local.get $n)))
   (i32.const 0)))"#;

#[test]
#[ignore = "needs the Docker coordinator stack, NC_ENROLL_SECRET and NC_EXEC_WORKER on Windows"]
fn submit_lease_run_complete_download() {
    let base = std::env::var("NC_TEST_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let enroll = std::env::var("NC_ENROLL_SECRET").expect("NC_ENROLL_SECRET");
    let worker = std::env::var("NC_EXEC_WORKER").expect("NC_EXEC_WORKER");
    let dir = tempfile::tempdir().unwrap();

    // Requester (also the module publisher).
    let requester_key = key(201);
    let publisher = nc_protocol::Signer::public_key(&requester_key);
    let req = HttpCoordinator::new(&base, Arc::new(key(201)), Some(enroll.clone()));
    req.register(&RegisterRequest {
        name: "e2e-requester".into(),
        platform: std::env::consts::OS.into(),
    })
    .unwrap();

    // Helper agent with the real sandbox.
    let runner = SandboxRunner::new(&worker);
    runner.prepare().expect("sandbox ready");
    let staging = dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let agent = Agent::with_backend(Backend {
        coord: Arc::new(HttpCoordinator::new(
            &base,
            Arc::new(key(202)),
            Some(enroll),
        )),
        probe: Arc::new(FakeProbe::new(good_device(), home())),
        clock: Arc::new(SystemClock),
        runner: Arc::new(runner),
        journal: Mutex::new(Journal::open(&dir.path().join("journal")).unwrap()),
        cache: Mutex::new(BlobCache::open(&dir.path().join("blobs"), 64 << 20).unwrap()),
        budget: BudgetGate::new(&NetworkBudget::Unmetered, chrono::Utc::now()).unwrap(),
        staging,
        device_name: "e2e-helper".into(),
        platform: std::env::consts::OS.into(),
        tuning: Tuning {
            poll: Duration::from_secs(1),
            ..Tuning::default()
        },
    });
    agent
        .start_helper_session(policy_trusting(publisher))
        .unwrap();

    let module = wat::parse_str(ECHO).unwrap();
    let mut spec = JobSpec::new(module, b"hello from the requester".to_vec());
    spec.limits.wall_ms = 10_000;
    let job =
        requester::build(&spec, &requester_key, chrono::Utc::now().timestamp_millis()).unwrap();
    let id = requester::submit(&req, &job).unwrap();
    println!("submitted {id}");
    let status =
        requester::wait(&req, &id, Duration::from_secs(120), Duration::from_secs(1)).unwrap();
    let out = requester::fetch_output(&req, status.output.as_ref().unwrap()).unwrap();
    agent.stop_helper_session();
    assert_eq!(out, b"hello from the requester");
    assert_eq!(agent.today().jobs_done, 1);
    println!("e2e: submit -> lease -> sandbox run -> complete -> output matches: passed");
}
