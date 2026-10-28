# Handoff: what is left to do

## 2026-10-28 Claude Code update: phases 9–11 code complete (branch `claude/great-davinci-bch39n`)

All remaining **code** items in this file are done, on top of `integrate/0.3.0` (merged as `b1a4943`). What is left
is **hardware acceptance, which only the owner can run**, then merging and tagging. Owner decisions taken on
2026-10-28: pystray **approved** (optional `[app]` extra), the `-Discovery` firewall rule **approved**, version
bumped to **0.3.0 without merging to `main` or tagging**.

Done (commits `476c7b1`, `1b9e5a1`, `c8cc5e4`, `c8751f1` and the docs commit after them):
- **Phase 9:** `tray.py` (pystray, `run_detached`, queue polled by tk), `autostart.py` (HKCU Run), `notify.py`
  (AUMID toasts → tray balloon → window; strict `accept:/decline:/stop:<id>`; 60 s Ask-me timeout to Decline),
  `radio.py` (Wi-Fi on only from a click), `app.py` (tray, close to tray, `--background`, Start at login switch,
  restart to change roles, sandbox setup offer, requester `wind_down` announces closing), helper consent flow in
  `link_gui.py`, `HelperPolicy` contract object, limits checked while running (+ "stop at once" option), Job Object
  CPU-rate cap (`cpu_rate_percent`), "what is running" view, `nc app open|autostart|reset`. Stop ≤ 1 s is tested on a
  real TLS cluster (`tests/test_stop_helping.py`).
- **Phase 7 leftovers:** `add-firewall-rule.ps1 -Discovery` (+ remove, + GUI button), open `DIRECT-NC-XXXX` groups with
  PBKDF2-derived passphrase, helper auto-join rules from the spike (`wifidirect.AutoJoin`), certificate expiry warning.
- **Phase 10:** store step 4, redundancy twins on a different helper, majority on a mismatch, reliability score,
  auto-block, requester-side Wasm verifier (`verify.py`), helper-side reputation and Block. `tests/test_verify.py`:
  a lying helper on a real cluster is caught, outvoted and dropped. Optional `python.wasm` (§17.6) **not done**.
- **Phase 11:** `examples/wasm/bench` (Rust, built, pinned module in `network_connector/data`), `--engines native,wasm`
  with a Wasm overhead column, `topN` scenarios, device names/short IDs, optional shared speed score; docs (README,
  new `docs/SETUP.md`, JOINING, SANDBOX, SECURITY, CONFIGURATION, BENCHMARK, TROUBLESHOOTING, CHANGELOG 0.3.0);
  security review in `docs/SECURITY_REVIEW_0.3.0.md` (4 findings fixed).
- **2.1b:** the Rust primes example was built (Rust 1.97.0, `wasm32-wasip1`): imports `fd_read`, `fd_write`,
  `environ_get`, `environ_sizes_get`, `proc_exit` (all allowed); prints `78498` under wasmtime. The committed
  `primes.wasm` is still the WAT build.

Verified here (Linux container, Python 3.12, Xvfb): 280+ tests pass; 3 `test_wifidirect.py` tests fail only because
they need Windows (they failed the same way before this work), and the 43+ sandbox/firewall tests are skipped off
Windows. Loopback-alias tests need `NO_PROXY=127.0.0.1,127.0.0.2,127.0.0.3` in a proxied environment.
**Not verified:** anything Windows-only (AppContainer, Job Object CPU rate, toasts, tray on screen, WinRT Wi-Fi/radio,
PowerShell firewall scripts). Run the full suite on Windows first.

Next steps for the owner: section 2.3 hardware tests and `docs/SECURITY_REVIEW_0.3.0.md` "Still to run on Windows",
then PLAN §22.3, then merge `release/0.2.0` and this branch to `main` and tag (2.7.5, 2.8).

## 2026-10-28 Codex partial update (work stopped at owner request)

The owner asked to stop work and update GitHub. The following changes are on
`integrate/0.3.0`; they are **partial Phase 9 work, not a completed 0.3.0 release**:

