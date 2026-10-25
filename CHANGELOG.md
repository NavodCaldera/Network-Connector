# Changelog

## 0.1.0

First release.

- Controller (HTTPS, LAN-only binding, SQLite state) and an outbound-only worker agent.
- One-time pairing codes bound to a pinned TLS certificate; per-worker revocable tokens; admin actions local-only.
- Job allowlist: prime_range, number_batch, monte_carlo_pi, cpu_burn, hash_files, image_resize, python_function.
- Leases, heartbeats, reconnect, timeouts, retries, cancellation, idempotent submits, graceful shutdown, restart recovery.
- Worker limits: CPU threads, jobs at once, memory per job (Windows Job Object), idle timeout, pause/disable, low priority.
- Controller and worker windows (Tkinter), `nc` command line, proof-of-work demo.
- Release-tag updater with self-test and automatic rollback.
