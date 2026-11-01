# Phase 0 report

Results of the Phase 0 feasibility gates (PLAN.md §8). Each agent writes only its own sections, with **real command
output**. Never invent results.

## 0.1 Toolchain, workspaces, CI (app-agent)

**Status:** pass locally; CI result below.

Installed on the owner's Windows 11 Pro PC (2026-11-01):

| Tool | Version | How |
|---|---|---|
| rustup | 1.29.1 | `winget install Rustlang.Rustup` |
| rustc / cargo | 1.99.1 (58cf8f9c3 2026-10-13), host `x86_64-pc-windows-msvc` | stable, pinned in `rust-toolchain.toml` |
| MSVC Build Tools 2022 | 17.14.37827.9 at `D:\BuildTools` (MSVC x64 tools + Windows 11 SDK 10.0.22621) | winget, minimal components |
| cargo-deny | 0.20.2 | GitHub release binary, SHA-256 checked against the published `.sha256` |
| Flutter | 3.47.9 stable, Dart 3.13.7, at `C:\src\flutter` (on user PATH) | `git clone -b stable` |

The first Build Tools install (full VCTools workload, `--includeRecommended`, on C:) failed with `0x80070070`
(disk full: C: had 5.4 GB free). The minimal component set installed to D: worked. C: is now about 1.4 GB free.

`flutter doctor` summary:

```
[√] Flutter (Channel stable, 3.47.9, on Microsoft Windows [Version 10.0.26200.9711], locale en-US)
[√] Windows Version (Windows 11 or higher, 25H2, 2009)
[X] Android toolchain - develop for Android devices        (no Android SDK; not needed until 0.9)
[√] Chrome - develop for the web
[!] Visual Studio - develop Windows apps (Visual Studio Build Tools 2022 17.14.44)
    X missing components: C++ CMake tools for Windows (+ v142 tools listed; v143 is installed)
[√] Connected device (3 available)
[√] Network resources
```

The Visual Studio item blocks Flutter **Windows desktop** builds (task 0.8), not Rust. Fix before 0.8 (needs more
free space on C:).

Local checks, `core/` (11 crates) and `server/` (2 crates):

```
cargo fmt --all --check                    ok (both)
cargo clippy --all-targets -- -D warnings  Finished, no warnings (both)
cargo test                                 core: 11 passed, 0 failed; server: 2 passed, 0 failed
cargo deny check                           advisories ok, bans ok, licenses ok, sources ok (both)
```

CI: `.github/workflows/ci.yml`, matrix windows-latest + ubuntu-latest × core/server (fmt, clippy, test) plus
cargo-deny per workspace. Actions pinned by commit SHA.

## 0.2 Wasmtime executor + Windows sandbox (core-agent)

**Status:** pass on the dev PC (Windows 11 Pro 26200, i7-13620H, not elevated). Linux sandbox not built yet:
on non-Windows the executor **refuses every job** (tested), it does not run without an OS sandbox.

Branch `feat/core-exec`, crate `core/crates/nc-exec`, Wasmtime **50.0.1** pinned (`=50.0.1`, features
`cranelift, runtime, std`; no WASI, no cache, no component model).

Layers per job (all required; failure of any one → `ExecError`, job refused):

| Layer | Where | Enforces |
|---|---|---|
| SHA-256 of module = manifest hash | parent, before spawn; re-checked in worker | tampered/wrong module |
| Publisher signature (`ModuleVerifier` trait) | parent, before spawn | unsigned/untrusted module (Ed25519 wired in 0.3) |
| Import allowlist `nc.{input_size,input_read,output_write,checkpoint}` with exact types | worker, before linking | WASI, `env.*`, anything else |
| Fuel + epoch deadline (10 ms tick thread) | worker | CPU loops |
| `ResourceLimiter` (memory, tables, 1 instance/memory/table), `max_wasm_stack` | worker | memory growth, recursion |
| Output / checkpoint caps in host API, plus capped pipe reader in parent | both | huge output |
| Separate single-use `nc-exec-worker.exe` | process | crash isolation |
| AppContainer, **0 capabilities**, cwd = container folder, 4-variable env | OS | files, network, registry |
| Job Object: process+job memory cap, `ActiveProcessLimit=1`, kill-on-close, die-on-unhandled-exception, all UI limits, completion port; `CHILD_PROCESS_POLICY=restricted`; `JOB_LIST` at create; `CREATE_SUSPENDED` | OS | native memory, child processes, leftovers |
| Wall-clock watchdog (wall + grace, 5 ms tick), `CancelToken` | parent | hung worker, Stop |
| Teardown on every path (`Drop` + explicit), start-up `sweep_leftovers()` (registry `AppContainer\Mappings`, owner-PID check) | parent | leftover profiles/folders |