- `network_connector/app.py` adds a single-window role chooser and embeds the existing
  requester and helper interfaces as tabs. `network-connector` points to it; the old
  `nc gui` and `nc worker-gui` entry points remain. Role switching after the views
  are loaded, tray, autostart, and notifications are still pending.
- Owner Stop now pauses the worker, calls `sandbox.stop_all()` immediately, and
  leaves an open group. The GUI Stop and `nc worker stop` use this path. The
  sub-second hardware acceptance check is still pending.
- Helper settings now include AC-only, input-idle, allowed hours, daily CPU
  minutes per requester, and local blocked device IDs. The helper checks these
  before claiming. Finished CPU time is persisted in `helper_usage.db`. A running
  chunk can finish after a limit is hit; a continuous in-flight quota display and
  immediate-limit option are still pending.
- The worker snapshot includes requester ID, CPU minutes today, policy reason,
  and running-job CPU/RAM estimates. The helper window does not yet display all
  of these details.
- Focused tests: 7 passed for Stop, policy/usage, and app integration
  (with identity creation and discovery mocked). `compileall` passed. A broader
  `tests/test_e2e.py` run was blocked at setup by Windows `PermissionError` on
  test certificate files; this did not exercise the changed worker code.
- Temporary `.test-app/` and `.test-app2/` directories were created by tests.
  Windows ACLs denied their removal. They are not project source and must not be
  included in commits. Other pending work and hardware tests below remain pending.

---

**Project rule:** every change is recorded in [WORKLOG.md](WORKLOG.md) in the same commit (AGENT.md §1 "Work log").

Written 2026-10-28 (updated after the Phase 7 + 8 merge) for the next coding agent (for example Codex) or developer who continues this project. Read
this file first, then [PLAN.md](PLAN.md) (the full design) and [AGENT.md](AGENT.md) (rules, security invariants,
file ownership). Wherever they differ, this file describes the **current state**; PLAN/AGENT describe the **target**.

---

## 0. Project in one paragraph

Network Connector shares CPU work between Windows PCs on the same network.
- A **requester** (called "controller" in the code) splits work into chunks.
- **Helpers** (called "workers" in the code) pull the chunks, run them and send results back.
- Version 0.1.0 is on `main`.
- **0.2.0** adds a Wi-Fi Direct link and benchmarks (PLAN Part 1).
- **0.3.0** (PLAN Part 2) is the current work:
  - any PC can ask for or give performance;
  - one-click joining without pairing codes;
  - owner consent;
  - a WebAssembly sandbox (AppContainer + Job Object + wasmtime), created per job and deleted after.

The stack is Python ≥ 3.10 (tested on 3.12.9), FastAPI + uvicorn over pinned TLS, SQLite, a tkinter GUI, and pytest.

---

## 1. Where everything is right now

### Branches

| Branch | Base | State | Contents |
|---|---|---|---|
| `main` | — | 0.1.0 only | Initial release |
| `release/0.2.0` | main | **2 commits ahead of main, not merged** | `1f6b665` 0.2.0 (Wi-Fi Direct link, benchmarks, cancel-all, metrics, migrations v1); `8ef9116` Phase 6 spike reports + PLAN §17 updates |
| `feat/identity-join` | 8ef9116 | Phase 7, merged into `integrate/0.3.0` | Device identity, signed discovery, one-click join, policies, trust/block, Verify, minimal UI/CLI, docs/JOINING.md |
| `feat/sandbox` | 8ef9116 | Phase 8, 7 commits, merged into `integrate/0.3.0` | Wasm sandbox, `wasm` job kind, modules, example, escape tests, docs/SANDBOX.md |
| **`integrate/0.3.0`** | release/0.2.0 | **Current work branch. Phases 7 + 8 merged, 280 tests passing** (2026-10-28) | Everything above. **Continue from here** |

The agent worktrees were removed after the merge. There is one checkout: `E:/Projects/New/Network Connector` on `integrate/0.3.0`.
`feat/identity-join` and `feat/sandbox` can be deleted once you no longer need them for reference.

### Spike evidence (Phase 6, done)

