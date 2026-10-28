# Agents for Network Connector 0.2.0 and 0.3.0

Two implementation agents carry out [PLAN.md](PLAN.md). A **coordinator** (the main Claude session or
you) assigns phases, reviews, and merges. Agent definitions that Claude Code can launch are in
[.claude/agents/](.claude/agents/).

| Agent | File | Owns | Part 1 phases (0.2.0) | Part 2 phases (0.3.0) |
|---|---|---|---|---|
| **Link agent** | `.claude/agents/link-agent.md` | Connecting: Wi-Fi Direct, binding, discovery, device identity, one-click join, consent, the one app, tray and messages | 0, 1 (§5.1, §5.3), 2 | 6 (network spikes), 7, 9 (UI/tray/messages), 11 |
| **Compute agent** | `.claude/agents/perf-agent.md` | Running and measuring work: jobs, store, metrics, benchmark, **Wasm sandbox**, modules, quotas, result checks | 1 (§5.2), 3, 4 | 6 (sandbox spikes), 8, 9 (quota enforcement), 10, 11 |

The Compute agent was called the "Perf agent" in Part 1. Its file name stays `perf-agent.md`.

Phase 5 (Part 1) and Phase 11 (Part 2) are shared. Each agent writes the docs for its own features, and the coordinator runs
the acceptance lists (PLAN.md §9 and §22.3).

---

## 1. Rules for both agents

### Project facts to rely on

- Python ≥ 3.10, Windows only. Run everything with the project venv: `.\.venv\Scripts\python.exe`, `.\.venv\Scripts\nc.exe`.
- Tests: `.\.venv\Scripts\python.exe -m pytest -q`. They must pass before every hand-back.
- GUI is tkinter/ttk. Background work goes through `gui_common.Background`, **never** blocking the UI thread.
- Style: match the surrounding code. Short functions, type hints, f-strings, the same comment density,
  user-facing text in plain English like the existing docs.
- Part 2 UI words: **Requester** ("Ask for performance") = controller, **Helper** ("Give performance") = worker.
  The code keeps the old names.

### Security invariants (do not break; see PLAN.md §6, §15–§17 and docs/SECURITY.md)

1. The requester/controller never binds `0.0.0.0` or `::` for TCP. Several addresses means several explicit binds.
2. `admin_hosts` contains only the controller's **own** addresses and loopback. Never a peer or a subnet.
3. TLS certificates stay pinned. No CA, no hostname check. **Never regenerate the controller or device certificate**
   without a migration path, because every helper would have to rejoin.
4. **Closed groups:** pairing codes stay in memory, single use, with a 10-minute TTL and a 5-try lock, and fingerprint-bound HMAC proofs.
   **Open groups:** `/join` requires a valid signature by the helper's device key over a fresh challenge.
   In both cases tokens are stored only as hashes, and secret and identity files are ACL-protected through `security.protect_path`.
5. Helpers open **no TCP listening ports.** The only listener allowed on a helper is the **receive-only UDP
   discovery socket** (PLAN §15.2). It never replies, never runs anything, drops bad, old or unsigned packets, is rate
   limited, and can only cause a list entry or, by policy, an outbound TLS connection.
6. Every new endpoint is admin-only (local), worker-authenticated, or a rate-limited join endpoint, with
   Pydantic-validated and size-capped input. Error format `{"error":{code,message}}`.
7. **Code from other people runs only as `wasm` jobs, and only inside the full sandbox** (PLAN §17.2):
   - a separate process, in an AppContainer with no capabilities (no network, no user files);
   - a Job Object with a memory cap, `ActiveProcessLimit=1` and kill-on-close;
   - wasmtime with WASI stdin/stdout/stderr only (no pre-opened folders, no environment, no sockets), and memory, fuel/epoch and stack limits.

   **If any layer can't be set up, refuse the job. Never fall back to weaker isolation for open groups.**

   Native job kinds stay a closed allowlist and are refused from open groups. No shell commands, native code or file
   paths over the network. `subprocess` uses argument lists, never `shell=True`. Validate every value passed to
   `netsh` or PowerShell against a strict pattern.
8. **Every sandbox is deleted after its job** (Job Object killed, working folder and AppContainer profile deleted), on every
   path, including Stop, timeout and crash. A start-up sweep removes leftovers.
