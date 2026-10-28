# Changelog

## 0.3.0 (in development)

### Joining

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

### Sandbox

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
