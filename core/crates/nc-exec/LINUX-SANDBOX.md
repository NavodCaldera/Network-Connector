# Linux sandbox (1.L8–1.L10)

The caller must be an ordinary user in a systemd-delegated cgroup v2 subtree with
`cpu`, `memory` and `pids` enabled in an ancestor's `cgroup.subtree_control`.
The library never changes controller policy, moves the caller, invokes sudo, or
falls back when delegation or namespace creation is unavailable.

The worker binary is trusted, installed application code. `spawn` starts its
bootstrap with an empty environment and a private descriptor 3. An async-signal-safe
pre-exec write to a pre-opened cgroup.procs descriptor places it in the limited
cgroup before even the dynamic loader runs. It cannot read job stdin yet.
The bootstrap creates the user namespace and single UID/GID mappings, then mount,
PID, network, IPC, UTS and cgroup namespaces. A trusted launcher waits for the
PID-namespace init worker. Both processes are in the limited cgroup.

Inside the PID namespace, the worker pivots into a private tmpfs. Only its already
mapped executable and libraries are bind-mounted read-only at /worker and /lib;
no host directory is mounted. `/proc` is private and read-only. `/dev` is absent;
randomness uses `getrandom`. Loopback stays down. After rlimits, no_new_privs,
capability removal and descriptor closure, seccomp is installed. Only then does
`spawn` receive readiness. The worker waits for `resume`, closes descriptor 3,
and begins normal worker IPC. No downloaded or guest code runs during bootstrap.

Seccompiler 0.5.0 supplies the BPF architecture check and compiler. Its
`Apache-2.0 OR BSD-3-Clause` license is allowed by the repository's deny policy.
The default action is KILL_PROCESS. `clone` accepts only pthread's exact thread
flags. A stacked filter always returns ENOSYS for `clone3` so glibc uses the
checked clone path. `execve` and `execveat` remain forbidden, including for memfds.
AArch64 permits the private JIT instruction-stream membarriers used by Wasmtime.

Each cgroup sets memory.max, memory.swap.max=0, memory.oom.group=1, pids.max=32,
and cpu.max="100000 100000". Events come from memory.events and pids.events;
SIGSYS also produces ProcessLimit. memory.peak supplies peak usage, with a polled
memory.current high-water mark for older kernels. Staging directories are private
(0700), carry the cgroup identity, and are removed when that stale cgroup is swept. Kill uses
cgroup.kill; teardown and Drop kill, reap, remove the cgroup and remove staging.
The sweep matches strictly formed names and checks owner PID start times before
removing stale groups, protecting both live owners and reused PIDs.

## Running the real sandbox tests

From `core`, with the pinned Rust toolchain and an active systemd user session:

```sh
CARGO_BUILD_JOBS=2 bash crates/nc-exec/tests/linux-sandbox.sh
```

The harness creates a delegated user scope, moves its shell into a supervisor
child cgroup, and enables controllers only inside that disposable scope. It
builds the real worker/probe, then explicitly runs the ignored Linux tests.
Ordinary `cargo test` does not silently treat unavailable isolation as success;
the privileged-environment tests are visibly ignored until explicitly requested.
CI creates a transient system service with User set to the runner UID and
Delegate=yes, then runs the inner harness as that ordinary user. This needs no
login/user bus. It disables the Ubuntu runner's AppArmor userns restriction for
this step. Production deployments must arrange delegation and
policy explicitly; the library cannot relax these policies.

## Verification record

Environment: Ubuntu 24.04.4 LTS, x86_64, WSL2 kernel
`6.6.87.2-microsoft-standard-WSL2`. No Docker containers or images were created;
no Docker security options were used.

Prerequisite commands and real output:

```text
$ unshare --user --map-root-user --mount --pid --fork --net --ipc --uts --cgroup /bin/true
(exit 0, no output)

$ systemd-run --user --scope -p Delegate=yes bash <disposable prerequisite script>
delegated controllers: cpu memory pids
cgroup limits + cgroup.kill + removal: OK

$ cargo deny check
advisories ok, bans ok, licenses ok, sources ok
(exit 0; existing duplicate-version and missing-license warnings)
```

Required workspace commands targeted `core` (from that directory or with
`--manifest-path core/Cargo.toml`), with `-j 2` for builds, clippy and
tests. Windows used `C:\nc-target-codex`; WSL used its `linux` subdirectory to
separate host artifacts. C: ran short of space, so this path became a junction to
`D:\nc-target-codex`. Subsequent validation disabled incremental compilation and
debug information via environment variables only. No repository profiles were changed.
The Linux Rust toolchain, cargo-deny, ARM64 GCC and diagnostic strace were all
extracted into this disposable directory; no Linux system packages were installed.

| Check | Windows x86_64 | WSL Linux x86_64 |
|---|---|---|
| `cargo fmt --all --check` | exit 0 | exit 0 |
| `cargo clippy --all-targets -j 2 --locked -- -D warnings` | exit 0 | exit 0 |
| `cargo test -j 2 --locked` | exit 0 | exit 0 |
| `cargo deny check` | exit 0 | exit 0 |

The Windows nc-exec OS suite reported:

```text
test result: ok. 12 passed; 0 failed; 1 ignored
```

