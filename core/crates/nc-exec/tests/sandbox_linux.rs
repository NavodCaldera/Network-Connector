#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Run under a delegated cgroup scope; see linux-sandbox.sh. No containment test silently skips.
mod common;
use common::*;
use nc_exec::sandbox::{self, SandboxEvent, SandboxSpec};
use nc_exec::{CancelToken, Executor, ExecutorConfig, Limits, Termination};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn worker() -> PathBuf {
    env!("CARGO_BIN_EXE_nc-exec-worker").into()
}
fn probe_path() -> PathBuf {
    worker().parent().unwrap().join("examples/sandbox_probe")
}
fn spec(probe: bool, memory: u64) -> SandboxSpec {
    SandboxSpec {
        worker: if probe { probe_path() } else { worker() },
        process_memory_bytes: memory,
    }
}
fn probe(command: &str, memory: u64) -> (String, Vec<SandboxEvent>, u32) {
    let (mut child, mut pipes) = sandbox::spawn(&spec(true, memory)).unwrap();
    child.resume().unwrap();
    pipes.stdin.write_all(command.as_bytes()).unwrap();
    drop(pipes.stdin);
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        pipes.stdout.read_to_string(&mut s).unwrap();
        s
    });
    let until = Instant::now() + Duration::from_secs(10);
    let mut events = Vec::new();
    let code = loop {
        events.extend(child.events());
        if let Some(code) = child.wait(Duration::from_millis(5)) {
            break code;
        }
        assert!(Instant::now() < until, "probe timeout");
    };
    events.extend(child.events());
    let peak = child.peak_memory_bytes();
    child.teardown();
    let output = reader.join().unwrap();
    eprintln!(
        "probe {}: exit={code} events={events:?} peak={peak}",
        command.split_whitespace().next().unwrap()
    );
    (output, events, code)
}
#[test]
#[ignore = "requires delegated cgroup v2 and unprivileged namespaces"]
fn malicious_jobs() {
    let ex = Executor::new(ExecutorConfig::new(worker()), Box::new(TestVerifier));
    for (module, expected) in [
        (echo(), Termination::Exited(0)),
        (infinite_loop(), Termination::OutOfFuel),
        (memory_growth(), Termination::MemoryLimit),
        (huge_output(), Termination::OutputLimit),
    ] {
        let limits = Limits {
            fuel: 50_000_000,
            memory_bytes: 16 << 20,
            output_bytes: 1 << 20,
            ..Limits::default()
        };
        let out = ex
            .run(&job(module, b"", limits), &CancelToken::new())
            .unwrap();
        eprintln!(
            "malicious: {:?}, timings={:?}",
            out.termination, out.timings
        );
        assert_eq!(out.termination, expected);
    }
}
#[test]
#[ignore = "requires delegated cgroup v2 and unprivileged namespaces"]
fn filesystem_and_environment() {
    let home = std::env::var("HOME").unwrap();
    let (out, events, code) = probe(
        &format!("read /etc/passwd\ndir {home}\nwrite /etc/planted\nwrite {home}/planted\nenv\n"),
        128 << 20,
    );
    assert_eq!(code, 0);
    assert!(events.is_empty());
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(lines.len(), 5);
    assert!(lines[0].starts_with("READ_DENIED"));
    assert!(lines[1].starts_with("DIR_DENIED"));
    assert!(lines[2..4].iter().all(|s| s.starts_with("WRITE_DENIED")));
    assert_eq!(lines[4], "ENV_COUNT 0");
}
#[test]
#[ignore = "requires delegated cgroup v2 and unprivileged namespaces"]
fn forbidden_syscalls_kill_process() {
    for command in [
        "tcp 1.1.1.1:443\n",
        "exec\n",
        "ptrace\n",
        "unshare\n",
        "mount\n",
        "forkbomb\n",
    ] {
        let (out, events, code) = probe(command, 128 << 20);
        assert!(out.is_empty());
        assert_eq!(code, 128 + libc::SIGSYS as u32);
        assert!(events.contains(&SandboxEvent::ProcessLimit));
    }
}
#[test]
#[ignore = "requires delegated cgroup v2 and unprivileged namespaces"]
fn native_memory_cap() {
    let (out, events, _) = probe("alloc 1024\n", 64 << 20);
    assert!(!out.contains("ALLOC_OK"));
    assert!(events.contains(&SandboxEvent::MemoryLimit));
}
#[test]
#[ignore = "requires delegated cgroup v2 and unprivileged namespaces"]
fn pids_cap() {
    let (out, events, _) = probe("threads\n", 128 << 20);
    assert!(out.contains("THREADS_DENIED"));
    assert!(events.contains(&SandboxEvent::ProcessLimit));
}
#[test]
#[ignore = "requires delegated cgroup v2 and unprivileged namespaces"]
fn suspended_start_kill_and_drop() {
    let (mut child, mut pipes) = sandbox::spawn(&spec(true, 128 << 20)).unwrap();
    let groups = own_groups();
    assert_eq!(groups.len(), 1);
    let processes = std::fs::read_to_string(groups[0].join("cgroup.procs")).unwrap();
    // The trusted launcher is the executor's child; PID-namespace init is its grandchild.
    assert!(processes.lines().count() >= 2);
    pipes.stdin.write_all(b"env\n").unwrap();
    // A suspended worker cannot produce output, even with a complete command waiting in stdin.
    // SAFETY: pollfd references our live pipe; poll only observes readiness.
    let mut fd = libc::pollfd {
        fd: std::os::fd::AsRawFd::as_raw_fd(&pipes.stdout),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: fd is our live pipe and poll receives exactly one initialized pollfd.
    assert_eq!(unsafe { libc::poll(&mut fd, 1, 100) }, 0);
    child.resume().unwrap();
    let t = Instant::now();
    child.kill();
    assert!(child.wait(Duration::from_millis(800)).is_some());
    assert!(t.elapsed() < Duration::from_secs(1));
    eprintln!("kill latency={:?}", t.elapsed());
    child.teardown();
    let (child, _) = sandbox::spawn(&spec(true, 128 << 20)).unwrap();
    drop(child);
}

fn delegated_base() -> PathBuf {
    let membership = std::fs::read_to_string("/proc/self/cgroup").unwrap();
    let relative = membership
        .lines()
        .find_map(|s| s.strip_prefix("0::/"))
        .unwrap();
    let mut base = PathBuf::from("/sys/fs/cgroup").join(relative);
    loop {
        let enabled =
            std::fs::read_to_string(base.join("cgroup.subtree_control")).unwrap_or_default();
        if ["cpu", "memory", "pids"]
            .iter()
            .all(|c| enabled.split_whitespace().any(|s| s == *c))
        {
            return base;
        }
        assert!(base.pop());
    }
}
fn own_groups() -> Vec<PathBuf> {
    let prefix = format!("{}{}-", sandbox::PROFILE_PREFIX, std::process::id());
    std::fs::read_dir(delegated_base())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&prefix)
        })
        .collect()
}
#[test]
#[ignore = "requires delegated cgroup v2 and unprivileged namespaces"]
fn teardown_drop_and_sweep_remove_everything() {
    assert!(own_groups().is_empty());
    let (mut child, _) = sandbox::spawn(&spec(true, 128 << 20)).unwrap();
    let groups = own_groups();
    assert_eq!(groups.len(), 1);
    let path = &groups[0];
    let pids: Vec<u32> = std::fs::read_to_string(path.join("cgroup.procs"))
        .unwrap()
        .lines()
        .map(|p| p.parse().unwrap())
        .collect();
    assert_eq!(
        std::fs::read_to_string(path.join("memory.max"))
            .unwrap()
            .trim(),
        (128u64 << 20).to_string()
    );
    assert_eq!(
        std::fs::read_to_string(path.join("memory.swap.max"))
            .unwrap()
            .trim(),
        "0"
    );
    assert_eq!(
        std::fs::read_to_string(path.join("pids.max"))
            .unwrap()
            .trim(),
        "32"
    );
    assert_eq!(
        std::fs::read_to_string(path.join("cpu.max"))
            .unwrap()
            .trim(),
        "100000 100000"
    );
    // A live owner must never be swept.
    sandbox::sweep_leftovers().unwrap();
    assert!(path.exists());
    child.resume().unwrap();
    child.teardown();
    assert!(!path.exists());
    for pid in pids {
        assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
    }
    let (child, _) = sandbox::spawn(&spec(true, 128 << 20)).unwrap();
    drop(child);
    assert!(own_groups().is_empty());
    let stale = delegated_base().join(format!(
        "{}4294967295-0-00000000000000000000000000000000",
        sandbox::PROFILE_PREFIX
    ));
    std::fs::create_dir(&stale).unwrap();
    let staging = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .prefix(&format!(
            "{}-",
            stale.file_name().unwrap().to_str().unwrap()
        ))
        .tempdir()
        .unwrap();
    assert!(sandbox::sweep_leftovers().unwrap() >= 1);
    assert!(!stale.exists());
    assert!(!staging.path().exists());
    eprintln!("teardown: processes gone; drop: cgroup gone; sweep: stale removed, live retained");
}