- [spikes/0.3-sandbox/REPORT.md](spikes/0.3-sandbox/REPORT.md): **all GO.** It contains the proven AppContainer launch recipe, timings, the escape results and the honest limits.
- [spikes/0.3-network/REPORT.md](spikes/0.3-network/REPORT.md): the parts tested on the i7 are GO. **The tests between the two laptops are PENDING** (§6 of that report).
- [spikes/REPORT.md](spikes/REPORT.md): the Part 1 Wi-Fi Direct hardware spike. Results are **PENDING** on the real laptops.

---

## 2. Step-by-step: what to do next

### 2.1 Merge of Phase 7 + Phase 8 — DONE (2026-10-28)

`integrate/0.3.0` = `release/0.2.0` + merge `feat/identity-join` (84a58f0) + merge `feat/sandbox` (cbaad48). **280 tests
pass**, and the machine had no leftover sandbox profiles afterwards. What was resolved, for reference:
- `store.py`: `MIGRATIONS = {1: _m1_measurement, 2: _m2_devices, 3: _m3_sandbox}`, with one `_migrate` loop. Backups: `state.db.bak-0.1.0`
  for v0, `state.db.bak-v{N}` otherwise.
- `controller.py`: the heartbeat returns both `closing` and `group_mode`. `/worker/claim` returns nothing while closing and filters kinds
  by `allowed_kinds(worker_group_mode(worker))`.
- `worker.py`: tracks both `remote_closing` and `_remote_group_mode`.
- `tests/test_join.py`: two tests that gave a joined helper a native job now use a Wasm echo module. **This proves that open groups get
  `wasm` only on the requester side.** The real-TLS join test runs the job in a real sandbox. The helper side is covered by
  `test_helper_refuses_native_kinds_in_an_open_group`.

**Still to check by hand on the i7:**
- `nc sandbox setup`, then `nc sandbox status --check` reports available.
- `nc sandbox run examples\wasm\primes\primes.wasm --input-text "0 1000000"` prints `78498`.

### 2.1b Phase 8 leftovers (Compute side)

- **The Rust example was never compiled.** There was no Rust toolchain on the machine. `examples/wasm/primes/src/main.rs` is written but
  unbuilt; the committed `primes.wasm` comes from the hand-written `primes.wat`.
  - Install Rust and run `rustup target add wasm32-wasip1`, then `cargo build --release --target wasm32-wasip1`.
  - Run the output with `nc sandbox run`.
  - Check that Rust std's WASI imports are all on the allowlist (`modules.ALLOWED_IMPORTS`). If Rust needs an import that isn't
    allowed, **don't widen the allowlist** without a security review; prefer a smaller program or `#![no_std]`-style I/O.
- **CPU-rate cap:** the owner's CPU-rate setting on the Job Object (PLAN §16.2) is not built. It belongs to Phase 9.
- **Progress for `wasm` jobs** is elapsed time ÷ time limit, because the wasmtime Store isn't thread-safe. This is fine; document it in SANDBOX.md if it isn't there.
- **Retries:** time-limit kills, crashes and sandbox start-up errors are retried. Traps, refused modules, memory kills and output caps are not.
- **For Phase 9 (from the Compute agent):**
  - Stop must call `sandbox.stop_all()` **and** pause the agent (`agent.pause()`), so the released jobs aren't claimed again at once.
  - "Give performance" should offer `sandbox.setup_runtime()` (no admin, one ACL grant) when `available()` says "not set up".
  - Keep stripping helper-supplied `info.group_mode` in the heartbeat; the requester-side filter trusts `worker_group_mode(worker)`.
- **`sandbox.available()` side effect:** it creates and deletes one profile per check (cached 30 s), and silently refreshes a stale
  runtime copy if the grant already exists. It never adds a grant.

### 2.2 Decisions the owner (Navod) still has to make

Ask before implementing these:
1. **pystray** (LGPL-3.0) for the tray icon in Phase 9. If it is refused, use a small Win32 `Shell_NotifyIcon` wrapper through ctypes instead.
2. **Discovery firewall rule:** add a `-Discovery` switch to `scripts/add-firewall-rule.ps1`:
   - inbound **UDP 8766** only, program = base `python.exe`;
   - Private profile, plus Public **scoped to the Wi-Fi Direct/hotspot subnet only**;
   - never a blanket "allow python.exe".

   Until then, discovery may be blocked on Public links. The outbound probe ("Search" / Find controller) is the fallback.

