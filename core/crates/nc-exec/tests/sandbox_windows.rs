#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Full-stack tests on this Windows PC: real AppContainer + Job Object + worker process.
//! Run with `--nocapture` to see the measured timings used in spikes/phase0/REPORT.md.

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use common::*;
use nc_exec::sandbox::{self, SandboxEvent, SandboxSpec};
use nc_exec::{CancelToken, ExecError, Executor, ExecutorConfig, Limits, Termination};

/// Sandbox tests run one at a time so timings are clean and the per-process profile count is exact.
fn serial() -> MutexGuard<'static, ()> {
    static M: Mutex<()> = Mutex::new(());
    M.lock().unwrap_or_else(|e| e.into_inner())
}

/// Copy the worker and the probe into a folder readable by AppContainers (only that folder gets the ACE).
fn runtime_dir() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("nc-exec-ac-runtime");
        std::fs::create_dir_all(&dir).unwrap();
        let worker = PathBuf::from(env!("CARGO_BIN_EXE_nc-exec-worker"));
        std::fs::copy(&worker, dir.join("nc-exec-worker.exe")).unwrap();
        let probe = worker
            .parent()
            .unwrap()
            .join("examples")
            .join("sandbox_probe.exe");
        std::fs::copy(&probe, dir.join("sandbox_probe.exe")).expect(
            "build the runnable probe first: cargo build -p nc-exec --example sandbox_probe --locked",
        );
        sandbox::grant_appcontainer_read(&dir).unwrap();
        dir
    })
}

fn executor() -> Executor {
    Executor::new(
        ExecutorConfig::new(runtime_dir().join("nc-exec-worker.exe")),
        Box::new(TestVerifier),
    )
}

fn own_profiles() -> Vec<String> {
    let mine = format!("{}{}-", sandbox::PROFILE_PREFIX, std::process::id());
    sandbox::list_profiles()
        .unwrap()
        .into_iter()
        .filter(|n| n.to_ascii_lowercase().starts_with(&mine))
        .collect()
}

#[test]
fn echo_job_runs_in_sandbox_and_is_torn_down() {
    let _g = serial();
    let ex = executor();
    let mut totals = Vec::new();
    for i in 0..6 {
        let out = ex
            .run(&job(echo(), b"abc", Limits::default()), &CancelToken::new())
            .unwrap();
        assert_eq!(out.termination, Termination::Exited(0));
        assert_eq!(out.output, b"abc");
        assert!(own_profiles().is_empty(), "profile left after run {i}");
        totals.push(out.timings);
    }
    for t in &totals {
        eprintln!(
            "TIMING echo: profile {:?} launch {:?} compile {:?} run {:?} teardown {:?} total {:?}",
            t.sandbox_profile, t.sandbox_launch, t.compile, t.run, t.teardown, t.total
        );
    }
}

#[test]
fn infinite_loop_fuel_in_sandbox() {
    let _g = serial();
    let lim = Limits {
        fuel: 100_000_000,
        ..Limits::default()
    };
    let out = executor()
        .run(&job(infinite_loop(), b"", lim), &CancelToken::new())
        .unwrap();
    assert_eq!(out.termination, Termination::OutOfFuel);
}

#[test]
fn infinite_loop_epoch_in_sandbox() {
    let _g = serial();
    let lim = Limits {
        fuel: u64::MAX / 2,
        wall: Duration::from_millis(500),
        ..Limits::default()
    };
    let t = Instant::now();
    let out = executor()
        .run(&job(infinite_loop(), b"", lim), &CancelToken::new())
        .unwrap();
    assert_eq!(out.termination, Termination::EpochDeadline);
    eprintln!("TIMING epoch 500 ms budget: call took {:?}", t.elapsed());
}

#[test]
fn memory_growth_in_sandbox() {
    let _g = serial();
    let lim = Limits {
        memory_bytes: 64 * 1024 * 1024,
        ..Limits::default()
    };
    let out = executor()
        .run(&job(memory_growth(), b"", lim), &CancelToken::new())
        .unwrap();
    assert_eq!(out.termination, Termination::MemoryLimit);
    eprintln!("peak worker memory: {} MB", out.peak_memory_bytes >> 20);
}

#[test]
fn huge_output_in_sandbox() {
    let _g = serial();
    let lim = Limits {
        output_bytes: 1024 * 1024,
        ..Limits::default()
    };
    let out = executor()
        .run(&job(huge_output(), b"", lim), &CancelToken::new())
        .unwrap();
    assert_eq!(out.termination, Termination::OutputLimit);
    assert!(out.output.is_empty());
}

