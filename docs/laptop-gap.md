# Laptop/desktop sharing: gap audit (task 1.L1)

_Written 2026-11-02 against `main` at cfec5c7. Owner priority: Windows/Linux PC sharing must work fully end to end
before any mobile work. Target: the Phase 1 exit test (PLAN.md §8): jobs complete repeatedly through worker crashes,
disconnects and tariff boundary changes, without ever breaking an owner limit._

The automated check for this is [`infra/fleet-e2e.ps1`](../infra/fleet-e2e.ps1). Its scenarios that cannot work yet
are reported as `EXPECTED-FAIL` with the gap id from this file (X1 = G1/G2, X2 = G3, X3 = G6, X4 = G7/G8).

## What already works on one Windows PC (fleet-e2e S1-S5)

Coordinator + Postgres in Docker; two `nc-agent` helper daemons and the `nc` CLI requester on one PC, all over
loopback HTTP to the coordinator. Signed job manifests and module signatures (publisher = requester key, trusted by
the helper owner's policy), Range-resumable tariff-gated downloads from coordinator blob staging, AppContainer + Job
Object sandbox, journal-based crash recovery, lease renewal, attempt-checked idempotent commit, and two-replica
result comparison in the coordinator. Real results of the run are in WORKLOG.md (1.L1 entry).

What the fleet run does **not** prove yet: S5 closes the window while helper B is idle or between jobs, so the
"window closes in the middle of a job" path (G4) is only covered by the simulator and fake-coordinator tests;
S3's 20 s pause is shorter than the 60 s lease TTL, so no lease expires during it; everything runs on one PC over
loopback, so nothing here measures LAN or internet links. Helper B does not log why it is paused (activity is only
visible through `nc status`), which made the boundary hard to observe from logs.

Fixed in 1.L1 (Core): `requester::wait` (used by `nc submit`) aborted on the first transient coordinator error, so
any disconnect lost the requester's result; it now retries until its timeout. `nc submit --replicas 2` exposes
two-helper validation, which the CLI could not ask for before.

## Gaps, in suggested order

Owner: **core** = Core agent, **net** = Net agent, **app** = App agent. Each task is meant to be small and mergeable
on its own; where a gap needs work in two areas it is split.

| Order | Task | Gap | Owner | Files |
|---|---|---|---|---|
| 1 | **1.L2** | **G4** tariff refusals burn attempts | net | `server/crates/nc-coordinator/src/jobs.rs` (`fail`), migration |
| 2 | **1.L3** | **G4** park results across a closed window | core | `core/crates/nc-agent/src/session.rs`, `journal.rs` |
| 3 | **1.L4** | **G3** dispute → third helper | net | `server/crates/nc-coordinator/src/jobs.rs`, migration, `proto/` request to Core if a state is added |
| 4 | **1.L5** | **G5** resumable chunked upload, server side | net | `server/crates/nc-coordinator/src/blobs.rs`, `proto/` (via Core) |
| 5 | **1.L6** | **G5** resumable chunked upload, agent side | core | `core/crates/nc-agent/src/{coord,transfer,requester,session}.rs` |
| 6 | **1.L7** | **G9** CLI requester: detached submit, `nc job status/wait/fetch` | core | `core/crates/nc-cli/src/main.rs`, `nc-agent/src/requester.rs` |
| 7 | **1.L8** | **G8a** Linux sandbox: namespaces + no network | core | `core/crates/nc-exec/src/sandbox/linux.rs` (new) |
| 8 | **1.L9** | **G8b** Linux sandbox: seccomp filter | core | same |
| 9 | **1.L10** | **G8c** Linux sandbox: cgroup v2 memory/pids/cpu + teardown sweep | core | same |
| 10 | **1.L11** | **G8d** Linux headless daemon: systemd unit, SIGTERM → Stop, paths | core (daemon) + app (unit file, .deb) | `core/crates/nc-agent/src/main.rs`, `packaging/linux/` |
| 11 | **1.L12** | **G7a** Windows service host for nc-agent | core | `core/crates/nc-agent/src/main.rs` (+ `service.rs`) |
| 12 | **1.L13** | **G7b** Windows installer (MSI/MSIX), worker folder ACL, Authenticode | app | `packaging/windows/`, `.github/workflows/` |
| 13 | **1.L14** | **G1a** proto: peer hints + relay token in the lease | core (proto) then net (server) | `proto/`, `server/crates/nc-coordinator` |
| 14 | **1.L15** | **G1b** nc-agent runs an nc-net node; requester serves blobs to its leased helper | core | `core/crates/nc-agent` (+ dep on `nc-net`) |
| 15 | **1.L16** | **G2** relay fallback + coordinator-staging fallback, byte accounting direct vs relayed | core + net | `nc-agent`, `nc-net`, `server/crates/nc-relay` |
| 16 | **1.L17** | **G1c** LAN-only mode (embedded coordinator, paired devices) | core + net | `nc-agent`, `nc-net` pairing |
| 17 | **1.L18** | **G6** requester flow in the Flutter app | app | `app/lib/`, `core/crates/nc-bridge` |
| 18 | **1.L19** | **G10** publisher keys: coordinator-certified publishers, revocation | core + net | `nc-policy`, `nc-identity`, server |

Why this order: G4 and G3 can make the exit test fail today (a job can be lost by tariff boundaries; a dispute is
terminal), so they come first. G5 and G9 make transfers and the requester survive disconnects. G8/G7 put the agent
on Linux and run it unattended. G1/G2 (P2P) is the biggest piece and the platform already works without it through
coordinator staging, so it comes after the fleet is reliable. G6 (app requester) and G10 are last for the PC MVP.

## Gap details

### G1 LAN and internet transport: nc-net is not wired into nc-agent

- `core/crates/nc-agent/Cargo.toml` has no `nc-net` dependency (fleet-e2e X1 checks this with `cargo tree`).
  All traffic is HTTPS to the coordinator; job payloads go through coordinator blob staging
  (`server/crates/nc-coordinator/src/blobs.rs`, `/v1/blobs/{sha}`), so "LAN" today only means "same coordinator".
- `core/crates/nc-net` has the pieces (libp2p QUIC/TCP + Noise/TLS, mDNS, QR/SAS pairing, `TrustStore`, connection
  ladder direct → DCUtR → relay, `Node::open_stream` keyed by `PeerId`), but nothing on the agent side uses them.
- No proto field tells a helper where to fetch from (peer id, addresses, relay token). `LeaseResponse.transfers`
  are coordinator URLs only.
- No LAN-only mode (embedded coordinator, PLAN.md §3) exists.
- Tasks: 1.L14 (proto + server issue peer hints and a short-lived relay token with the lease), 1.L15 (agent node,
  fetch inputs from the requester's node over an authenticated stream, verified per chunk exactly as the HTTP path),
  1.L17 (LAN-only).

### G2 Direct transfer with relay fallback

- Depends on G1. `nc-relay` (token + quota) exists in `server/`, the coordinator mints relay tokens
  (`RELAY_TOKEN_TTL_SECS`), but no device uses them. The fallback order PLAN.md §2 wants is direct → relay →
  coordinator staging; today only the last exists. Tariff accounting must count relay overhead at both ends
  (PLAN.md §6). Phase 0.5 two-ISP measurement is still pending (TASKS.md).
- Task: 1.L16.

### G3 Two-helper validation: comparison yes, resolution no

- Enforced today (`server/crates/nc-coordinator/src/jobs.rs` `lease`/`complete`): `validation_count` (1 or 2) from
  the signed manifest; one result per device per task (`task_results UNIQUE(task_id, device_id)` and the lease
  query skips devices that already answered); a task commits only when `validation_count` results exist and all
  have the same sha256 and size; a mismatch sets the task to `disputed`.
- Missing: `disputed` is terminal (no third helper or trusted verifier, PLAN.md §5); "independently chosen" is not
  enforced (both replicas may be devices of the same owner/account; Phase 0 has one account, `PHASE0_ACCOUNT`);
  no per-helper correctness record; no higher replication for new helpers. The requester gets
  `RequesterError::Ended("disputed")`.
- fleet-e2e S4 checks the happy path with two real helpers; X2 reports the missing resolution.
- Task: 1.L4 (on mismatch queue one more replica on a third device, commit on 2-of-3 agreement, keep `disputed`
  only when there is no majority). Distinct-owner rule follows real accounts.

### G4 Night-window transfers: tariff boundaries burn attempts and discard work

- Downloads are tariff-gated and Range-resumable (`nc-agent/src/transfer.rs`), and the run loop stops even control
  traffic outside the window when the profile says `block`. That part holds the owner's limit.
- But when a window closes during a job, the helper fails the lease (`TARIFF_WINDOW` / `UPLOAD_WINDOW` in
  `session.rs`) or, if control traffic is blocked too, cannot renew and loses it. The computed result is dropped
  and the coordinator counts the attempt (`fail` and `lease` both cap at 6 attempts in `jobs.rs`), so a job whose
  helpers keep crossing window boundaries ends `failed` with no helper at fault.
- PLAN.md §6 wants stages scheduled separately (download at night, compute, upload in the next cheap window).
  There is no "parked" state: the journal has `Uploaded`, but nothing keeps a finished result and its lease across
  a closed window, and the coordinator lease cannot outlive `LEASE_MAX_TOTAL_SECS` (6 h).
- Tasks: 1.L2 (net: a `TARIFF_WINDOW` fail does not count toward the attempt cap; optional "release" without
  penalty), 1.L3 (core: keep a finished output on disk when the upload window is closed, release the lease cleanly,
  and either upload later under a new lease for the same task or let another helper take it; never upload outside
  the window).
- Scheduler-side tariff awareness (rank by open windows, reject transfers that cannot finish before the boundary)
  is not in the coordinator at all; the device-side guard is the only protection. Listed for after 1.L3.

### G5 Uploads are not resumable

- `Coordinator::put_blob` sends one whole body (`coord.rs`); outputs and requester inputs (≤ 16 MiB each,
  `MAX_TRANSFER`) restart from zero after a drop. Downloads already resume with `Range`.
- Tasks: 1.L5 (server: chunked/offset upload with per-chunk hash, size cap, owner check), 1.L6 (agent + requester).

### G6 Requester flow from the app

- The CLI has `nc submit <module> <input> [--replicas 2]` (blocking, one job per process). `Agent::submit_job`
  exists in `nc-agent/src/lib.rs`, but `nc-bridge` and `app/lib` have no submit, progress or download screens
  (fleet-e2e X3 checks for it).
- Task: 1.L18 (bridge `submit_job`/`job_status`/`fetch_output`, Requester screen with file pickers, progress,
  result save). Job payloads stay out of Flutter (AGENT.md §3): the bridge takes file paths, not bytes.

### G7 Windows background service and installer

- `nc-agent run --config` is a console process; there is no Windows service host (SCM start/stop, Stop → session
  stop within 1 s), no installer, no per-machine data dir with the right ACLs, and the AppContainer-readable worker
  folder is set up by hand (fleet-e2e does it with `icacls`). The app's `app/windows` runner builds the Flutter
  shell only. Desktop updates must be signed (AGENT.md §1 rule 9).
- Tasks: 1.L12 (core: service host in nc-agent), 1.L13 (app: MSI/MSIX that installs nc-agent + worker into a
  read-only folder granted to ALL APPLICATION PACKAGES, registers the service, Authenticode-signed).

### G8 Linux headless daemon

- `core/crates/nc-exec/src/sandbox/unsupported.rs` is used on every non-Windows OS: nc-exec refuses every job there
  (correct per rule 3, never weaker isolation), so **a Linux or Raspberry Pi helper cannot run any job today**.
- No systemd unit, no SIGTERM handling (Ctrl-C/SIGTERM kills the process without a clean Stop; the journal
  recovers, but the sandbox teardown path is the Job Object's on Windows only), no `/var/lib` layout.
- `SystemProbe` reads power only on Windows; Linux needs `/sys/class/power_supply` and a thermal zone reading.
- Tasks: 1.L8-1.L10 (Linux sandbox layers: user/mount/pid/net namespaces, seccomp allow-list, cgroup v2 memory,
  pids and CPU; refuse if any layer fails; start-up sweep), 1.L11 (daemon + unit file + .deb; ARM64 build in CI).

### G9 CLI requester is one blocking process per job

- `nc submit` uploads, submits and waits in one process. A requester that crashes or reboots cannot pick the job up
  again (no `nc job status <id>`, `nc job wait <id>`, `nc job fetch <id>`), and there is no batch submit.
- Task: 1.L7.

### G10 Signed modules: the signing works, publisher trust is manual

- The helper checks the module hash and the publisher's signature over it and over the whole job
  (`nc-protocol::jobs::verify_job`, `nc-exec` verify), and runs only publishers in the owner's `trusted_peers`.
- The owner has to paste each publisher key (`nc key`) into the policy. There are no coordinator-certified
  publisher keys, no revocation, and no module allow-list by hash. Acceptable for a small owner-run fleet; needed
  before strangers submit.
- Task: 1.L19.

### Other notes found while testing

- Device keys are software keys sealed with DPAPI (`SoftwareKeyStore`), marked lower assurance; TPM-backed keys
  are not implemented. Not a blocker for the PC MVP.
- macOS (PLAN.md "basic macOS"): same as Linux, nc-exec refuses jobs. Not in this list; after G8.
