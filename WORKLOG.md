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
