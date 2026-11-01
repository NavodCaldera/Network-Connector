# Work log

Every person or agent who changes this project adds an entry here **in the same commit as the change**
(rule: [AGENT.md §1 "Work log"](AGENT.md#work-log-required)). Newest entry first.

Each entry has:
- **Date, who, branch** and the commits it covers;
- **Asked:** what the request was;
- **Done:** what changed, by file or feature;
- **Decisions:** choices made and who made them (owner decisions are named as such);
- **Verified:** commands run and their real results, including failures;
- **Not done / open:** what is left, what could not be checked, and why.

Never write secrets, tokens, passphrases, pairing codes or job contents into this file.

---

## 2026-11-01 · Claude Code (coordinator) · `main`

- **Asked:** review and merge task 0.1; start the parallel Phase 0 tasks.
- **Done:** reviewed app-agent hand-back for 0.1 (40 files, skeleton only), merged `feat/app-skeleton` into `main`
  (053ab24; TASKS.md/WORKLOG.md conflicts resolved with the branch versions, which already contained the
  coordinator commit). Set user env `CARGO_TARGET_DIR=D:
c-target` because C: has 1.4 GB and E: 3.8 GB free.
  TASKS.md: 0.1 done; 0.4 ready for Codex; 0.2, 0.3, 0.5–0.7, 0.9a in progress; 0.8 blocked.
- **Verified:** relied on the agent's CI run 41292475565 (all 6 jobs green) and its local results; did not re-run.
- **Not done / open:** 0.8 blocked until the owner frees ≥3 GB on C: and the VS "C++ CMake tools" component is added.

## 2026-11-01 · Claude Code (app-agent) · `feat/app-skeleton`

- **Asked:** Phase 0 task 0.1: install toolchains, create the `core/` and `server/` Cargo workspaces, add
  `rust-toolchain.toml`, `deny.toml` and CI for Windows and Linux.
- **Done:** installed rustup 1.29.1, Rust 1.99.1 (MSVC), MSVC Build Tools 2022 17.14 (minimal: x64 tools + Win11 SDK,
  at `D:\BuildTools`), cargo-deny 0.20.2, Flutter 3.47.9 (`C:\src\flutter`, added to user PATH). Created
  `core/` (nc-protocol, nc-identity, nc-policy, nc-tariff, nc-store, nc-blob, nc-exec, nc-net, nc-bridge libs;
  nc-agent bin; nc-cli bin `nc`) and `server/` (nc-coordinator, nc-relay bins), each with a crate doc comment and one
  test. Shared `[workspace.package]` (edition 2021, MIT, rust-version 1.99), placeholder `[workspace.dependencies]`,
  `[workspace.lints]` with `unsafe_code = "deny"`. `proto/` README + placeholder `nc/v1/common.proto`; `infra/`
  and `app/` READMEs; `spikes/phase0/REPORT.md` with headings 0.1–0.10 and the 0.1 section filled in.
  `rust-toolchain.toml`, `deny.toml`, `.github/workflows/ci.yml` (actions pinned by SHA). TASKS.md row 0.1 set to
  review.
- **Decisions:** app-agent: `deny`, not `forbid`, for `unsafe_code`, so nc-exec/nc-identity can allow it per module.
  Clippy `unwrap_used`/`expect_used` set to warn (errors in CI through `-D warnings`). Build Tools went to D: because C:
  is short of space.
- **Verified:** fmt ok, clippy clean, tests core 11/11 and server 2/2 passed, `cargo deny check` ok for both. The first
  Build Tools install failed with 0x80070070 (disk full on C:). CI result: see the hand-back.
- **Not done / open:** `flutter doctor` still flags Visual Studio as missing "C++ CMake tools"; this blocks Flutter
  Windows builds (0.8) and needs space on C: (about 1.4 GB free now). No Android SDK (0.9).

## 2026-11-01 · Claude Code (coordinator) · `main`

- **Asked:** start Phase 0; give the owner tasks for Codex that don't collide with the Claude agents; keep a log.
- **Done:** launched app-agent on task 0.1 (branch `feat/app-skeleton`, running). Added `TASKS.md` (task board with
  owner, branch and allowed files per task). Assigned to Codex: 0.4 tariff engine (after 0.1 merges), 0.9b Play
  policy document, 0.10 iOS feasibility document. AGENT.md §2 now points to TASKS.md.
- **Decisions:** coordinator: Codex gets self-contained tasks only (one crate or one document) so file sets never
  overlap; every Codex branch is reviewed by the coordinator before merge.
- **Verified:** documents only.
- **Not done / open:** 0.1 still running.

## 2026-11-01 · Claude Code (coordinator, local Windows) · `main`

- **Asked:** bring the platform work to `main`; delete all unnecessary files, code and folders.
- **Done:** fast-forwarded `main` to `platform/v1` (5a8b833). Removed the legacy Python app from `main`
  (`network_connector/`, `tests/`, `scripts/`, `spikes/`, `docs/`, `pyproject.toml`, `Start.cmd`, `.env.example`,
  `CHANGELOG.md`); all of it remains on `legacy/python-v0.3`. Rewrote README.md and HANDOFF.md. AGENT.md, PLAN.md
  and the three agents now point to the legacy branch and use `main` as the trunk. `.gitignore` gained Rust/Flutter
  entries. Deleted local untracked leftovers: `.venv`, `build`, `dist`, `*.egg-info`, `.windows-validation`,
  `.controller-debug`, `.ryzen-test`, three `.test-app-*` folders.
- **Decisions:** owner: clean `main`. Coordinator: keep LICENSE, CLAUDE.md, WORKLOG.md, .gitattributes.
- **Verified:** `git status` clean after commit; top-level now holds only docs, LICENSE and `.claude/`.
- **Follow-up:** the 14 ACL-locked local folders (`.pytest_cache`, `.test-*`) were then deleted with an elevated
  `takeown` + `icacls /reset` (owner approved the UAC prompt). Project folder now holds only the tracked files.
- **Not done / open:** The other PC still pulling `main` will no longer get the Python app; it must
  switch to `legacy/python-v0.3`.

## 2026-11-01 · Claude Code (coordinator, local Windows) · `platform/v1`

- **Asked:** on a new branch, write AGENT.md and PLAN.md for the new platform and create three agents to build it.
- **Done:** created branch `platform/v1` from `main`. Rewrote `PLAN.md` (goal, device roles, decided stack, hybrid
  architecture, repo layout, security model, night-data tariff scheduling, owner control, phases 0–3, engineering
  standards) and `AGENT.md` (11 security invariants, work method, Phase 0 task split, ownership and contracts).
  New agents `.claude/agents/{core,net,app}-agent.md`; the Python-era `link-agent`/`perf-agent` were removed here
  (still on `legacy/python-v0.3`). Banner added to HANDOFF.md.
- **Decisions:** owner: adopt the researched stack, LAN + internet, industry grade, new branch. Coordinator:
  desktop + Raspberry Pi MVP before Android, iOS last; the Python app stays on this branch as read-only reference;
  three agents split by core / networking + server / app + CI.
- **Verified:** documents only; no code built. `cargo`, `rustc`, `flutter` are **not installed** on this PC
  (checked with Get-Command). Git and Docker are present.
- **Not done / open:** Phase 0 not started. Needs the Rust and Flutter toolchains installed first (task 0.1).
  `main` was not changed in this step because the owner asked for a new branch.

## 2026-11-01 · Claude Code (local, Windows) · `main`, `legacy/python-v0.3`

- **Asked:** keep the current Python version safe in a branch; do not start on new technologies yet; give a
  Perplexity research prompt for the target platform (PCs + phones + any capable device, LAN and internet).
- **Done:** created and pushed `legacy/python-v0.3` from `main` at febfb85. No code changes. Research prompt given
  in chat.
- **Decisions (owner):** platform must work across the internet as well as LAN; must be industry-grade; no move to
  new technologies until the owner says so. Proposed (not adopted): Rust core, Flutter UI, Wasm jobs.
- **Verified:** `git push -u origin legacy/python-v0.3` succeeded; branch visible on remote.
- **Not done / open:** technology choice pending owner's research.

## 2026-11-01 · Claude Code (local, Windows) · `main`

- **Asked:** research how the joiner can keep internet (not via the host); build the "joiner keeps its own Wi-Fi"
  option; give a prompt for the other PC.
- **Research:** a legacy group makes the joiner leave its router (one infrastructure connection per adapter).
  Microsoft's WDI docs say one adapter can run an infrastructure connection and Wi-Fi Direct at once. This PC's
  adapter: Wi-Fi Direct Client/GO supported, P2P Clients Port Count 1.
- **Done:** new `p2p.py` with native Wi-Fi Direct (WinRT). `Host` = advertisement publisher (discoverable) +
  connection listener: accepts, pairs push-button with an auto-accept handler, opens WiFiDirectDevice and reports
  the link IPs. `Joiner` = finds ASSOCIATION_ENDPOINT devices, pairs (GO intent 0), connects and returns the host's
  link IP. The window uses it by default; a "Classic mode" checkbox keeps the old legacy group. If several devices
  are found, a picker asks which one. The window now shows any exception type. Added `winrt-Windows.Networking` to the
  extra. Rebased onto 5ea657c (other session's Stop rework) and merged: Stop also closes native host/joiner.
- **Verified:** pytest 21 passed. On this PC: `Joiner.find()` listed WINDOWS_LAPTOP. `Host.start()`/`stop()`
  advertised and stopped cleanly.
- **Not done / open:** pairing, connect and Hello over the native link are not tested between the two PCs. It is
  unknown whether Windows still shows its own "allow device" prompt. The joiner's adapter capabilities are not yet
  checked. No unit tests for p2p.py (needs hardware).

---

## 2026-11-01 · Claude Code (local, Windows) · `main`

- **Asked:** add a Stop button for the host.
- **Result before the change:** first real two-PC run worked. In the window: Firewall fixed (one UAC Yes), group
  DIRECT-NC-JXSS, and repeated Hellos from Windows_Laptop (192.168.137.50) answered by DESKTOP-VMRF6PJ.
- **Done:** `hello.serve(..., stop=Event)` (0.5 s accept timeout). The window has a **Stop** button, enabled while
  hosting: it stops the listener and the Wi-Fi Direct group, then re-enables Host/Join. Closing the window also stops
  the listener.
- **Verified:** pytest 21 passed (new: the listener exits on stop and frees the port). The Stop button was not
  clicked on hardware.

---

## 2026-11-01 · Claude Code (local, Windows) · `main`

- **Asked:** update the local copy; restart the app; explain the "Hosting DIRECT-NC-JXSS" screenshot.
- **Done:** pulled 29f2976 (Retry button, from another session). Restarted the window. Found a bug: the firewall
  check matched only the interpreter that was running, so under the window's `pythonw.exe` it missed the Block
  rules for `python.exe` (and the reverse). `firewall._pythons()` now matches both python.exe and pythonw.exe.
- **Verified:** `problems()` under both python.exe and pythonw.exe reports "2 rule(s) block Python". pytest 20 passed.
- **Not done / open:** the two "Python" Block rules (python.exe only) are still enabled here. They don't affect
  the window, which listens as pythonw.exe; the next Host click offers to disable them.

---

## 2026-11-01 · Claude Code (local, Windows) · `main`

- **Asked:** a GUI on both PCs: run the app, click Yes, and see the messages passing.
- **Done:** `gui.py` (Tkinter, one window for both roles). **Host** checks the firewall (one UAC prompt), starts
  `DIRECT-NC-XXXX` (retries "aborted" twice after 10 s) and logs every Hello received and the reply sent.
  **Join the other PC** scans, joins with the built-in password and sends Hello; **Send Hello** sends another.
  `Start.cmd` (double-click) creates `.venv` and installs on first run, then opens the window with pythonw.
  `nc-gui` gui-script added. README now leads with the window.
- **Verified:** pytest 20 passed, including a GUI test where the window sends a Hello to a local server and logs
  both directions. `Start.cmd` opened the "Network Connector - Hello" window on this PC.
- **Not done / open:** Host and Join buttons not clicked on real hardware yet. The first-run install branch of
  `Start.cmd` was not run here (`.venv` already existed).

---

## 2026-11-01 · Claude Code (local, Windows) · `main`

- **Asked:** an easier fix for the firewall than pasting an admin PowerShell command.
- **Done:** new `firewall.py`. `nc host` checks for enabled inbound Block rules on this Python
  (`sys._base_executable`) and for the Hello allow rule. If either is wrong, it shows one Windows admin prompt
  (Start-Process -Verb RunAs, encoded command) that disables those Block rules and adds the TCP 8765 LocalSubnet
  rule. Fixed README lines damaged by Python string escapes (`\a`, `\n`). Stopped Claude's own host so the owner can
  run `nc host`.
- **Decisions:** the fix runs only after the user clicks Yes on the UAC prompt. Claude did not run it itself
  (Claude Code refused to weaken the firewall). It is limited to Block rules for this exact python.exe.
- **Verified:** `firewall.problems()` here reports "2 rule(s) block Python" (the allow rule is present). pytest:
  19 passed. `fix()` itself not run.
- **Not done / open:** the elevated fix and the two-PC Hello are still untested.

---

## 2026-11-01 · Claude Code (local, Windows) · `main`

- **Asked:** run the app between two PCs; then make joining easier, with no password to type; then push `main` after
  every change.
- **Done:** merged PR #3 (Hello-only app) into `main`. Started `nc host` here and added the TCP 8765 firewall rule
  (UAC). The second PC joined `DIRECT-NC-8OES` (got 192.168.137.168), but its Hello timed out. Cause: two enabled
  inbound **Block** rules named "Python" (Public profile, `C:\program files\python312\python.exe`), which override
  the allow rule. Claude Code would not disable them (security block); the owner was given the command.
  New: `nc host` with no options uses `DIRECT-NC-XXXX` (from the computer name) and a PBKDF2-derived built-in
  password; `nc join` with no arguments finds the group with `netsh wlan show networks` and joins with that password.
  Test added. README updated.
- **Decisions:** the built-in password is the same in every copy of the app (owner asked for no password); README
  says so. Scanning uses netsh, so no new WinRT package is needed.
- **Verified:** pytest 18 passed. `nc host` started `DIRECT-NC-JXSS` here. One `nc host` run in the owner's admin
  window failed with "Windows aborted Wi-Fi Direct". A second run failed with "already running", because Claude's
  host held the group. Starting again about 10 s after a stop worked.
- **Not done / open:** `nc join` with no arguments is not yet tried on the second PC. The netsh scan only sees the
  networks Windows has cached (it retries for 30 s). The "Python" block rules are still enabled unless the owner
  ran the command.

---

## 2026-11-01 · Claude Code (local, Windows) · `simple-hello` (new branch from cd2e4a3)

- **Asked:** make the application very simple: two PCs just send a Hello message to each other. The owner chose
  "strip the app down" and "plain TCP for now".
- **Done:** removed every module except `wifidirect.py` (open groups, AutoJoin and TLS probe also removed;
  `protect_path` moved in), and every test except `test_wifidirect.py` (trimmed) and the new `test_hello.py`. Added
  `hello.py` (one line each way, 200-byte cap, 10 s timeout) and a new `cli.py`: `nc host` (start group, print
  SSID/password, answer Hellos, stop group on exit), `nc join SSID PASS` (join, send Hello to gateway/.1),
  `nc send IP`, `nc listen`. Removed examples, setup scripts and the GUI entry points. The firewall scripts now add one
  inbound TCP 8765 rule (Private+Public, LocalSubnet). pyproject has no runtime dependencies; version 0.4.0. README
  rewritten; notes added at the top of HANDOFF.md and AGENT.md; CHANGELOG entry.
- **Decisions:** worked on a new branch so the full app stays intact on `claude/great-davinci-bch39n`. The plain TCP
  Hello breaks AGENT.md's TLS invariant on purpose (owner's choice); recorded in the AGENT.md note. PLAN.md, docs/,
  spikes/ and `.claude/agents` kept as history and not updated.
- **Verified:** `.venv` pytest: 17 passed. `nc listen --bind 127.0.0.1 --once` plus `nc send 127.0.0.1` as two
  processes: both sides printed the other's "Hello from DESKTOP-VMRF6PJ".
- **Not done / open:** `nc host`/`nc join` not run on two real PCs (needs the i7 and i3). The firewall scripts were
  not run.

---

## 2026-11-01 · Claude Code (local, Windows) · `claude/great-davinci-bch39n` · ae6c341

- **Asked:** remove the Mobile Hotspot path (mode B) from the whole system; use Wi-Fi Direct only.
- **Done:** `wifidirect.DirectGroup.start` no longer adopts an existing owner adapter: it refuses with "Another Wi-Fi
  Direct group or Mobile Hotspot is already running". `status()` reports interface/IP only for our own group (mode A),
  so mode "B" no longer exists. `controller._refresh_direct_subnets` uses the group status instead of any owner
  adapter, so a hotspot subnet is no longer trusted as a direct subnet. Hotspot wording removed from errors, the GUI
  Wi-Fi Direct tab, the CLI help and `add-firewall-rule.ps1`. Hotspot test replaced by
  `test_existing_hotspot_is_neither_adopted_nor_reported`.
- **Decisions:** refuse rather than silently reuse an external group. Design docs (PLAN/AGENT/HANDOFF/ACCEPTANCE)
  still describe mode B; not edited, because the owner then asked to strip the app down to a Hello exchange.
- **Verified:** full `.venv` pytest: 345 passed.
- **Not done / open:** not tried on hardware. ae6c341 also carries the uncommitted 2026-10-29 changes below; this
  entry was committed separately because the first edit failed.

---

## 2026-10-29 · Claude Code (local, Windows) · `claude/great-davinci-bch39n`

- **Asked:** find out why Windows Mobile Hotspot was on, with a helper connected, but the Wi-Fi Direct tab still showed
  "Mode: A | Stopped, Interface: -, IP: -".
- **Done:** `wifidirect._read_adapter_rows`: `Get-NetIPConfiguration` threw for a hidden adapter that has no IP interface
  (here ifIndex 17). `-ErrorAction SilentlyContinue` did not catch the error raised inside the module under
  `$ErrorActionPreference='Stop'`, so the whole PowerShell query exited 1 and the app saw **no adapters at all**. It
  then never detected the hotspot and fell back to starting its own Wi-Fi Direct publisher. That call is now wrapped in
  `try { ... } catch { $null }` for each adapter.
- **Decisions:** one-line fix. The command is still constant (nothing interpolated). Security invariants unchanged.
- **Verified:** before the fix the query returned rc 1 and `direct_interfaces()` returned `[]`. After it,
  `direct_interfaces()` returns the "Microsoft Wi-Fi Direct Virtual Adapter #4" at 192.168.137.1/24, owner=True,
  peer 192.168.137.158. `.venv` pytest `-k "direct or wifi or link or discovery"`: 89 passed.
- **Also:** `link_gui.firewall_status` always said "Firewall rule needs updating" because Windows reports the rule's
  remote address as `192.168.137.0/255.255.255.0` and the code string-compared it to `192.168.137.0/24`. It now
  compares parsed networks. With the real rule: "Direct-link rule enabled for this address and subnet."
  `-k "firewall or link_gui or direct"`: 44 passed. App relaunched (owner approved); controller listens on 192.168.137.1:8765.
- **Also:** owner reported PowerShell windows popping up. Under pythonw every console child opens a window unless
  started with CREATE_NO_WINDOW; only `wifidirect._run` and the job runner did. Added it to the three PowerShell calls in
  `link_gui.py` (firewall status runs on every Wi-Fi Direct refresh), `security.protect_path` (icacls) and the two
  icacls calls in `sandbox.py` (Windows-only flag, 0 elsewhere). `-k "firewall or link or direct or sandbox or
  security"`: 115 passed. Not yet seen in the relaunched GUI. Same flag added to `updater._run` (git/pip).
- **Also (GUI slowness):** owner said the interface is very slow and asked about another UI technology. Measured: the
  adapter query took 12.3 s per call and the firewall check 4.0 s, both started on every 5 s Wi-Fi Direct refresh.
  Tkinter itself was not the cause (work already runs on background threads). Rewrote the adapter query as one bulk
  call per cmdlet joined by ifIndex, without `Get-NetIPConfiguration` (gateway from `Get-NetRoute 0.0.0.0/0`): 2.6-2.9 s,
  same detected interface and peer. The firewall check now reruns only when the address changes, once a minute, or
  after "Allow through firewall". `-k "firewall or link or direct or wifi or discovery or config"`: 90 passed.
  Recommended keeping Tkinter. Not yet seen in the GUI.
- **Also (controller would not start):** popup "Port 8765 is used by another program (PID …, pythonw.exe)" where the
  PID was the app itself. Cause: `controller.json` `bind_hosts` held 192.168.137.1, and the hotspot had turned off
  (Windows hotspot power saving), so that bind failed after 10.63.182.8 and 127.0.0.1 had bound; `port_in_use_message`
  then found the app's own listener. Fixes in `controller.py`: `start()` skips an extra saved address this PC no longer
  has (logged, kept in config); main address and loopback still must bind. `port_in_use_message` ignores this
  process's own listeners and says when the address is missing. Two tests added in `tests/test_local_connection.py`;
  that file plus `test_controller.py`: 13 passed. App restarted at the owner's request.
  Open: how 192.168.137.1 got persisted although the direct listener is added with `persist=False` (likely a later
  `save_config` of the in-memory cfg); not investigated.
- **Not done / open:** no regression test (the failure needs a real hidden adapter). The GUI was not re-run here.
  Separately, the saved controller `bind_host` is 172.21.224.1, the WSL/Hyper-V vEthernet address, which the helper
  cannot reach. The direct-link listener adds 192.168.137.1 once the hotspot is detected.

---

## 2026-10-29 · Codex · `claude/great-davinci-bch39n`

- **Asked:** fix the controller startup popup, "Unable to configure formatter 'default'".
- **Done:** explicitly disable Uvicorn console colours so its formatter does not call
  `sys.stdout.isatty()` in Windows GUI/pythonw launches, where stdout is None.
  Added a regression test starting a real TLS controller with stdout/stderr absent.
- **Decisions:** logging-only change; TLS, binds and security invariants unchanged.
- **Verified:** targeted controller tests: 4 passed, 1 cache-permission warning.
  Initial sandboxed run: 4 setup errors due to temp-directory access; rerun outside
  the execution sandbox passed. Full suite started; result pending.
- **Not done / open:** the existing GUI process needs reopening to load this change;
  Wi-Fi Direct hardware acceptance is still pending.

---

## 2026-10-29 · Claude Code (local, Windows) · `claude/great-davinci-bch39n`

### Acceptance sections 1-2 on a third laptop (Ryzen), run as "i3"

- **Asked:** clone/update the repo, read `docs/ACCEPTANCE_0.3.0.md`, do everything possible on this laptop, then update `main`.
- **Done:** installed Git and Python 3.12.10 (winget), switched to this branch (commit `238819b`), built `.venv`
  (`.[test,app,wifidirect]`, version 0.3.0, import path inside the checkout), ran
  `collect-local.ps1 -PC i3 -FullTests`, then `nc sandbox setup --remove` in the isolated shell. No source changed.
- **Machine:** AMD Ryzen 3 3250U, 4 threads, about 3.4 GB RAM. This is **not** the i3-1115G4; the "i3" label was chosen
  by the owner so the script would run. It cannot count as the real i3 result.
- **Verified:** full suite **342 passed, 0 failed, 0 skipped** (1 deprecation warning), 15 min 19 s. `sandbox setup`
  and `status --check` OK (self-check 5140 ms, profile deleted); primes printed `78498` (0.70 s); bench printed digest
  `f21b6b5f...cab6a` (1 unit, 1.76 s); leftover `NC-Sandbox-*` profiles: 0 before and after.
- **Not done / open:** sections 3-7 (app/tray/notifications, join/consent, Stop <= 1 s timing, quotas, direct link and
  hotspot, firewall rules, two-PC benchmark, listener checks) need the real i7 and i3 and a person at the screens.
  Hardware acceptance is still pending; no tag was created. Result files stay local under
  `spikes/0.3-windows/out` (git-ignored).

---

## 2026-10-29 · Claude Code (local, Windows) · `claude/great-davinci-bch39n`

### Windows validation, i7 checks and acceptance preparation (finishes Codex's interrupted work)

- **Asked:** run the full suite on Windows (never done before), check the i7, fix real failures without weakening
  security, prepare the i3 acceptance steps, commit and push. No merge to `main`, no tags. The owner confirmed Codex had
  stopped (token limit) and asked me to finish.
- **Done:**
  - Reviewed and kept Codex's uncommitted work: `winlimits.limit_memory` now closes the job and returns `None` when the
    CPU-rate cap can't be set, and `worker._start_job` kills and refuses the job in that case (the owner's cap is never
    silently skipped); two new tests for it (`test_helper_policy.py`, `test_sandbox.py` real Job Object check of the
    25 % cap and sandbox refusal); `conftest.py` pins `psutil.sensors_battery` so integration tests don't depend on
    charging; `docs/ACCEPTANCE_0.3.0.md`; `spikes/0.3-windows/collect-local.ps1`.
  - Fixed `tests/test_wasm_bench.py::test_wasm_engine_needs_helpers_with_a_sandbox`: it set `worker._info["sandbox"] = False`
    by hand, but every heartbeat re-reads `sandbox.available()`. It only passed where no sandbox exists (Linux). Now the
    test makes `sandbox.available` return False and waits until the controller has seen it. Product code unchanged.
  - Fixed `collect-local.ps1`: with `$ErrorActionPreference='Stop'` and `2>&1`, Windows PowerShell 5.1 turns the first
    line `nc` prints to stderr (`[ok] exit code ...`) into a terminating error, so the script died after the primes run.
    Native commands now run through `Invoke-Logged` (`Continue`, stderr logged as plain text).
  - `.gitignore`: `.test-tmp*/` widened to `.test-*/` (Codex's `.test-app*`, `.test-policy*`, `.test-stop` folders are
    Windows-ACL-locked and must not be committed).
  - Checked that the environment variables used by the acceptance steps exist (`NC_PORT`, `NC_CONTROLLER_PORT`,
    `NC_DISCOVERY_PORT` come from config's generic `NC_<FIELD>` override; the discovery code uses `cfg.discovery_port`).
- **Decisions:** the stricter behaviour (refuse the job if the owner's CPU cap can't be set) stays; nothing in the
  security invariants was loosened.
- **Verified (real results, this i7, Windows 11, Python 3.12.9, commit `7c1c178` + the uncommitted changes above):**
  - Baseline full suite with Codex's changes: **341 passed, 1 failed** (the test fixed above), 11 min 38 s. This was the
    first run of the Windows-only escape suite, wasm job tests, firewall harness and CPU-cap tests: all passed.
  - Re-run of `test_wasm_bench.py` + `test_helper_policy.py`: 13 passed.
  - Final full suite: **342 passed, 0 failed, 0 skipped, 1 warning (Starlette deprecation)**, 7 min 18 s.
    `sandbox.leftovers()` afterwards: `[]`.
  - `collect-local.ps1 -PC i7` (isolated `NC_HOME`/`NC_RUNTIME_HOME` under `spikes\0.3-windows\out`): `nc sandbox setup` ok;
    `status --check` available, self-check ok (2625 ms, profile deleted); primes printed `78498` (0.39 s, peak 36 MB);
    bench digest `f21b6b5f35b38489a4d9e714089c3df49b59b43de6c0efb2f9d37b43f43cab6a` (matches), 1 unit; leftover profiles
    `[]`; 0 failed commands. (The first run of this script crashed on stderr; see the fix above.)
  - App launch (`network-connector.exe`, isolated home, ports 18765/18766): process alive after 10 s, no output/errors,
    the process owned a visible `TkTopLevel` window titled "Network Connector" (the role chooser, as no role was saved)
    and a `...SystemTrayIcon` window (pystray created its tray window). Probe process was terminated afterwards;
    no sandbox profiles left.
- **Not done / could not be checked (be clear: these are NOT verified):**
  - What the app looks like: tray icon image and right-click menu, a real toast/notification, the "Start at login"
    switch (HKCU Run value), close-to-tray, Both/Give/Ask role screens. I cannot see the screen; the owner must check
    these (acceptance doc §3).
  - Everything that needs the i3: join/consent, Stop <= 1 s on hardware, Wi-Fi Direct group and hotspot, auto-join, the
    discovery firewall rule on a Public link, `Get-NetTCPConnection` listener check on the i3, two-PC benchmark, quotas
    with real unplugging. Hardware acceptance is **pending**; nothing was merged to `main` or tagged.
  - Optional `python.wasm` (PLAN §17.6) was not built.
  - The Windows-locked folders `.test-app*`, `.test-policy*`, `.test-stop`, `.test-tmp-*`, `.pytest_cache` could not be
    deleted by this session (access denied); they are git-ignored.

## 2026-10-28 · Codex · `claude/great-davinci-bch39n`

### Windows validation and acceptance preparation (interrupted: Codex ran out of tokens; finished by Claude Code in the entry above)

- **Asked:** run the complete Windows suite and isolated i7 checks, fix real
  failures without weakening isolation, prepare exact i3 acceptance steps, and
  push this branch only after tests pass. No main merge or tags.
- **Done:** fetched and checked out the requested branch; read CLAUDE, WORKLOG,
  HANDOFF, AGENT §1, PLAN and the security review. Added
  `docs/ACCEPTANCE_0.3.0.md` and `spikes/0.3-windows/collect-local.ps1` for isolated
  per-PC evidence; ignored validation artifacts.
- **Decisions:** keep all application state/runtime copies separate from the
  owner's real data. Two-laptop outcomes stay pending until observed/reported.
- **Verified:** Python 3.12.9 venv initialized from
  `C:\Program Files\Python312\python.exe`; `pip install -e .[test,app]` succeeded;
  import resolves to this checkout. Full baseline Windows suite is running.
- **Not done / open:** baseline/final summaries, actual i7 UI and sandbox results,
  and fixes will be recorded here before the corresponding commit. Hardware
  acceptance and actual login persistence remain owner-run checks.

## 2026-10-28 · Claude Code (cloud session) · `claude/great-davinci-bch39n`

### Session 2: work-log rule

- **Asked:** create a log file that records everything done, and make updating it a project rule.
- **Done:** added this `WORKLOG.md` with the history below; the rule is in `AGENT.md` §1 ("Work log"), in both agent
  definitions (`.claude/agents/link-agent.md`, `perf-agent.md`) and in a new `CLAUDE.md` that Claude Code loads
  automatically; `HANDOFF.md` points to the log.
- **Verified:** documentation-only change; no code touched.

### Session 1: finishing the remaining 0.3.0 work in HANDOFF.md

Commits: `476c7b1`, `1b9e5a1`, `c8cc5e4`, `c8751f1`, `a2e184e` (base: `b1a4943`, the merged `integrate/0.3.0`).

- **Asked:** "read the repo and HANDOFF.md fully and do the left parts".

**Owner decisions (asked first, as HANDOFF §2.2 requires):**
- pystray for the tray icon: **approved** (optional `[app]` extra, LGPL-3.0 used unmodified).
- `add-firewall-rule.ps1 -Discovery` (UDP 8766, base python.exe, Private + direct-link subnet): **approved**.
- Release: **bump to 0.3.0, don't merge to `main`, don't tag** (hardware acceptance pending).

**Environment:** Linux cloud container, Python 3.12 venv, Xvfb for tkinter tests. No Windows, no PowerShell
(downloading PowerShell was refused by the session's safety policy). `rustup target add wasm32-wasip1` was installed.

**Phase 9 — one app, tray, messages, owner limits** (`476c7b1`)
- New `tray.py` (pystray `run_detached`; menu clicks only queue actions that tk polls every 100 ms).
- New `autostart.py` (per-user `HKCU\...\Run` value `NetworkConnector`, `--background`).
- New `notify.py` (AUMID `NetworkConnector.App` registration; toast → tray balloon → window fallback; strict
  `accept:|decline:|stop:<64-hex device id>` arguments; `ConsentRequests` with a 60 s timeout to Decline; messages
  show only name, short ID and thread count).
- New `radio.py` (Wi-Fi radio state; turn **on** only from the owner's click; never off).
- `app.py` rewritten: tray, close to tray, `--background`, Start at login switch, restart to change roles, offers
  `sandbox.setup_runtime()` for Give performance; `ControllerRuntime.wind_down()` announces closing first.
- `link_gui.py`: helper consent flow (auto-accept with message, Ask me with Accept/Decline, Trusted only), Stop from
  a message, in-app consent dialog, Wi-Fi radio row.
- `helper_policy.py`: `limit_reason()` and the `HelperPolicy` contract object (validated, maps to `worker.json`).
- `worker.py`: limits checked before claims **and** about once a second while jobs run; running CPU time counts
  against the daily limit; `limit_stop_now`; blocking always stops jobs; job label in "what is running";
  `cpu_rate_percent` (Job Object CPU-rate hard cap in `winlimits.py`/`sandbox.py`, job refused if it can't be set).
- `worker_gui.py`: accept mode, stop-at-once, CPU cap, "What is running" view, blocked count.
- `controller.py`/`store.py`: claim response carries the batch label. `cli.py`: `nc app open|autostart|reset`.
- `pyproject.toml`: `[app]` extra.
- Tests added: `test_stop_helping.py` (real TLS cluster: job process gone < 1 s, job released; limit reached
  mid-job), `test_notify.py`, `test_tray.py`, `test_autostart.py`, `test_radio.py`, `test_consent.py`,
  `test_helper_policy.py` additions, `test_app.py` rewrite.

**Phase 7 leftovers** (`1b9e5a1`)
- `scripts/add-firewall-rule.ps1 -Discovery [-LocalAddress]`, `remove-firewall-rule.ps1 -Discovery`, and an elevated
  "Allow discovery through firewall" button (`link_gui.discovery_firewall_command`, values validated).
- `wifidirect.py`: `open_group_ssid` (`DIRECT-NC-XXXX`), `derived_passphrase` (PBKDF2, app salt),
  `connected_wlan_ssids`, `scan_direct_groups` (WinRT), `join_open_group`, `AutoJoin` (≤ every 30 s; join only when
  on no Wi-Fi, otherwise offer). Open requesters start groups with derived credentials; closed ones stay random.
- `identity.py`: `cert_expires`, `expiry_warning` (90 days) shown in the log, `nc identity`, Devices tab, helper.
- Tests: firewall static checks (+ Windows-only harness), open-group credentials, auto-join rules, expiry warning.

**Phase 10 — result checks** (`c8cc5e4`)
- `store.py` migration step 4 (`excluded_workers`, `check_of`, `verifier`; `job_checks` state columns).
- Redundancy (`redundancy_percent`, default 10) for open-group wasm results: held as `checking` until a twin on a
  different helper matches (stdout + exit code); mismatch → third helper, majority wins; twins only when another
  helper is online; given up after 10 minutes.
- Reliability score (`devices.score`: +1 / −5 / −1); negative → one chunk at a time; `block_score` −10 → blocked and
  revoked.
- New `verify.py`: requester-side Wasm verifier in the requester's sandbox; rejected results go to another helper;
  fails closed. Submit option `verifier_sha256` (wasm only, module must exist, sandbox must be available).
- Helper-side reputation in `helper_usage.py`; "Block this requester" button.
- API/CLI/GUI: `/admin/checks`, `nc checks`, `nc policy --redundancy`, `--verifier`, Devices tab Score column.
- Tests: `test_verify.py` (a lying helper on a real TLS cluster is caught, outvoted and dropped; verifier paths).

**Phase 11** (`c8751f1`, `a2e184e`)
- `examples/wasm/bench` (Rust, std-only SHA-256) built for `wasm32-wasip1`; module shipped as
  `network_connector/data/wasm_bench.wasm`, pinned SHA-256 `245016c7…4257`; answers byte-identical to the native job.
- Benchmarks: `engines` native/wasm (`scenario@wasm`, Wasm overhead column), `topN` scenarios, device names and short
  IDs, optional shared speed score (`share_speed_score`, off by default); benchmark jobs are never held for checks.
- Rust primes example built once: imports `fd_read`, `fd_write`, `environ_get`, `environ_sizes_get`, `proc_exit`
  (all allowed); prints `78498` for `0 1000000` under wasmtime.
- Docs: README, new `docs/SETUP.md` + `setup-worker.ps1 -App`, JOINING, SECURITY, SANDBOX, CONFIGURATION,
  BENCHMARK, TROUBLESHOOTING, CHANGELOG 0.3.0, example READMEs, HANDOFF update.
- Security review `docs/SECURITY_REVIEW_0.3.0.md`. Fixed: limit-hit kill was process-wide (now only the
  requester's jobs); `Store.finish` now requires `status == running`; checks no longer stall a lone helper;
  benchmarks excluded from redundancy.
- `__version__` = `0.3.0`.

**Verified**
- Baseline before changes: 238 passed, 6 failed (3 need Windows; 3 were loopback aliases going through the proxy),
  43 skipped.
- Final full run (`NO_PROXY=127.0.0.1,127.0.0.2,127.0.0.3`, Xvfb): **286 passed, 4 failed, 49 skipped**. The 3
  `test_wifidirect.py` failures need Windows (same as before). `test_benchmark_two_real_workers_api_cli_exports`
  timed out once while pairing in setup; it passed in the previous full run and twice on its own afterwards.
- `compileall` passed.

**Not done / open**
- Optional `python.wasm` (PLAN §17.6).
- Nothing Windows-only was executed: escape suite, AppContainer/Job Object (incl. the CPU cap), PowerShell scripts,
  toasts and tray on screen, WinRT Wi-Fi/radio.
- Hardware tests (HANDOFF §2.3), PLAN §22.3 acceptance, then merge to `main` and tag — owner.