#[test]
#[ignore = "requires delegated cgroup v2 and unprivileged namespaces"]
fn clone3_always_returns_enosys_without_creating_a_process() {
    let (out, events, code) = probe("clone3\n", 128 << 20);
    assert_eq!(code, 0);
    assert!(events.is_empty());
    assert_eq!(out.trim(), format!("CLONE3_DENIED {}", libc::ENOSYS));
}

#[test]
#[ignore = "requires delegated cgroup v2 and unprivileged namespaces"]
fn external_watchdog_and_stop() {
    let mut config = ExecutorConfig::new(probe_path());
    config.watchdog_grace = Duration::from_millis(200);
    let executor = Executor::new(config, Box::new(TestVerifier));
    let limits = Limits {
        wall: Duration::from_millis(200),
        ..Limits::default()
    };
    let out = executor
        .run(&job(echo(), b"", limits), &CancelToken::new())
        .unwrap();
    assert_eq!(out.termination, Termination::WatchdogKilled);
    assert!(out.timings.kill_latency < Duration::from_secs(1));
    let executor = Executor::new(ExecutorConfig::new(worker()), Box::new(TestVerifier));
    executor.warm_up().unwrap();
    let cancel = CancelToken::new();
    let token = cancel.clone();
    let handle = std::thread::spawn(move || {
        executor.run(
            &job(
                infinite_loop(),
                b"",
                Limits {
                    fuel: u64::MAX / 2,
                    ..Limits::default()
                },
            ),
            &token,
        )
    });
    std::thread::sleep(Duration::from_millis(200));
    let t = Instant::now();
    cancel.cancel();
    let out = handle.join().unwrap().unwrap();
    assert_eq!(out.termination, Termination::Cancelled);
    assert!(t.elapsed() < Duration::from_secs(1));
    assert!(own_groups().is_empty());
    eprintln!(
        "external watchdog and Stop: passed; Stop returned in {:?}",
        t.elapsed()
    );
}

#[test]
#[ignore = "requires delegated cgroup v2 and unprivileged namespaces"]
fn kernel_reports_all_isolation_layers() {
    let (out, events, code) = probe("isolation\n", 128 << 20);
    assert_eq!(code, 0);
    assert!(events.is_empty());
    assert_eq!(out.trim(), "ISOLATION_OK");
    eprintln!("kernel: PID 1, private cgroup root, only loopback, capabilities empty, no_new_privs, seccomp, rlimits, stdio only, environment empty");
}
