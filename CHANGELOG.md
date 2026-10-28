# Changelog

## 0.3.0

Not yet accepted on real hardware: run PLAN §22.3 (and docs/JOINING.md, docs/SANDBOX.md checklists) before tagging.

### Upgrade notes

- A `controller.json` written by 0.2.0 or earlier becomes join policy **Require pairing code** with announcements off,
  so an existing closed i7 + i3 group doesn't open by itself. Choose another policy in the Devices tab to open it.
- A controller certificate from 0.2.0 (2-year lifetime) becomes the device identity. A warning appears 90 days before
  it expires; renewing means helpers pair or join again.
- The state database gains schema step 4 (result checks). `state.db` is backed up first.

### App, tray and messages

- `network-connector` opens one window with **Give performance**, **Ask for performance** and **Both**; the Part 1
  windows are its tabs (`nc gui` and `nc worker-gui` still work). Changing roles restarts the app after stopping work.
- Tray icon (pystray): status, Pause, Stop helping, Open, Quit; closing the window keeps the app in the tray.
- Starts in the tray at login for this user (HKCU Run), with a switch to turn it off. `nc app open|autostart|reset`.
- Owner messages through Windows toasts (`NetworkConnector.App`), falling back to the tray balloon and the window:
  auto-accept tells the owner who is using how many threads, with **Stop**; **Ask me each time** shows
  Accept / Decline and declines after 60 s. Only names, short IDs and thread counts are shown.
- Choosing Give performance offers the sandbox setup. "Wi-Fi is off — Turn on" asks Windows to turn Wi-Fi on, only on
  that click.
- New optional extra `[app]` (pystray, WinRT notifications, radios, Wi-Fi and connectivity).

### Joining

- Helpers join open requesters by themselves (auto-accept) or after Accept, only with a working sandbox.
- Open requesters start Wi-Fi Direct groups as `DIRECT-NC-XXXX` with a passphrase derived from the name; helpers can
  join them by themselves when on no other Wi-Fi (off by default), otherwise one click "Join (disconnects …)".
- `add-firewall-rule.ps1 -Discovery`: UDP 8766 for one python.exe, Private plus the direct-link subnet only; a button
  in the Nearby box adds it with administrator approval.
- Certificate expiry warnings in `nc identity`, the log, the Devices tab and the helper.

### Helper limits

- Limits are enforced on the helper before every claim and while jobs run: plugged in, idle, hours, daily CPU minutes
  per requester (running jobs count), blocked requesters (stop at once), and a new Job Object CPU-rate cap.
  Optionally stop running jobs as soon as a limit is reached. `HelperPolicy` object in `helper_policy.py`.
- **Stop** kills every job within 1 s and leaves an open group (tested on a real cluster).
- "What is running": requester name and short ID, batch label, module hash, CPU and RAM now, CPU minutes today, why
  no new work is taken, and how often this requester's jobs had to be killed. **Block this requester** in one click.

### Result checks

- Redundancy for open groups (default 10 %, `nc policy --redundancy`, Devices tab): results are held until a twin on a
  different helper matches byte for byte; a mismatch goes to a third helper and the majority wins.
- Reliability score per helper device (Score column): wrong results −5, lost jobs −1, confirmed +1; negative scores get
  one chunk at a time; at −10 the device is blocked and its helpers revoked.
- Optional requester-side Wasm verifier (`nc job submit wasm --verifier <sha256>`), run in the requester's sandbox.
- `/admin/checks`, `nc checks`. Schema step 4.

### Benchmark

- `wasm_bench`: the `hash_chain` workload in Rust compiled to Wasm (`examples/wasm/bench`), shipped pinned by SHA-256.
  `--engines native,wasm` adds `scenario@wasm` rows and a **Wasm overhead** column; both engines must give the same
  digest.
- `topN` scenarios (the N fastest by calibration); participants shown by device name and short ID.
- Helpers may share a locally measured speed score (off by default); requesters then skip calibration.
- The Rust `examples/wasm/primes` was built once: its imports are all on the sandbox allowlist.

