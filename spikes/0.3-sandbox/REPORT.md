# Phase 6 sandbox spikes (PLAN §17.5): result GO on all 6 items

Python 3.12 + `wasmtime==50.0.1` runs inside a no-capability AppContainer inside a Job Object (memory cap, `ActiveProcessLimit=1`,
kill-on-close), launched from Python with ctypes.
- Every layer held in every escape test.
- No admin rights were needed and no system-wide ACL changed.
- Start-up meets the 300 ms target without a pool, but only with the launch details below.
- Nothing was left behind after 50 runs or after a simulated helper crash.

**Machine:** i7-13620H (hybrid, 6 performance + 4 efficiency cores), Windows 11 Pro 26200, not elevated.
- Python 3.12.9 at `C:\Program Files\Python312`.
- gcc 16.1 (MSYS2), used only for the native comparison.
- The agent harness already runs every process, including the control runs, inside an outer Job Object; the nested jobs worked.

Spike code is in this folder. Raw outputs are in `build\t*.txt` (git-ignored).

## Launch recipe that worked (input for `sandbox.py`)

```
CreateAppContainerProfile("NC-Sandbox-<id>", caps=NULL)            # Windows stores the name in lower case
Job: PROCESS_MEMORY | ACTIVE_PROCESS(1) | KILL_ON_JOB_CLOSE | DIE_ON_UNHANDLED_EXCEPTION, UI limits 0xFF,
     completion port (kill on PROCESS_MEMORY_LIMIT)
STARTUPINFOEX: SECURITY_CAPABILITIES(sid, 0 caps), HANDLE_LIST(3 pipe ends), JOB_LIST(job), CHILD_PROCESS_POLICY(restricted)
CreateProcessW(base "C:\Program Files\Python312\python.exe" -I -S -B runner.py --site <site-packages> ...,
   EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | DETACHED_PROCESS | BELOW_NORMAL_PRIORITY_CLASS,
   env {SystemRoot, LOCALAPPDATA, PROCESSOR_ARCHITECTURE=AMD64, TEMP, TMP}, cwd = container folder)
helper-side wall clock -> TerminateJobObject -> CloseHandle(job) -> DeleteAppContainerProfile
```

Four things broke on the way; each has evidence:
- **No `LOCALAPPDATA` in the environment:** CreateProcessW fails with error 203 (`ERROR_ENVVAR_NOT_FOUND`). With it, Windows points
  `LOCALAPPDATA`, `TEMP` and `TMP` at `%LOCALAPPDATA%\Packages\<name>\AC`.
- **No `PROCESSOR_ARCHITECTURE`:** `platform.machine()` returns `''` because WMI is blocked in the container, and wasmtime then refuses to load
  ("unsupported architecture").
- **`CREATE_NO_WINDOW`:** `conhost.exe` joins the job as a second process. `DETACHED_PROCESS` avoids that.
- **The venv's `python.exe`:** it is a launcher that starts the base python as a child, so `ActiveProcessLimit=1` blocks it
  ("Unable to create process", exit 0x65). Launch the base interpreter directly and add site-packages to `sys.path` by hand.

## Item 1: wasmtime limits and WASI surface (GO)

- **Method:** WAT modules compiled with `wasmtime.wat2wasm`, so no Rust toolchain was needed. Every case ran inside AppContainer + Job.
- **Runner setup:** WASI gets `argv=["module"]`, inherited stdin, and stdout/stderr through capped callbacks. No preopened folders, no environment.
  Store limits (memory, instances, tables, memories = 1), `max_wasm_stack` 512 KB, fuel or epoch.
- **Import allowlist:** checked on `module.imports` before linking, `wasi_snapshot_preview1` only:
  `fd_read/write/close/fdstat_get/seek`, `fd_prestat_*`, `args_*`, `environ_*`, `clock_*`, `random_get`, `proc_exit`, `sched_yield`, `poll_oneoff`.

Results:

```
probe (all WASI allowed)    'P=08 O=08 o=08 E=00 e=00 A=01 S=08'  (prestat, path_open on fd 3 and on stdin with ../.., sock_accept all EBADF; 0 env vars; argc 1)
probe (strict allowlist)    refused_imports [path_open, sock_accept]
import env.system           refused
import sock_open (not p1)   link_error "unknown import"
bad module                  invalid "unexpected end-of-file"
memory bomb, Store 64 MB    memory.grow -1 at 1009 pages, exit 42
recursion, stack 512 KB     STACK_OVERFLOW trap
loop, fuel 2e9              OUT_OF_FUEL after 548 ms
loop, epoch 1 s             INTERRUPT after 1005.8 ms
stdout flood, 1 MB cap      exit 3, stdout_cap
poll_oneoff 10 s sleep      neither fuel nor epoch stopped it; the helper's 3 s wall-clock kill did (3013 ms)
sha_chain 1000              36c1cb4f... matches the hashlib reference
```

**wasmtime-py 50 API notes:**
- `stdout_custom` and `stderr_custom` are property setters, not methods.
- `Config.cache = False` raises an error; caching is already off by default.
- There is no `stdin_bytes`; stdin is either inherited or a file.
- A WASI call with bad arguments raises a trap instead of returning an errno.