Test results (`cargo test -j 1 -p nc-exec -- --nocapture`, real run on this PC):

```
tests\refusals.rs        5 passed   (wrong hash, tampered module, bad signature, bad limits, missing worker → refused)
tests\runtime_limits.rs 12 passed   (in-process Wasmtime layer, also runs on Linux CI)
tests\sandbox_windows.rs 12 passed  (full stack: AppContainer + Job Object + worker)
unit tests               5 passed
```

Malicious modules through the full sandbox: infinite loop → `OutOfFuel` (fuel 1e8) and `EpochDeadline`
(fuel ~∞, 500 ms budget: call returned in 627–758 ms incl. process start/compile); memory growth (64 MB cap) →
`MemoryLimit`, worker peak 64 MB; 64 KB-per-call output flood (1 MB cap) → `OutputLimit`, no output returned;
deep recursion (512 KB stack) → `StackOverflow`; WASI import → refused; hung worker (stands in for a blocking host
call) → `WatchdogKilled` after 614–724 ms for a 300 + 200 ms budget, kill → exit 2–13 ms. A native 1 GB allocation
inside the sandbox with a 64 MB Job cap → `PROCESS_MEMORY_LIMIT` event, killed (exit `0xE0000001`), no output.

Escape probe (a native test binary started in the sandbox in place of the worker; control = the test process):

```
read  <temp dir secret file>        READ_DENIED 5      (control: readable)
write <temp dir>\planted.txt        WRITE_DENIED 5
read  %USERPROFILE%\NTUSER.DAT      READ_DENIED 5
write %USERPROFILE%\nc-escape.txt   WRITE_DENIED 5
write E:\nc-escape.txt              WRITE_DENIED 5
tcp   127.0.0.1:<test listener>     TCP_DENIED (timed out; listener got 0 connections)
tcp   1.1.1.1:443                   TCP_DENIED 10013   (control: reachable)
dns   example.com                   DNS_DENIED 11001
spawn cmd.exe                       SPAWN_DENIED 367   ("process creation has been blocked")
env                                 ENV_COUNT 4
read  C:\Windows\win.ini            READ_OK 92         (honest limit: world-readable OS files stay readable)
```

Cancel (Stop) latency, warm worker, `cancel()` → `run()` returned (5 runs, cancel 0/20/100/400/1000 ms after
start): **6.7 ms, 150 ms, 57 ms, 40 ms, 54 ms**; kill → process exit 0–12.6 ms. All < 1 s.

Executor start time (echo job, warm binary, 6 runs): profile create 33–124 ms, Job + launch 8–43 ms, compile
16–35 ms (debug build), run < 1 ms, teardown 11–41 ms, **total 105–253 ms**. Numbers vary with load: other agents
were compiling on the same PC and free commit memory was ~1.2 GB.

**Finding: cold first launch of a new worker binary is slow.** A freshly copied `nc-exec-worker.exe` took
**2.1–4.8 s** inside `CreateProcessW` on its first launch (4 fresh copies), then 6–27 ms. Probably the anti-malware
scan of a new image (not proven). Consequences, all handled: the worker is created suspended and only resumed if
no cancel arrived, so a Stop during that window runs no guest code; `Executor::warm_up()` spawns and tears down a
suspended worker without running anything, for the agent to call at start-up and after updates. Open: `run()` still
cannot *return* before `CreateProcessW` does, so the agent's Stop must not wait on a job that is still launching.

Once, under heavy load from other builds, a warm echo run did not finish and was killed by the watchdog at
62 s (wall 60 s + 2 s grace); the worker runs at `BELOW_NORMAL_PRIORITY_CLASS`. The watchdog worked; the cause
(starvation vs. something else) is not confirmed.

Setup facts: the worker's folder needs `ALL APPLICATION PACKAGES:(OI)(CI)(RX)` (`grant_appcontainer_read`, only
that folder; no admin). Profiles are named `nc-exec-<owner pid>-<nonce>`; the sweep deletes those whose owner is gone
and the test confirms the profile and its folder disappear. No leftover profiles after any test.

## 0.3 Identity, manifests, lease store (core-agent)

## 0.4 Tariff profiles, planner, simulator

## 0.5 libp2p across two ISPs (net-agent)

## 0.6 mDNS + QR pairing (net-agent)

## 0.7 Minimal coordinator + relay (net-agent)

## 0.8 Flutter shell + nc-bridge (app-agent)

## 0.9 Android footprint + foreground service (app-agent)

## 0.10 iOS feasibility (app-agent)