9. The helper owner is always in control. **Stop** kills all sandboxes within 1 s. Limits, hours, quotas and the
   block list are enforced on the helper itself, not only by the requester.
10. Stale-report protection by attempt number stays in place on every path that writes job results.
11. Never log passphrases, tokens, pairing codes, private keys, or job input/output contents.

### Work log (required)

- Every change to this project gets an entry in [WORKLOG.md](WORKLOG.md), **in the same commit** as the change
  (or the last commit of a series). Newest entry first.
- An entry says: date, who (person or agent), branch and commits; what was asked; what was done (files/features);
  decisions and who made them; what was verified, with the real results including failures; and what is not done
  or could not be checked, with the reason.
- Never put secrets, tokens, passphrases, pairing codes or job contents in the log.
- A hand-back or pull request is not complete until WORKLOG.md is updated.

### Working method

- Work on your own branch (PLAN.md §11 for Part 1; for Part 2 `feat/identity-join`, `feat/sandbox`,
  `feat/app-tray`, `feat/verify`), in small commits. Don't push or merge, because the coordinator merges.
- Stay inside your ownership areas (§4). If you need a change in the other agent's area, write it down
  under **Requests** in your hand-back instead of making it.
- Add or extend tests for every behaviour change. Hardware-only behaviour gets a manual checklist entry
  in the docs plus mocked unit tests.
- Never kill or restart a controller or worker that the user started. Use test clusters (`tests/conftest.py`
  `Cluster`) or a separate `NC_HOME` under a temp folder for manual runs.