**What this means for §17.2:**
- Keep the allowlist strict. C programs that use `fopen` import `path_open` and will be refused; write that into SANDBOX.md.
- When stdout goes over the cap, call `os._exit` from the callback. The process is single-use anyway.

## Item 2: AppContainer (GO)

**ACLs:**
- The base Python needs nothing: `C:\Program Files\Python312` already has `ALL APPLICATION PACKAGES:(RX)`.
- The folder holding the runner and wasmtime needs one grant. It was applied to this spike folder only (no admin, and no parent folders needed):
  `icacls "E:\Projects\New\Network Connector\spikes\0.3-sandbox" /grant "*S-1-15-2-1:(OI)(CI)(RX)"`.
  - Before it, the runner failed with "can't open runner.py: Permission denied".
  - The grant is still in place so the scripts can be re-run; `/remove "*S-1-15-2-1"` removes it.
- Untested: a per-user Python install (`%LOCALAPPDATA%\Programs\Python\...`) would probably need the same grant.

**Method:** the parent opened listeners on 127.0.0.1 and the LAN IP (10.63.182.8). The same probe ran first as a plain process as a control.
For user files only counts and lengths were printed, never contents.

| Test | Control | Sandbox |
|---|---|---|
| TCP to 127.0.0.1 and LAN IP (parent's listeners) | OK, 1 accept each | timed out, 0 accepts |
| TCP to gateway, 8.8.8.8:53, 1.1.1.1:443 | 8.8.8.8 and 1.1.1.1 OK; gateway:80 refused | WinError 10013 |
| DNS lookup | OK | getaddrinfo failed |
| UDP DNS query to 8.8.8.8 | 61-byte reply | `sendto` returns OK but no reply |
| UDP to parent's receivers | 5 + 5 received | 0 + 0 received |
| `listen()` on 0.0.0.0 | OK | OK, but no inbound connection or datagram ever arrived (control's loopback connect was accepted) |
| Read user profile, `.gitconfig`, `.aws`, `%LOCALAPPDATA%\NetworkConnector`, project `AGENT.md` and `.venv` | OK | all Errno 13 / WinError 5 |
| Read `C:\Windows\win.ini`, `hosts`, Program Files | OK | readable (honest limit) |
| Write to its own AC folder | — | OK |
| Write to scratchpad, spike folder, user profile, `C:\Users\Public`, `E:\`, real `%TEMP%` | scratchpad and spike folder OK (only those were tried) | all denied |

Token inside the sandbox: AppContainer = True, Low integrity (S-1-16-4096).

**Stricter LPAC mode** (hides world-readable system files too) is not usable. Even bare `python -c pass` dies with 0xC0000022, including after
granting `ALL RESTRICTED APPLICATION PACKAGES` (that grant was removed again). It is not needed for 0.3.0.

**What this means for §17:**
- Put the sandbox runtime (runner + wasmtime only) in its own folder with the grant, for example `%LOCALAPPDATA%\NetworkConnector-runtime`.
  It must never sit inside the secrets folder `%LOCALAPPDATA%\NetworkConnector`, which the test showed is unreadable from the sandbox.
- `sandbox.available()` should check for that ACL entry.
- The module and its input must reach the sandbox through the stdin pipe (a framed header, then WASI stdin) or the container's own AC folder.
- The docs must not say "bind fails" — binding succeeds, nothing reaches it. They should say escaped code can read world-readable OS files
  but not user files.

## Item 3: Job Object (GO)

```
AC + job limit 1:                   child python and cmd fail: WinError 1816; job reports ACTIVE_PROCESS_LIMIT
AppContainer only, no job:          child python and cmd succeed   <- the container alone does not stop children
AC + CHILD_PROCESS_POLICY:          WinError 367 "process creation has been blocked"
Job 128 MB, allocate 1 GB:          MemoryError after 112 MB plus PROCESS_MEMORY_LIMIT message; with kill-on-message, exit 0xE0000009
Wasm memory bomb, Store 1 GB, Job 128 MB:  killed with 0xE0000009, peak 128 MB
Helper killed mid-job:              sandbox process gone (kill-on-close)
```

**What this means for §17:**
- Use both `ActiveProcessLimit=1` and the child-process policy.
- Use `JOB_LIST`, so there is no suspend/assign/resume race.
- Kill the job when the memory-limit message arrives.
- An `ACTIVE_PROCESS_LIMIT` message is an escape-attempt signal, but only once the `platform` fix in item 4 is in. Without it, Python itself
  triggers three of them per run.

## Item 4: start-up time, 20 runs + 1 warm-up, every run checked (GO, no pool needed)

Times are medians in ms, measured with `perf_counter` in the launcher plus marks inside the runner:

| Scenario | profile | launch | process | `import wasmtime` | delete* | total |
|---|---|---|---|---|---|---|
| A: cold, as first written | 41.9 | 4.0 | 437.1 | 323.2 | 80.3 | 565.5 |
| B: cold + `platform.machine` fix | 40.3 | 4.4 | 169.0 | 55.7 | 81.6 | 295.4 (p90 315) |
| C: as B without the AppContainer | 0 | 4.3 | 169.4 | 49.6 | 55.4 | 237.4 |
| D: pre-started pool | 43.5 | 4.7 | ready 158; job sent to exit 23.6 | 55.1 | 78.4 | 316.8 |

\*The delete column includes spike-only overhead: up to 50 ms waiting for the polling thread, plus a leftover scan. Measured alone,
`DeleteAppContainerProfile` is 8.7 ms median and profile create is 20.1 ms.

- **Why A is slow:** Python 3.12's `platform` module tries WMI and then three `cmd /c ver` spawns, which the job blocks. That adds about 270 ms.
  The fix is to set `platform.machine = lambda: "AMD64"` before importing wasmtime.
- **Critical path for a cold job:** about 215 ms. The delete can run after the result is sent.
- **Pool:** an optional later optimisation, with job latency of about 24 ms. Each pooled sandbox would still be used once, and Stop and the sweep
  must cover idle pool members.

## Item 5: fuel vs epoch, and Wasm vs native (GO, with roles split)

**Speed method:**
- Workload: a SHA-256 chain, 2,000,000 rounds of one 64-byte block. This is the inner loop of the Part 1 `hash_chain`.
- The same code was written in WAT, in C (gcc `-O2` and `-march=native`) and with hashlib.
- 7 repetitions; every run had to print the same digest (9d57f1cc…) or it is invalid.
- Timers sit inside each program around the loop only; Wasm compile time is excluded.

```
gcc -O2                      503.1 ms   x1.00
gcc -march=native            490.2 ms   x0.97
Wasm, no fuel or epoch       530.0 ms   x1.05
Wasm, epoch on               613.8 ms   x1.22
Wasm, fuel on                568.9 ms   x1.13   (8116 fuel per round)
Python hashlib              1181.8 ms   x2.35   (Python per-call overhead)
```

All valid. An earlier 5-repetition run gave the same ratios: 1.07, 1.26 and 1.12.

**Epoch accuracy:**
- With a one-shot timer at a monotonic deadline: 0.25 s stopped at +13 ms, 1 s at +3.8 ms, 3 s at +4.8 ms.
- Counting periodic ticks is wrong on Windows: 10 ms ticks stopped at 1623 ms, 1 ms ticks at 15738 ms. Windows sleeps about 15.6 ms per tick.
- A module doing heavy `fd_write` calls was still stopped at 1003 ms.

**Fuel accuracy:**
- Fuel per second: integer loop 3.7e9 to 5.2e9 (varied between runs), SHA chain 1.9e10 to 2.6e10, `fd_write`-heavy 7.9e5.
- A "1 second" budget calibrated on the integer loop lasts 1036 ms there, 182 ms on the SHA chain, and is never used up on the `fd_write` module
  (it would last about 6527 s). A 10 s wall-clock kill stopped that one.
- A `poll_oneoff` sleep used only 24 fuel in 10 s.

**What this means for §17.1/§17.2:**
- The real time limit must be the helper's own wall-clock `TerminateJobObject`. Kill latency was under 20 ms (3013 ms for a 3 s limit).
- Epoch (one-shot) is optional; it only gives a clean "time limit" message and costs +17 % on this workload.
- Turn fuel on only when the requester sets `limits.fuel` (a deterministic work cap, useful for §18 checks). It costs +8 %.
- Make `fuel_used` in the result optional.
- Untested: the cost with fuel and epoch both on.

## Item 6: cleanup (GO)

- 20 bare profile create/delete pairs: create + folder lookup 20.1 ms, delete 8.7 ms (medians).
- 50 runs cycling six endings: normal (9), leaves 1 MB of files, folders and a TEMP file (9), timeout kill (8), memory kill (8), `abort()` crash (8), stdout cap (8).
- Leftovers right after each delete: none.
- Before and after: `Packages` folders 107 → 107, AppContainer `Mappings` keys 141 → 141, `%TEMP%` matches none.
- Helper killed mid-job: the sandbox process died through kill-on-close. The profile was left behind (its folder and registry mapping);
  the sweep (`DeleteAppContainerProfile` by name, then remove any orphan folder) brought it back to 0.
- More than 400 profiles were created across all scripts, and the leftover check was empty at the end of each one.

**What this means for §17.3:**
- One `DeleteAppContainerProfile` call removes the non-empty AC folder and the registry mapping.
- The start-up sweep looks under `HKCU\Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppContainer\Mappings`
  and matches the `Moniker` value without regard to case.

**No fallback is needed.** If the profile, the job, or the runtime folder's ACL entry is missing, `sandbox.available()` returns
`(False, reason)` and the helper refuses `wasm` jobs. Low integrity alone was not tested and is not proposed.

## Not done / open

- LPAC hardening does not work with Python yet.
- Untested: a per-user Python install, and fuel and epoch both on.
- Native code ran outside the container; Wasm ran inside it.
- The ALL APPLICATION PACKAGES read/execute grant on this spike folder is still in place (`icacls ... /remove "*S-1-15-2-1"` to undo).