### 2.3 Hardware tests the owner must run (need the i7 and i3 together)

Nothing below has been run on real hardware yet. Prepare the scripts and ask the owner to run them, then record the results:
1. **Part 1 (0.2.0) acceptance:** PLAN §9, 12 items: Wi-Fi Direct group, pairing over the direct link, demo, reconnect, impostor refused, benchmark, worker sees results, cancel-all, GUI attach.
   Spike scripts are in `spikes/check-*.ps1` and `spikes/check-link.py`.
2. **Phase 6 network tests:** `spikes/0.3-network/REPORT.md` §6, tests A–D (multicast across PCs on a router, direct group and hotspot; join concurrency; tray, toast and radio on screen).
   Copy the `out\` results into that REPORT.
3. **Phase 7 on hardware:**
   - On the i7: run `nc policy anyone`, or use the Devices tab.
   - On the i3: `nc nearby` should list the i7 within 5 s. Click Join.
   - Check: `Get-NetTCPConnection -State Listen` shows no `nc` TCP listener on the i3. `Get-NetUDPEndpoint -LocalPort 8766` shows only specific addresses.
   - Full checklist: `docs/JOINING.md`.
4. **Phase 8 on hardware:** `nc sandbox setup`, then run `examples/wasm/primes` on both PCs, run the escape suite on both, and check there are no leftover `NC-Sandbox-*` profiles.

### 2.4 Phase 9 — one app, tray, messages, owner limits (code done 2026-10-28; on-screen checks pending)

Spec: PLAN §13, §16.2, §21 (Phase 9). Ownership: the Link side does the UI/tray/messages, the Compute side does the quota enforcement.

1. **`network_connector/app.py`**, launched by the `network-connector` gui-script:
   - First start shows three buttons: **Give performance**, **Ask for performance**, **Both**. The choice is saved.
   - The existing controller window (`gui.py`) and worker window (`worker_gui.py`) become tabs or sections of one window.
     `nc gui` and `nc worker-gui` must keep working.
2. **Tray** (`tray.py`, pending the pystray decision):
   - Use `run_detached()`. Menu callbacks only put actions on a `queue.Queue` that tk polls with `after(100)` (spike-proven).
   - Menu: status, Pause, **Stop helping**, Open, Quit.
3. **Autostart at login**, per user (Startup folder shortcut or `HKCU\...\Run`), no admin rights. There must be a setting to turn it off.
4. **Messages** (`notify.py`):
   - Register the AUMID `NetworkConnector.App` under `HKCU\Software\Classes\AppUserModelId\` (DisplayName, IconUri) on first run, and remove it on reset/uninstall.
   - Use the WinRT `create_toast_notifier_with_id`. If `setting` is not ENABLED, fall back to the tray balloon, then to the app window.
   - **Auto-accept** (default): after joining, show "<name> (<short id>) is using N CPU threads — [Stop]".
   - **Ask me first:** show Accept / Decline. Only accept `accept:<id>` / `decline:<id>` arguments. With no answer, time out to Decline after 60 s.
   - **Never** show job contents, tokens or keys.
5. **Turn auto-accept on** only when `sandbox.available()` is true. The gate already exists in Phase 7 (`link_gui.sandbox_ready()`).
6. **Stop:** from the tray or a message, call `sandbox.stop_all()` and leave every group. All sandboxes must be gone within **1 s**. Add a test for this.
7. **Helper settings panel** (PLAN §16.2 table), stored as `HelperPolicy` in `worker.json` (AGENT.md §4 contract).
   Fields: accept mode, threads, jobs, RAM per job, only plugged in, only when idle N min, hours, daily CPU-minutes per requester, blocked.
8. **Quota enforcement on the helper** (Compute side):
   - Check before every claim **and** while running: AC power (`psutil.sensors_battery()`), idle time (`GetLastInputInfo` via ctypes), allowed hours,
     the per-requester daily CPU-minutes in the `helper_usage` table (step 3), and the block list.
   - When a limit is hit, stop claiming, let the current chunk finish (or stop it if the owner chose "immediately"), and show why.
9. **"What is running" view** on the helper: requester name and short ID, job label, Wasm module SHA-256, CPU/RAM now, CPU-minutes used today.
10. **Wi-Fi radio:** show "Wi-Fi is off — Turn on". Call `Windows.Devices.Radios` `SetStateAsync(On)` **only on that click**. Never turn a radio off.
11. **Tests:** quota logic (mock battery, idle and clock), Stop timing, policy save/load, and message argument parsing. Mock the tray and toasts in unit tests.

### 2.5 Phase 7 leftovers (Link side)

- **Wi-Fi Direct auto-join** (PLAN §15.3; spike consequences in `spikes/0.3-network/REPORT.md` §2):
  - Scan with WinRT `WiFiAdapter.scan_async()` for `^DIRECT-NC-[A-Z0-9]{4}$`, at most every 30 s.
  - **Auto-join only when the helper is not connected to any Wi-Fi.** Check this with WinRT `NetworkInformation`, not netsh text.
    The i7's AX203 has 0 secondary STAs, so assume no concurrency unless the hardware test (B) shows otherwise.
  - When the helper is on Wi-Fi, offer one click: "Join (disconnects <SSID>)".
  - Passphrase = `PBKDF2(ssid, app salt)` (PLAN §15.3). Prefer `connect_with_password_credential_async`; netsh is the fallback. `leave()` deletes the created profile.
- **Discovery firewall rule** after approval (§2.2).
- **Certificate expiry:** certificates migrated from 0.2.0 keep their 2-year expiry. Plan a renewal or a warning.
  Pinned certificates must not change silently, so warn early in the UI and docs.

### 2.6 Phase 10 — check results and trust (done 2026-10-28, except optional item 7)

Spec: PLAN §18.
1. **Redundancy:**
   - For open groups, send a configurable share (default 10 %) of chunks also to a **different** helper.
   - Compare the outputs byte for byte. Record pairs in `job_checks(job_id, twin_job_id, match)` (the table exists from step 3).
2. **On a mismatch:** run a third time on another helper or on the requester itself, take the majority, and mark the losing helper.
3. **Reliability score** in `devices.score` (column exists from step 2): + for matches, − for mismatches, timeouts and abandoned jobs.
   Low scores get fewer chunks. Below a threshold, drop the helper and auto-block it for this requester.
4. **Verifier hook:** an optional small Wasm module that the requester runs **on its own machine, in the same sandbox**, to check each result.
5. **Helper side:** a per-requester reputation (jobs killed for limits or oversized output) and a one-click Block.
6. **Tests:** a fake lying helper in a test `Cluster` must be caught and dropped. Results stay correct by majority.
7. **Optional (§17.6):** ship a pinned, hash-checked CPython WASI build (`python.wasm`, about 25 MB) so requesters can send Python scripts as `wasm` jobs.

### 2.7 Phase 11 — many-helper benchmarks, docs, security review, release 0.3.0 (items 1–3 done; 4–5 need the owner)

Spec: PLAN §19, §22.
1. **Benchmark extensions:**
   - Scenarios: each helper alone (optional when there are many), top N, everyone.
   - New workload `wasm_bench`: the Part 1 `hash_chain` compiled to Wasm (Rust source in `examples/wasm/bench/`, plus a prebuilt module with a pinned hash).
   - Add a "Wasm overhead" column next to the native `benchmark` kind. Phase 6 measured Wasm at about 1.05× native, 1.13× with fuel and 1.22× with epoch.
   - Show device names and short IDs, not IPs.
   - Optional shared speed score, only when the owner allows it.
2. **Docs:**
   - README: features and quick start for the new one-click flow.
   - SETUP: one installer path for any role.
   - Finish JOINING, SANDBOX and SECURITY (all threat rows), and CHANGELOG 0.3.0.
   - Honest limits stated plainly: risk very low, not zero; job inputs are visible to helpers; escaped code could read world-readable OS files but not user files.
3. **Security review:** walk through AGENT.md §1 invariants 1–11 against the merged code, rerun the escape suite, and fix anything found.
4. **Acceptance:** run PLAN §22.3, 12 items, on real hardware.
5. **Release:** bump `network_connector/__init__.py` to 0.3.0, merge to `main` (after 0.2.0), and tag `v0.3.0`.
   Workers install only tagged releases (see `docs/UPDATING.md`).

### 2.8 Release 0.2.0 (can happen before 0.3.0)

- After the Part 1 hardware acceptance (§2.3 item 1) passes, merge `release/0.2.0` into `main` and tag `v0.2.0`.
- If the owner wants it merged earlier, merge it but say plainly that the hardware acceptance is still pending.

---

## 3. Rules you must keep (summary; full list in AGENT.md §1)

- **Never bind `0.0.0.0`/`::` for TCP.** `admin_hosts` = own addresses + loopback only.
- **Never regenerate the controller/device certificate** without a migration. Pinned TLS everywhere, with no hostname check.
- **Helpers open no TCP listening ports.** The only allowed listener is the receive-only, signed, rate-limited UDP 8766 discovery socket.
- **Other people's code runs only as `wasm` jobs inside the full sandbox:**
  - AppContainer with no capabilities;
  - Job Object with a memory cap, `ActiveProcessLimit=1`, a child-process policy and kill-on-close;
  - wasmtime with WASI stdin/stdout/stderr only;
  - a helper-side wall-clock kill.

  If any layer is missing, **refuse the job**. Never fall back to weaker isolation.
- **Open groups run `wasm` only,** enforced on both the requester and the helper side.
- **Every sandbox is deleted after its job** on every path. The start-up sweep removes leftovers.
- **The owner can always Stop** within 1 s. Limits are enforced on the helper itself.
- **Every new endpoint** is admin-only (local), worker-authenticated, or a rate-limited join route, with Pydantic validation and size caps.
- **No `shell=True`.** Validate everything passed to netsh/PowerShell.
- **Never log** tokens, keys, passphrases, pairing codes or job contents.
- **Stale-report checks** by attempt number on every result path.

---

## 4. Known issues and gaps

| Issue | Where | What to do |
|---|---|---|
| ~~Open-group helpers could run native kinds~~ | controller claim + worker | **Fixed in the merge** (tests in `test_join.py`, `test_wasm_jobs.py`). Recheck on hardware |
| ~~Auto-accept UI isn't wired yet~~ | `link_gui.py` | Done: auto-accept / Ask me / Trusted only with messages, gated on the sandbox |
| ~~Rust example never compiled~~ | `examples/wasm/primes/src/main.rs` | Built once; imports all allowed (§2.1b) |
| Toast buttons only work while the app runs | `notify.py` | By design (Phase 6 spike); the tray keeps the app running. COM activation after exit is not planned |
| Optional CPython-in-Wasm (`python.wasm`) | PLAN §17.6 | Not done; needs a pinned, hash-checked ~25 MB download |
| Discovery may be blocked on Public / direct links | Windows Firewall (a Block rule beats Allow) | `-Discovery` rule and button added; outbound probe fallback exists; a Block rule still wins |
| A 0.2.0 controller cert migrated as the identity keeps its 2-year expiry | `identity.py` | Warning from 90 days before; renewal = rotate + re-join (SECURITY.md) |
| An older `controller.json` becomes join policy "code" with announcements off | `config.py` | Intended (existing groups stay closed). Document it in the release notes |
| LPAC (stricter sandbox) doesn't work with Python | sandbox | Not needed for 0.3.0. Revisit later |
| C programs that use `fopen` are refused (`path_open` not allowed) | sandbox allowlist | Documented in SANDBOX.md. Jobs must use stdin/stdout |
| Spike folder `spikes/0.3-sandbox` still has the `ALL APPLICATION PACKAGES (RX)` ACL grant | the spike folder | `icacls "spikes\0.3-sandbox" /remove "*S-1-15-2-1"` once the spikes are no longer needed |
| Leftover temp/test folders Windows wouldn't let the agent delete | `.pytest_cache`, `.test-tmp-link`, `.test-tmp-perf1`, `.test-tmp-perf2`, `.test-tmp-root`, `C:\Users\User\AppData\Local\Temp\nc-smoke-t7fsv29y` | Owner deletes them (admin PowerShell if needed). All are git-ignored or outside the repo |
| `build/`, `dist/`, `network_connector.egg-info/` reappear after installs | repo root | Git-ignored build output. Safe to delete |

---

## 5. Environment notes (save time)

- **Run with the project venv:** `.\.venv\Scripts\python.exe -m pytest -q` and `.\.venv\Scripts\nc.exe ...`.
- **The editable install points at one folder.** In a separate worktree, create its **own** venv:
  ```
  & "C:\Program Files\Python312\python.exe" -m venv .venv
  .\.venv\Scripts\python.exe -m pip install -e .[test]
  .\.venv\Scripts\python.exe -c "import network_connector, sys; print(network_connector.__file__)"   # must point into this folder
  ```
- **wasmtime is pinned at `wasmtime==50.0.1`** in `pyproject.toml`, and already installed in the main `.venv`. API notes are in `spikes/0.3-sandbox/REPORT.md` item 1.
- **Sandbox tests** use the session fixture `sandbox_runtime` (`tests/conftest.py`). It sets up a temp runtime with its ACL grant and
  removes it at the end. The full suite takes about 7.5 minutes now.
- **Sandbox launch details that matter** (from the spike):
  - Start the **base** `python.exe`, not the venv launcher, which is blocked by `ActiveProcessLimit=1`.
  - Pass `LOCALAPPDATA` and `PROCESSOR_ARCHITECTURE=AMD64` in the environment.
  - Use `DETACHED_PROCESS`, not `CREATE_NO_WINDOW`.
  - Set `platform.machine = lambda: "AMD64"` before importing wasmtime.
  - The runtime folder `%LOCALAPPDATA%\NetworkConnector-runtime` gets the single `*S-1-15-2-1:(OI)(CI)(RX)` grant.
  - Never put the runtime inside `%LOCALAPPDATA%\NetworkConnector` (secrets).
- **Tests use a temp `NC_HOME`** (autouse fixture in `tests/conftest.py`). Never touch the real `%LOCALAPPDATA%\NetworkConnector` or a controller/worker the owner is running.
- **Tests that run real child processes, sandboxes and TLS clusters are slow but must run.**
- **Git:**
  - Commit on feature branches. Don't push or merge to `main` without the owner's OK.
  - End commit messages with a `Co-Authored-By:` line for the agent that wrote them, if that is your convention.
- **The owner is on Windows 11 Pro**, i7-13620H (16 threads) + an i3-1115G4 laptop (4 threads), Wi-Fi Intel AX203 on the i7.

---

## 6. Quick checklist (copy into your task list)

- [x] 2.1 Merge `feat/identity-join` + `feat/sandbox` into `integrate/0.3.0`, all 280 tests green, worktrees removed
- [x] 2.1 Open groups run `wasm` only (tested on the requester and helper side)
- [ ] 2.1 Manual on the i7: `nc sandbox setup` / `status --check` / `run primes.wasm`
- [x] 2.1b Build the Rust example once; check its imports against the allowlist (all allowed)
- [x] 2.2 Ask the owner: pystray? discovery firewall rule? (both approved 2026-10-28)
- [ ] 2.3 Owner runs the hardware tests (Part 1 §9, Phase 6 network §6, Phase 7 join, Phase 8 sandbox) → record the results
- [x] 2.4 Phase 9: app.py, tray, autostart, messages, Stop ≤ 1 s, helper settings, quotas, "what is running", radio button (code; on-screen checks pending)
- [x] 2.5 Wi-Fi Direct auto-join (rules from the spike), firewall rule, certificate expiry warning (code; Test B pending)
- [x] 2.6 Phase 10: redundancy, majority, reliability score, auto-block, verifier hook — optional python.wasm not done
- [x] 2.7 Phase 11: many-helper + `wasm_bench` benchmarks, docs, security review, version 0.3.0
- [ ] 2.7 PLAN §22.3 acceptance on hardware, then merge to `main` and tag `v0.3.0` (owner)
- [ ] 2.8 Release 0.2.0 to `main` after its acceptance (or earlier, if the owner says so)
- [ ] Clean-up items in §4