#[test]
fn deep_recursion_in_sandbox() {
    let _g = serial();
    let out = executor()
        .run(
            &job(deep_recursion(), b"", Limits::default()),
            &CancelToken::new(),
        )
        .unwrap();
    assert_eq!(out.termination, Termination::StackOverflow);
}

#[test]
fn forbidden_import_refused_by_worker() {
    let _g = serial();
    let err = executor()
        .run(
            &job(wasi_import(), b"", Limits::default()),
            &CancelToken::new(),
        )
        .unwrap_err();
    assert!(matches!(err, ExecError::Refused(_)), "{err:?}");
    assert!(own_profiles().is_empty());
}

#[test]
fn cancel_kills_within_one_second() {
    let _g = serial();
    let ex = executor();
    let lim = Limits {
        fuel: u64::MAX / 2,
        wall: Duration::from_secs(60),
        ..Limits::default()
    };
    // Warm-up: the first launch of a freshly copied binary is slow on this PC (measured separately).
    let _ = ex
        .run(&job(echo(), b"", Limits::default()), &CancelToken::new())
        .unwrap();
    let mut lat = Vec::new();
    for delay_ms in [0u64, 20, 100, 400, 1000] {
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        let j = job(infinite_loop(), b"", lim);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::scope(|s| {
            s.spawn(|| {
                let r = ex.run(&j, &c2);
                tx.send((r, Instant::now())).unwrap();
            });
            std::thread::sleep(Duration::from_millis(delay_ms));
            let t = Instant::now();
            cancel.cancel();
            let (r, done) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let out = r.unwrap();
            assert_eq!(out.termination, Termination::Cancelled);
            let l = done.duration_since(t);
            assert!(
                l < Duration::from_secs(1),
                "cancel took {l:?}; timings {:?}",
                out.timings
            );
            lat.push((delay_ms, l, out.timings.kill_latency));
        });
        assert!(own_profiles().is_empty());
    }
    for (d, l, k) in lat {
        eprintln!("TIMING cancel after {d} ms: cancel->run() returned {l:?} (kill->exit {k:?})");
    }
}

/// Probe in place of the worker: everything an escaped job would try.
fn probe(cmds: &str, mem: u64) -> (String, Vec<SandboxEvent>, Option<u32>) {
    let spec = SandboxSpec {
        worker: runtime_dir().join("sandbox_probe.exe"),
        process_memory_bytes: mem,
    };
    let (mut child, mut pipes) = sandbox::spawn(&spec).unwrap();
    child.resume().unwrap();
    let folder = child.container_folder().unwrap().to_string();
    pipes.stdin.write_all(cmds.as_bytes()).unwrap();
    drop(pipes.stdin);
    let mut out = String::new();
    let reader = std::thread::spawn(move || {
        let _ = pipes.stdout.read_to_string(&mut out);
        out
    });
    let mut events = Vec::new();
    let mut code = None;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        events.extend(child.events());
        if events.contains(&SandboxEvent::MemoryLimit) {
            child.kill();
        }
        if let Some(c) = child.wait(Duration::from_millis(20)) {
            code = Some(c);
            break;
        }
    }
    events.extend(child.events());
    child.teardown();
    assert!(
        !std::path::Path::new(&folder).exists(),
        "container folder left behind"
    );
    (reader.join().unwrap(), events, code)
}