### Identity, discovery and the sandbox (phases 7 and 8)

#### Joining

- Device identity: one EC P-256 key and certificate per install (device ID, short ID, name), ACL-protected. An
  existing controller certificate becomes the device identity, so paired workers don't pair again.
- Signed nearby announcements and a receive-only, per-interface, rate-limited listener; `nc nearby`; an outbound
  probe of the direct-link gateway when announcements can't arrive.
- `/join/hello` and `/join`: helpers join open groups with a device-key signature, no code. Join policies Anyone
  nearby / Trusted only / Ask me / Require pairing code (upgraded controllers keep requiring codes). Trusted and
  blocked devices, `/worker/leave`, and `closing` in the heartbeat.
- Optional Verify (6-digit numeric comparison, commit-then-reveal) before marking a device Trusted.
- Minimal UI: Nearby list with Join/Trust/Block/Verify/Leave in the worker window, Devices tab in the controller
  window. CLI `nc identity`, `nc policy`, `nc devices`, `nc trust|block`, `nc worker connect|leave|verify`.
- Store: numbered schema migrations; step 2 adds the `devices` table.

#### Sandbox

- Added the Wasm sandbox for helpers: each `wasm` job runs in a fresh no-capability
  AppContainer inside a Job Object (memory cap, one process, kill-on-close) with
  wasmtime limited to WASI stdin/stdout/stderr, and is deleted afterwards on every
  path. A start-up sweep removes leftovers; `sandbox.stop_all()` stops everything
  within a second. `nc sandbox setup|status|sweep|run`; see docs/SANDBOX.md.
- Added the `wasm` job kind, module upload and listing (`/admin/modules`,
  `nc module upload|list`, `nc job submit wasm`), hash-checked module download
  for helpers running a job that uses it, and a 200 MB helper module cache.
- Open groups hand out and accept `wasm` jobs only. Helpers without a working
  sandbox don't take `wasm` jobs and refuse them with the reason.
- Database migrations are now numbered steps; step 3 adds `modules`,
  `job_checks` and `helper_usage`. `state.db` is backed up first.
- New pinned dependency: `wasmtime==50.0.1`. Example: `examples/wasm/primes`.

## 0.2.0 (in development)

### Wi-Fi Direct

- Added controller-owned Wi-Fi Direct publishing, Mobile Hotspot detection, worker
  join/discovery and fingerprint-checked reconnection. Actual adapter acceptance
  remains pending tests on the real i7 and i3.
- Added dynamic explicit listeners, direct-link status, GUI/CLI controls and a narrow
  firewall rule for Public/Unidentified direct networks.
- The controller GUI can attach to a running controller. Local admin and local
  worker traffic use an explicit loopback listener without changing pairing.

### Benchmark

- Added cancel-all, versioned database migration with a backup, job CPU/memory
  measurements, attempt durations and persisted heartbeat samples.
- Added fixed-work benchmarks, controller orchestration, history, authenticated
  APIs, CLI commands, controller controls and shared worker results.
- Added JSON, CSV and self-contained HTML reports.

## 0.1.0

First release.

- Controller (HTTPS, LAN-only binding, SQLite state) and an outbound-only worker agent.
- One-time pairing codes bound to a pinned TLS certificate; per-worker revocable tokens; admin actions local-only.
- Job allowlist: prime_range, number_batch, monte_carlo_pi, cpu_burn, hash_files, image_resize, python_function.
- Leases, heartbeats, reconnect, timeouts, retries, cancellation, idempotent submits, graceful shutdown, restart recovery.
- Worker limits: CPU threads, jobs at once, memory per job (Windows Job Object), idle timeout, pause/disable, low priority.
- Controller and worker windows (Tkinter), `nc` command line, proof-of-work demo.
- Release-tag updater with self-test and automatic rollback.
