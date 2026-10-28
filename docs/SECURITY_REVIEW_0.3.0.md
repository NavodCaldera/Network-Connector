# Security review for 0.3.0

Date: 2026-10-28. Scope: AGENT.md §1 invariants 1–11, walked against the merged 0.3.0 code (phases 7–11).
Done in a Linux container: everything that runs on Linux was run; Windows-only layers (AppContainer, Job Object,
firewall scripts, toasts, WinRT) were reviewed by reading and with mocked tests only. The Windows checks at the end
**must still be run on the i7 and i3** before tagging v0.3.0.

## Invariants

| # | Invariant | Where it is enforced | Evidence |
|---|---|---|---|
| 1 | No TCP bind on `0.0.0.0`/`::` | `config.validate_bind_host`; `ControllerRuntime.add_listener` validates every host; discovery binds per interface (`discovery.py`) | `test_security_api.py`, `test_local_connection.py`; no new listener was added in phases 9–11 (grep for `bind(`) |
| 2 | `admin_hosts` = own addresses + loopback | `ControllerRuntime.__init__`/`add_listener`/`remove_listener` | `test_security_api.py::test_admin_*`, `test_wifidirect.py::test_dynamic_listener_keeps_certificate_and_admin_sources`. New `/admin/checks` uses `require_admin` |
| 3 | Certificates pinned, never regenerated | `ControllerRuntime` refuses a half-missing pair; `identity.install_server_cert` never overwrites | `test_identity.py`. Phase 7 leftover: expiry is now **warned** (`identity.expiry_warning`), never renewed silently |
| 4 | Codes in memory; `/join` signed; tokens hashed; secrets ACL-protected | `PairingManager`, `JoinManager`, `hash_token`, `save_config` → `protect_path` | `test_join.py`, `test_security_api.py`. The derived `DIRECT-NC-XXXX` passphrase is deliberately **not** a secret (SECURITY.md, "The Wi-Fi password is not the security boundary") |
| 5 | Helpers open no TCP listener; discovery socket receive-only | `discovery.Listener`; tray (pystray window messages), toasts (WinRT, foreground activation while running), radio and Wi-Fi scan open no sockets | Code review of `tray.py`, `notify.py`, `radio.py`, `wifidirect.scan_direct_groups`. **Windows check:** `Get-NetTCPConnection -State Listen` on the i3 |
| 6 | Endpoints admin-only / worker-auth / rate-limited join; validated, size-capped | New or changed: `/admin/checks` (admin), `PUT /admin/policy` (`redundancy_percent` 0–100), `/admin/jobs[/batch]` (`verifier_sha256` hex-64, wasm only, module must exist), claim response adds the requester's own batch label | `test_verify.py::test_submitting_with_a_verifier_needs_wasm_a_known_module_and_a_sandbox` |
| 7 | Others' code only as `wasm` in the full sandbox; no shell; validated netsh/PowerShell | Verifier runs through `sandbox.run` and fails closed (`verify.py`); `wasm_bench` is a pinned Wasm module run as a normal `wasm` job; no `shell=True` anywhere; firewall/autostart/discovery commands validate every value | `test_verify.py::test_verifier_that_cannot_run_fails_the_job_closed`, `test_firewall_scripts.py::test_gui_discovery_command_passes_only_validated_values`, `test_autostart.py`. The CPU-rate cap refuses the job if Windows won't set it |
| 8 | Every sandbox deleted on every path; start-up sweep | Unchanged `sandbox.Sandbox.run` `finally` + `sweep()`. New kill paths (limit reached, blocked, Stop) go through `Sandbox.kill` / `stop_all` | Escape suite `test_sandbox.py` (Windows only, skipped here) |
| 9 | Owner in control; Stop ≤ 1 s; limits enforced on the helper | `WorkerAgent.stop_helping`, `_check_limits_while_running`, `helper_policy.limit_reason`, Job Object CPU rate | `test_stop_helping.py` (real TLS cluster: process gone < 1 s, job released; limit reached mid-job stops it or lets it finish), `test_consent.py`, `test_helper_policy.py` |
| 10 | Stale-report checks on every result path | `Store.finish` now requires `status == 'running'` with matching worker and attempt (stricter: also covers `checking`/`verifying`); `Store.verified` requires `verifying` | `test_verify.py`, `test_benchmark.py::test_samples_prune_and_cancel_all_metrics` |
| 11 | Never log secrets or job contents | Messages carry name, short ID, threads; verifier logs job ID and verdict only; "what is running" shows label and module hash | grep of `log.*(token|passphrase|secret|stdout|input_b64)` finds nothing; `test_notify.py::test_message_text_holds_only_name_short_id_and_threads` |

## Findings fixed during the review

1. **Limit reached while running killed too much.** `_check_limits_while_running` called `sandbox.stop_all()`, which
   kills every sandbox in the process, not only this requester's jobs. It now kills only its own jobs (`_kill` per job).
2. **Result reports for held jobs.** With the new `checking`/`verifying` states, `Store.finish` accepted a second report
   for a job that was no longer `running`. The guard now requires `running`.
3. **Checks could stall a lone helper.** A twin is started only when another helper was seen in the last minute;
   unanswered twins are given up after 10 minutes (result kept, marked *unchecked*).
4. **Benchmarks distorted by checks.** Benchmark jobs (they set `eligible_workers`) are never held for redundancy;
   benchmarks compare their own answers.

## Accepted residual risks (documented in SECURITY.md)

- Sampled redundancy catches a lying helper with a probability; colluding helpers or many identities (Sybil) can
  agree on a wrong answer. Use Trusted only, 100 % redundancy or a verifier for important work.
- A helper's `info.sandbox` and `speed_units_per_s` are self-reported. Lying can only stall or skew a benchmark the
  requester can cancel; they never widen what runs.
- Anyone with the app can join an open `DIRECT-NC-XXXX` group (by design).

## Still to run on Windows (both PCs) before tagging

- [ ] `pytest` on the i7 and i3, including the escape suite (`tests/test_sandbox.py`, `tests/test_wasm_jobs.py`) and
      the Windows-only firewall harness tests.
- [ ] `nc sandbox setup`, `nc sandbox status --check`, `nc sandbox run examples\wasm\primes\primes.wasm --input-text "0 1000000"` → `78498`.
- [ ] A wasm job with `cpu_rate_percent` set: Task Manager shows the cap; with an impossible cap the job is refused.
- [ ] `Get-NetTCPConnection -State Listen` on a helper shows no Network Connector TCP listener while the app, tray and
      toasts run.
- [ ] No `NC-Sandbox-*` profiles after the acceptance run.