#[test]
fn appcontainer_denies_files_network_and_children() {
    let _g = serial();
    // Secret file in a folder without the AppContainer ACE; the test process itself can read it.
    let secret_dir = tempfile::tempdir().unwrap();
    let secret = secret_dir.path().join("secret.txt");
    std::fs::write(&secret, b"not for jobs").unwrap();
    assert!(std::fs::read(&secret).is_ok());
    let profile = std::env::var("USERPROFILE").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let control_internet = std::net::TcpStream::connect_timeout(
        &"1.1.1.1:443".parse().unwrap(),
        Duration::from_secs(3),
    )
    .is_ok();

    let cmds = format!(
        "read {}\nwrite {}\nread {profile}\\NTUSER.DAT\nwrite {profile}\\nc-escape.txt\nwrite E:\\nc-escape.txt\n\
         tcp 127.0.0.1:{port}\ntcp 1.1.1.1:443\ndns example.com\nspawn\nenv\nread C:\\Windows\\win.ini\n",
        secret.display(),
        secret_dir.path().join("planted.txt").display(),
    );
    let (out, events, code) = probe(&cmds, 256 << 20);
    eprintln!("PROBE (control internet reachable: {control_internet}):\n{out}events {events:?} exit {code:?}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 11, "probe output: {out}");
    assert!(lines[0].starts_with("READ_DENIED"), "{}", lines[0]);
    assert!(lines[1].starts_with("WRITE_DENIED"), "{}", lines[1]);
    assert!(lines[2].starts_with("READ_DENIED"), "{}", lines[2]);
    assert!(lines[3].starts_with("WRITE_DENIED"), "{}", lines[3]);
    assert!(lines[4].starts_with("WRITE_DENIED"), "{}", lines[4]);
    assert!(lines[5].starts_with("TCP_DENIED"), "{}", lines[5]);
    assert!(lines[6].starts_with("TCP_DENIED"), "{}", lines[6]);
    assert!(lines[7].starts_with("DNS_DENIED"), "{}", lines[7]);
    assert!(lines[8].starts_with("SPAWN_DENIED"), "{}", lines[8]);
    assert!(
        listener.accept().is_err(),
        "loopback listener got a connection"
    );
    assert!(!secret_dir.path().join("planted.txt").exists());
}

#[test]
fn job_object_memory_cap_kills_native_allocation() {
    let _g = serial();
    let (out, events, code) = probe("alloc 1024\n", 64 << 20);
    eprintln!("alloc probe: out {out:?} events {events:?} exit {code:?}");
    assert!(!out.contains("ALLOC_OK"));
    assert!(events.contains(&SandboxEvent::MemoryLimit));
}

#[test]
fn watchdog_kills_hung_worker() {
    // A worker that never answers (stands in for a blocking host call or hung runtime).
    let _g = serial();
    let mut cfg = ExecutorConfig::new(runtime_dir().join("sandbox_probe.exe"));
    cfg.watchdog_grace = Duration::from_millis(200);
    let ex = Executor::new(cfg, Box::new(TestVerifier));
    let lim = Limits {
        wall: Duration::from_millis(300),
        ..Limits::default()
    };
    let t = Instant::now();
    let out = ex.run(&job(echo(), b"", lim), &CancelToken::new()).unwrap();
    let took = t.elapsed();
    eprintln!(
        "TIMING watchdog 300+200 ms: {took:?}, kill {:?}",
        out.timings.kill_latency
    );
    assert_eq!(out.termination, Termination::WatchdogKilled);
    assert!(took < Duration::from_secs(2));
    assert!(own_profiles().is_empty());
}

#[test]
fn sweep_removes_leftovers_of_dead_owner() {
    let _g = serial();
    // 4294967 is not a multiple of 4, so it is never a live Windows PID.
    let name = format!("{}4294967-crashleftover", sandbox::PROFILE_PREFIX);
    let folder = sandbox::create_named_profile_for_test(&name).unwrap();
    assert!(sandbox::list_profiles()
        .unwrap()
        .iter()
        .any(|n| n.eq_ignore_ascii_case(&name)));
    let removed = sandbox::sweep_leftovers().unwrap();
    assert!(removed >= 1);
    assert!(!sandbox::list_profiles()
        .unwrap()
        .iter()
        .any(|n| n.eq_ignore_ascii_case(&name)));
    assert!(!std::path::Path::new(&folder).exists());
}

/// Measurement only (run with `--ignored`): first launch of a freshly written worker binary vs. warm launches.
#[test]
#[ignore]
fn measure_cold_vs_warm_launch() {
    let _g = serial();
    let base = runtime_dir();
    for i in 0..3 {
        let name = format!(
            "nc-exec-worker-cold-{}-{i}.exe",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );
        let path = base.join(&name);
        std::fs::copy(base.join("nc-exec-worker.exe"), &path).unwrap();
        let ex = Executor::new(ExecutorConfig::new(&path), Box::new(TestVerifier));
        if i == 2 {
            eprintln!(
                "TIMING warm_up of fresh binary {i}: {:?}",
                ex.warm_up().unwrap()
            );
        }
        for run in 0..3 {
            let out = ex
                .run(&job(echo(), b"x", Limits::default()), &CancelToken::new())
                .unwrap();
            eprintln!(
                "TIMING fresh binary {i} run {run}: launch {:?} total {:?} ({:?})",
                out.timings.sandbox_launch, out.timings.total, out.termination
            );
        }
        let _ = std::fs::remove_file(&path);
    }
}