The ignored test is the cold/warm launch measurement. Existing external-fleet
and Docker-dependent workspace tests retain their original ignored status.
Linux unit tests also exercised both unavailable-layer hooks and diagnostic
redaction (the hooks exist only in cfg(test), with no production env override):

```text
test sandbox::linux::tests::required_layers_refuse_before_starting_worker ... ok
test sandbox::linux::tests::diagnostics_contain_only_os_error_text ... ok
test result: ok. 7 passed; 0 failed; 0 ignored
```

ARM64 cross-checks, using Rust 1.99.1 and an actual ARM64 GCC 13.3 toolchain:

```sh
cargo check -p nc-exec --target aarch64-unknown-linux-gnu -j 2 --locked
cargo clippy -p nc-exec --all-targets --target aarch64-unknown-linux-gnu -j 2 --locked -- -D warnings
```

Both exited 0. A separate C compilation confirmed ELF e_machine=183 (AArch64),
so this did not substitute an x86 C compiler for the native runtime helpers.

Final real-kernel sandbox command:

```sh
bash crates/nc-exec/tests/linux-sandbox.sh
# Builds worker/probe, then runs:
cargo test -p nc-exec --test sandbox_linux -j 2 --locked -- --ignored --nocapture --test-threads 1
```

Real output excerpts (timings are one observed run, not guarantees):

```text
probe clone3: exit=0 events=[] peak=2228224
external watchdog and Stop: passed; Stop returned in 7.107031ms
probe tcp: exit=159 events=[ProcessLimit] peak=2174976
probe exec: exit=159 events=[ProcessLimit] peak=2113536
probe ptrace: exit=159 events=[ProcessLimit] peak=2117632
probe unshare: exit=159 events=[ProcessLimit] peak=2117632
probe mount: exit=159 events=[ProcessLimit] peak=2224128
probe forkbomb: exit=159 events=[ProcessLimit] peak=2351104
probe isolation: exit=0 events=[] peak=2220032
kernel: PID 1, private cgroup root, only loopback, capabilities empty, no_new_privs, seccomp, rlimits, stdio only, environment empty
malicious: Exited(0)
malicious: OutOfFuel
malicious: MemoryLimit
malicious: OutputLimit
probe alloc: exit=137 events=[MemoryLimit, MemoryLimit] peak=67108864
probe threads: exit=0 events=[ProcessLimit] peak=5517312
kill latency=5.575011ms
teardown: processes gone; drop: cgroup gone; sweep: stale removed, live retained
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.94s
```

| Layer | What the real tests checked |
|---|---|
| 1.L8 | Namespace creation with all seven flags; PID 1 and private cgroup view; only loopback; root entries limited to worker/lib/proc; capabilities (effective/permitted/inheritable/bounding/ambient) all zero; no_new_privs; seccomp mode; empty environment; only stdio; exact rlimits; passwd/home reads and outside writes denied; suspended start |
| 1.L9 | TCP socket, direct execve, ptrace of the executor's host PID, unshare, mount and first fork all terminate with SIGSYS/ProcessLimit; clone3 always returns ENOSYS; real Wasmtime echo/loop/memory/output workloads still run |
| 1.L10 | Readback of memory.max=128 MiB, swap=0, pids.max=32, cpu.max=100000/100000; native 64 MiB OOM with MemoryLimit; thread exhaustion reaches pids.max and reports ProcessLimit; kill with launcher plus grandchild under 1 s; no process/cgroup after teardown; Drop cleanup; stale sweep and live-owner retention; stale staging removal |

Failures caught and fixed during development: a moved bootstrap gate prevented
compilation; clippy caught one misplaced SAFETY comment; the first expanded
containment run passed 8/10 but caught Rust's statx metadata syscall missing from
the allow-list and tempfile's default staging permissions not matching the
sweeper's private-directory check. A raw-argument strace confirmed statx was the
killed syscall. The ordinary metadata syscall was added and both production
staging and its fixture explicitly request 0700. The final 10/10 run above used
the normal untraced probe. No isolation layer was disabled to make tests pass.

The CI-style transient service flow was also exercised locally on WSL2, with
`User=1000`, `Delegate=yes`, `--wait --pipe --collect` and the inner harness:

```text
kill latency=6.237284ms
external watchdog and Stop: passed; Stop returned in 13.69857ms
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.95s
Finished with result: success
Main processes terminated with: code=exited/status=0
```

This verifies the service/delegation flow locally, not a GitHub-hosted run.

## Implementation choices and limits of verification

- The initial exec is trusted application bootstrap; seccomp is installed before
  readiness/resume and before any job IPC. There is no permitted post-start exec.
  The private readiness/resume channel is a descriptor-3 stream, closed before job IPC.
- Native forks are rejected at their first syscall. The independent pids.max test
  exhausts native threads, which the kernel pids controller counts, instead of
  permitting a fork bomb solely to test the controller.
- ARM64 was cross-checked and linted, not executed on ARM hardware. The older-kernel
  memory.current fallback was not tested on an older kernel. The Ubuntu CI step
  is provided; this local record does not claim a GitHub-hosted runner result.
- No Docker sandbox flags were used because containment ran directly on WSL2.
  No containers/images were created and no Docker prune command was run.
- Post-push worktree/build-directory cleanup is reported in the final hand-back.