- If a plan step turns out to be wrong (for example a Phase 0/6 spike shows an API doesn't work), stop and report
  with evidence instead of working around the plan silently. **Never weaken a security invariant to make something work.**

### Hand-back format (each agent's final message)

```
## Done
- <change> — <files> — <test that covers it>
## Verified
- pytest: <N passed / failures pasted>
- manual: <what was run, output excerpt>
## Security invariants checked
- <numbers from §1, and how>
## Not done / deviations from PLAN.md
## Requests to the other agent / coordinator
## Docs updated
## WORKLOG.md entry added (required)
```

---

## 2. Link agent

### Part 1 mission (0.2.0)

The i7 and i3 connect over Wi-Fi Direct (or the Mobile Hotspot fallback) without a router, safely, from the GUI,
with the worker finding the controller again by itself.

**Part 1 tasks, in order:**

1. **Phase 0 spike** (PLAN §4). Write the scripts in `spikes/` (not shipped). Record the answers to every question in
   `spikes/REPORT.md`, with exact command output, and make the **mode A/B decision**.
   - The publisher test and `netsh wlan connect` need the user at the machines. Prepare one script
     per machine and ask the coordinator to have the user run them, rather than guessing the results.
2. **§5.1** Attach the GUI to a running controller. Clearer "port in use" message with the PID.
3. **§5.3** The local worker and the admin client go through `127.0.0.1`.
4. **§6.2** `bind_hosts`, one uvicorn server per address, always loopback, `add_listener`/`remove_listener`.
5. **§6.1** `wifidirect.py` (`DirectGroup`, `join`, `find_direct_interface`, `candidate_controllers`, `probe`)
   and the optional `wifidirect` extra in `pyproject.toml`.
6. **§6.3** `controller_fingerprint` and `known_hosts`. The fingerprint-checked re-find in the worker loop, and "Find controller".
7. **§6.4** The firewall rule for the direct link, with `-WiFiDirect` in the add/remove/setup scripts and the elevated GUI button.
8. **§6.5** The Wi-Fi Direct tab, the Pair tab text, the worker "0. Connect" section, the CLI `nc wifidirect …`,
   `nc worker join|find`, and the Dashboard `link` column. The controller works out the link per worker from the heartbeat source IP.
9. **§6.6** A clear updater error when offline.
10. **§6.7** Tests. **§9** `docs/WIFI_DIRECT.md`, plus the SECURITY, CONFIGURATION, SETUP_I7/I3 and TROUBLESHOOTING updates.

**Part 1 done when:**
- PLAN §9 items 1–5, 10 and 12 pass on the hardware (the coordinator runs them with the user).
- e2e: a worker re-finds the controller on a second loopback alias, and an impostor cert is refused.
- `pytest` is green and no security invariant is touched.

### Part 2 mission (0.3.0)

Any PC can find nearby requesters and help with **one click or none, with no pairing codes**. The owner stays in control
through clear messages, Accept/Decline when they want it, and an instant Stop.

**Part 2 tasks, in order:**

1. **Phase 6 network spikes** (PLAN §21), results in `spikes/REPORT-0.3.md`:
   - multicast and broadcast announcements on a home router, the Wi-Fi Direct group and the hotspot;
   - auto-joining `DIRECT-NC-*` while already on another Wi-Fi network (concurrency);
   - `pystray` and Windows toast notifications from desktop Python;
   - asking Windows to turn the Wi-Fi radio on (`Windows.Devices.Radios`).
2. **§14 Device identity:** `identity.py` (key, cert, device ID, short ID, friendly name), migrating the existing
   controller cert, and the `devices` table **requests** to the Compute agent (store owner).
3. **§15 Discovery:** `discovery.py` with the signed announcer (requester) and the receive-only listener (helper):
   strict parsing, size, age and signature checks, a rate limit, and the nearby list model.
4. **§16.1 Join protocol:** `/join/hello` and `/join` routes, the helper-side join client, the requester join policy
   (Anyone / Trusted only / Ask me / Require code), leave and closing, and the trusted and blocked lists.
5. **§14 Verify:** numeric comparison with commit-then-reveal nonces, and the Trust button.
6. **§15.3** No-router flow: the requester starts the group from "Ask for performance", the SSID is `DIRECT-NC-<short id>`, and the
   passphrase is derived from it. The helper's "Join nearby groups automatically" follows the PLAN rules.
7. **§13 + §16.2 + Phase 9 UI:** `app.py` (role buttons; the Part 1 windows become tabs), `tray.py` (status,
   Pause, Stop, Open, Quit), autostart for the current user, the auto-accept message and Ask-me Accept/Decline, the helper settings
   panel, and the "what is running" view. **Stop** calls the Compute agent's `sandbox.stop_all()`.
8. **Tests:** PLAN §22.2, plus `test_identity.py`, `test_discovery.py` and `test_join.py`.
9. **Docs:** a new `docs/JOINING.md` and updates to SECURITY (open groups, why the Wi-Fi password is not the boundary,
   announcement spoofing, consent), README and SETUP.

**Part 2 done when:**
- PLAN §22.3 items 1–3, 5 (the UI side), 8 and 12 pass on the hardware.
- A helper joins an open group on a LAN with no code, a blocked device is refused, and the helper has no TCP listener.

---

## 3. Compute agent (`perf-agent`)

### Part 1 mission (0.2.0)

The same fixed work can be run as *each machine alone* and *all together*. The numbers are measured
honestly and stored, and both computers can see the comparison.

**Part 1 tasks, in order:**

1. **§5.2** Cancel all: store, endpoint, client, `nc job cancel-all`, and the Jobs-tab button.
2. **§7.1** `meta.schema_version` + `_migrate()`. Back up `state.db` before the first migration. Test that a
   0.1.0 database still opens and keeps its rows.
3. **§7.2** Per-job `cpu_seconds`/`peak_mem_mb` (jobrunner `METRICS` line, Job Object peak), attempt durations,
   and `release` keeping its attempt row. Heartbeat `rtt_ms`, power and frequency.
4. **§7.3** The `worker_samples` table with 24 h pruning, and `Store.samples()`.
5. **§8.1** The `benchmark` job kind (`hash_chain`, `prime_sieve`), checked per chunk and in total, the `validate_total` hook,
   and the calibration helper.
6. **§8.2–8.3** `benchmark.py` `BenchmarkRunner` (settle, snapshot, submit, wait, collect, check, derive),
   the `benchmarks` and `benchmark_runs` tables, and the cancel and interrupted states.
7. **§8.4** The admin benchmark API and the worker read-only `/worker/benchmarks/latest`, plus the `AdminClient`/worker client methods.
8. **§8.5** CLI `nc bench run|list|show|export|cancel` and `nc worker perf`.
9. **§8.6** The controller Benchmark tab (inputs, live view, table, canvas charts, history, export) and the worker
   "Performance" section. Self-contained HTML export.
10. **§8.7** Record and show the fairness data. **§8.8** Tests. **§9** `docs/BENCHMARK.md`, plus the README and FIRST_JOB updates.

**Part 1 done when:**
- PLAN §9 items 6–9 pass on the hardware.
- On a `Cluster` with 2 workers: the scenarios each + all finish, the answers are identical, speedup and efficiency are computed,
  cancel works mid-run, and a restart marks the benchmark `interrupted`.
- `pytest` is green. Benchmark numbers come from the store's timestamps, not client-side timers.

### Part 2 mission (0.3.0)

Other people's code runs **only** inside a WebAssembly sandbox that can't reach the owner's network, files or
devices. Each sandbox is created for one job and completely deleted afterwards. Results are checked, so lying
helpers are caught.

**Part 2 tasks, in order:**

1. **Phase 6 sandbox spikes** (PLAN §17.5), results in `spikes/REPORT-0.3.md`:
   - wasmtime-py inside a no-capability AppContainer (what ACLs or DLL paths it needs);
   - start-up time per job;
   - fuel vs epoch time limits;
   - Wasm vs native speed for `hash_chain`.
   - **Go/no-go:** if Python + wasmtime can't run in an AppContainer, report the fallback options. Don't build on a weaker layer.
2. **§17.2 Sandbox layers:**
   - `sandbox.py` (AppContainer profile create/delete through ctypes, launch with `SECURITY_CAPABILITIES`, the Job
     Object with memory cap and `ActiveProcessLimit=1`, `stop_all()`, the start-up sweep);
   - `sandbox_runner.py` (wasmtime engine, WASI with stdin/stdout/stderr only, import allowlist, memory, fuel/epoch and
     stack limits, progress over a pipe).
3. **§17.3** Per-job create and delete on every path, and the test that leaves 0 leftovers after 100 jobs.
4. **§17.1 + §17.4** The `wasm` job kind (validation, size caps), the `modules` table, `/admin/modules`, the hash-checked
   `/worker/modules/{sha256}` download, the helper LRU cache, and **refusing native kinds in open groups**.
5. **§16.2 enforcement (Phase 9):** plugged-in only, idle only, hours, the per-requester daily CPU-minute limit
   (`helper_usage`), and the per-requester block, all checked on the helper before claiming and while running.
6. **§18 Result checks:** redundancy % with a twin job on a different helper (`job_checks`), majority on a mismatch, the
   reliability score in `devices.score`, auto-block, and the requester-side Wasm verifier hook.
7. **§19:** `wasm_bench` (Rust source in `examples/wasm/bench/`, plus a prebuilt module with a pinned hash), the "Wasm
   overhead" column, many-helper scenarios, and the optional shared speed score.
8. **§17.6 (optional):** a pinned `python.wasm` for Python scripts in the sandbox.
9. **Examples:** `examples/wasm/primes/` (Rust, stdin → stdout) with build instructions and an `nc job submit wasm` example.
10. **Tests:** the PLAN §22.1 escape suite (including the native "already escaped" layer tests), plus `test_sandbox.py`,
    `test_wasm_jobs.py` and `test_verify.py`. **Docs:** a new `docs/SANDBOX.md` (what is isolated, honest limits, how to write a
    job) and the SECURITY threat rows for the sandbox.

**Part 2 done when:**
- PLAN §22.3 items 4–7, 9–11 pass on the hardware.
- The escape suite passes. No sandbox leftovers remain after 100 jobs or a simulated crash. A lying test helper is dropped.

---

## 4. Ownership of shared files

To keep merge conflicts small, each agent edits only its own areas. Add new code in **new functions or
sections**, not by rewriting existing ones.

| File | Link agent | Compute agent |
|---|---|---|
| `config.py` | `bind_hosts`, `list_private_ips`, `detect_lan_ip`, `WorkerConfig.controller_fingerprint/known_hosts`, join policy and helper connect settings | helper limit/quota settings (plugged in, idle, hours, daily limit, redundancy %) |
| `controller.py` | `ControllerRuntime` start/stop/listeners, `admin_hosts`, heartbeat `link`, `/join/hello`, `/join`, `/worker/leave`, `closing` | cancel-all, benchmarks, `/worker/benchmarks/latest`, metric fields, samples, `/admin/modules`, `/worker/modules/{sha256}`, `wasm` validation, open-group kind filter, redundancy scheduling |
| `store.py` | — (request tables from the Compute agent) | everything, including `devices`, `modules`, `job_checks`, `helper_usage` |
| `client.py` | `pair_worker`, `fetch_server_cert`, `probe`, `admin_client_from_disk`, the join client | new `AdminClient` methods, worker perf and module fetch |
| `worker.py` | reconnect/re-find, `controller_host` switching, joining several requesters, leave | per-job metrics, running `wasm` jobs through `sandbox`, quota checks before claiming |
| `jobs.py`, `jobrunner.py`, `winlimits.py`, `sysinfo.py`, `benchmark.py`, `demo.py`, `sandbox.py`, `sandbox_runner.py`, `modules.py`, `verify.py` | — | everything |
| `wifidirect.py`, `localworker.py`, `updater.py`, `identity.py`, `discovery.py`, `app.py`, `tray.py`, `notify.py`, `scripts/*.ps1` | everything | — |
| `cli.py` | `controller`, `wifidirect`, `worker join/find/pair`, `identity`, `nearby`, `trust/block` groups | `job cancel-all`, `bench`, `worker perf`, `module upload/list`, `job submit wasm` |
| `gui.py`, `link_gui.py` | top bar/attach, Settings network frame, Wi-Fi Direct tab, Pair tab, Dashboard `link` column, join policy panel, nearby/devices views | Jobs-tab Cancel all, Benchmark tab, Demo tab text, modules view, result-check view |
| `worker_gui.py` | "0. Connect" section, address field, nearby list, Accept/Decline, Stop | "Performance" section, helper limits/quotas panel, "what is running" details (module hash, usage today) |
| `benchmark_gui.py` | — | everything |
| `pyproject.toml` | the `wifidirect` and tray/notification extras | the `wasmtime` dependency (pinned) |
| `tests/` | `test_wifidirect.py`, `test_identity.py`, `test_discovery.py`, `test_join.py`, bind/re-find cases | `test_benchmark*.py`, `test_store.py` migrations, `test_sandbox.py`, `test_wasm_jobs.py`, `test_verify.py`, escape suite, metrics cases |
| `docs/` | WIFI_DIRECT, JOINING, SECURITY (network/join rows), CONFIGURATION, SETUP_I7/I3, TROUBLESHOOTING | BENCHMARK, SANDBOX, SECURITY (sandbox rows), FIRST_JOB, README features |
| `examples/` | — | `examples/wasm/*` |

**Shared contract**, agreed up front so the two agents can work in parallel:

- **Part 1:**
  - The worker heartbeat `metrics` keys added by the Compute agent: `rtt_ms`, `on_ac_power`, `battery_percent`, `cpu_freq_mhz`.
  - The worker row field added by the Link agent: `link: "lan" | "wifi-direct" | "loopback"` (in `/admin/workers` output).
    The Compute agent reads it into benchmark settings snapshots, and treats it as `"unknown"` until it exists.
- **Part 2:**
  - `identity.current() -> Identity{device_id, short_id, name, cert_pem, sign(bytes), key_path}` (Link). Used by the
    Compute agent for module and verify records.
  - The `devices` table schema in PLAN §20 (owned by Compute). The Link agent calls
    `Store.upsert_device / set_trusted / set_blocked / get_device`.
  - `sandbox.stop_all() -> int` and `sandbox.available() -> (bool, reason)` (Compute). The Link agent calls them from
    Stop, the tray and the "Give performance" start-up check.
  - `group_mode: "closed" | "open"` per requester (Link sets it from the join policy). The Compute agent enforces "open → `wasm`
    only".
  - The helper policy object `HelperPolicy{accept, threads, jobs, mem_mb, ac_only, idle_min, hours, daily_cpu_min,
    blocked}` (UI by Link, enforcement by Compute), stored in `worker.json`.
- `CHANGELOG.md` entries: each agent adds lines under the right version in its own subsection
  (`### Wi-Fi Direct`, `### Benchmark`, `### Joining`, `### Sandbox`).

## 5. How to launch them

From Claude Code in this folder:

```
Use the link-agent to do Phase 6 (network spikes) of PLAN.md.
Use the perf-agent to do Phase 6 (sandbox spikes) of PLAN.md.
```

Then, after the spike reports are reviewed:

```
Use the link-agent to do Phase 7 of PLAN.md.
Use the perf-agent to do Phase 8 of PLAN.md.
```

The coordinator reviews each hand-back, runs `pytest`, merges, and then starts the next phase.
