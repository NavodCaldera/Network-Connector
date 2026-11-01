# Network Connector platform (v1): plan

> This plan replaces the Python 0.x plan. The Python app was removed from `main` and is kept unchanged on branch
> `legacy/python-v0.3` as the **reference implementation**: behaviour it already has (consent, identity, Stop, benchmarks) is ported, not reinvented.
>
> Source: owner-approved research (2026-11-01, Perplexity report + coordinator review). Owner decisions are marked
> **(owner)**.

---

## 1. Goal

One platform where **any capable device** can give or ask for compute, on the **local network and across the
internet** **(owner)**, built to industry standard **(owner)**.

The milestone that matters first is not "every phone joins". It is:

> A small fleet completes useful jobs **correctly**, survives failures, **honours owner policies**, and costs less
> than it is worth after bandwidth, verification and retries.

### Device roles

| Device | Role in v1 | Notes |
|---|---|---|
| Windows / Linux / macOS PCs, servers | **Primary helpers** and requesters | Persistent background agent |
| Raspberry Pi / ARM64 Linux | Headless helpers | Capability profile per architecture |
| Android | Requester; helper **only in user-started sessions** | Foreground service + notification; Play policy gate |
| iOS / iPadOS | Requester and monitor; bounded, user-started bundled kernels | No unattended overnight work (Apple limits, App Review 2.5.2) |
| Browser | Optional foreground worker, later | Separate browser executor (no WASI host) |

### Out of scope for v1

GPU jobs, payments/marketplace/tokens, federation, arbitrary native/container jobs, distributed training,
confidential data on untrusted helpers.

---

## 2. Technology stack (decided)

| Layer | Choice | Notes |
|---|---|---|
| Device core | **Rust** (stable), Tokio | Headless agent; UI is a client of it |
| UI | **Flutter** + `flutter_rust_bridge` | Windows, macOS, Linux, Android, iOS |
| Job execution | **Wasmtime** (pinned), core-Wasm profile `cpu-core-v1` first | Fuel + epoch + external watchdog |
| OS isolation | Windows AppContainer + Job Object; Linux namespaces/seccomp/cgroups; macOS sandbox (later) | Extra layers, not a substitute for a narrow host API |
| Mobile executor | Phase 0 spike: Wasmtime/Pulley vs WAMR interpreter | iOS ships bundled kernels first |
| Peer networking | **rust-libp2p**: QUIC, Noise/TLS, relay v2, AutoNAT, DCUtR | Direct first, relay fallback |
| Local discovery | mDNS; QR pairing; Wi-Fi Direct / Wi-Fi Aware adapters where validated | BLE for bootstrap only. Not MultipeerConnectivity (deprecated) |
| Control plane | Rust **Axum**, HTTPS + WebSocket, **Protobuf** schemas | Modular monolith |
| State | PostgreSQL (server), SQLite (device), S3-compatible encrypted blobs | |
| Identity | OIDC/passkey accounts; device root key in OS keystore (TPM, Android Keystore/StrongBox, Secure Enclave); short-lived session keys | Software fallback marked lower assurance |
| Hosting | Singapore VPS (control plane) + separate relay VM; measure Mumbai | Pilot budget ≈ $50–150/month |
| Ops | OpenTelemetry, Prometheus metrics, Sentry-compatible crash reports, signed staged updates, SBOM | Never log job data or secrets |

---

## 3. Architecture

```text
                    INTERNET CONTROL PLANE (Axum + PostgreSQL + blob store)
   accounts · device registry/certs/revocation · job planner · capability + tariff-aware scheduler
   leases · verifier · reputation/abuse · rendezvous/signalling
                 │ outbound HTTPS/WSS            │ scoped, short-lived blob URLs
          ┌──────┴──────┐                 ┌──────┴──────┐
          │ Requester A │◄══ direct QUIC ═►│  Helper B   │
          └──────┬──────┘                 └──────┬──────┘
                 ╚═════════ encrypted relay ═════╝   (authenticated, quota-limited relays)

  Inside each device:
   Flutter UI / CLI ── typed bridge API ──► Rust supervisor (nc-agent)
                                             ├ policy engine (consent, limits, hours, battery, thermal)
                                             ├ tariff engine (network budgets, time windows)
                                             ├ identity + platform keystore adapter
                                             ├ networking + discovery (libp2p)
                                             ├ SQLite queue + encrypted blob cache
                                             └ isolated executor process ─ Wasmtime ─ narrow host API

  LOCAL-ONLY MODE: one trusted device runs an embedded coordinator. Discovery → explicit pairing → leases →
  direct LAN transfers. No account or cloud needed.
```

Rules:
- **The coordinator decides who may run what. The device has the final say.** A helper refuses any lease or transfer
  that breaks its owner's policy.
- Payloads go peer-to-peer when possible, otherwise through the relay or encrypted object staging.
- No all-to-all mesh: a device keeps connections to the control plane and a few assigned peers.
- Protocol, job format and coordinator are open and versioned.

### Workload model

Submitters must declare: module hash(es) and inputs; map/reduce or DAG shape; resources and execution profile;
checkpoint behaviour; result validator; deadline, transfer budget and trust tier. v1 targets independent,
CPU-heavy, modest-data tasks (parameter sweeps, simulations, image transforms, batch processing).

### Task state machine

```text
queued → leased → running → uploaded → verified → committed
                     ↘ failed / expired → (retry with backoff, limit)
```
Leases expire. Attempt IDs are unique. Commit is idempotent; late or duplicate results never complete a task twice.

---

## 4. Repository layout (on this branch)

```text
core/                    Cargo workspace (device side)
  crates/nc-protocol     Protobuf types, versioning, manifest + task schema
  crates/nc-identity     device root key, session certs, keystore adapters
  crates/nc-policy       owner policy engine (consent, limits, hours, battery, thermal)
  crates/nc-tariff       network tariff profiles, byte budgets, window planner
  crates/nc-store        SQLite queue, leases, attempts, migrations
  crates/nc-blob         chunked, hashed, resumable, encrypted blob cache
  crates/nc-exec         executor process: Wasmtime + OS sandbox + watchdog
  crates/nc-net          libp2p transport, discovery, pairing, relay client
  crates/nc-agent        supervisor binary (headless agent)
  crates/nc-cli          `nc` command line
  crates/nc-bridge       flutter_rust_bridge API surface
server/                  Cargo workspace (cloud side)
  crates/nc-coordinator  Axum control plane: accounts, registry, scheduler, leases, verifier
  crates/nc-relay        libp2p relay v2 with tokens and quotas
proto/                   .proto files (single source of truth for the wire format)
app/                     Flutter application
infra/                   infrastructure as code, deployment scripts
spikes/phase0/           throwaway feasibility code + REPORT.md
docs/                    user, operator and security docs
```

---

## 5. Security model

### Threats assumed

Malicious submitter (sandbox escape, host-function abuse); malicious helper (fake results, stealing inputs);
collusion and Sybil identities; relay observing metadata or dropping traffic; compromised coordinator; compromised
update or dependency; honest failures (crashes, overheating, numerical differences).

> **Sandboxing protects the helper from the job. It does not protect the job's data from the helper's owner.**
> Confidential data is never sent to untrusted helpers just because the link is encrypted.

### Identity

1. Account identity: OIDC / passkeys.
2. Device root identity: generated on the device, non-exportable where hardware allows.
3. Short-lived transport/session keys certified by the root. Algorithm agility kept.

Local pairing uses a QR code or short authentication string bound to a signed challenge. **Discovery is not
authorisation.**

### Verification (v1)

Deterministic tasks run on **two independently chosen helpers**; disagreement → third helper or trusted verifier.
Group numerically sensitive jobs by execution profile. Track correctness and reliability separately; raise
replication for new or suspicious helpers. Two matching answers are not proof (collusion), and reputation alone
does not stop Sybils.

---

## 6. Data-cost-aware scheduling (Sri Lanka night data)

Several Sri Lankan ISPs give free or cheap data at night **(owner requirement)**. This is a first-class subsystem.

- Pipeline stages are scheduled **separately**: module download → input transfer → execution → verification →
  result upload. A device may download at night, compute while charging and upload in the next cheap window.
- Each connection has a **versioned tariff profile** (time zone, connection binding confirmed by the owner, windows
  with price/GB and byte caps, monthly reset, safety reserve, cellular fallback flag, boundary guard). Windows are
  **user-configured**, never hard-coded ISP facts. SSID alone is not trusted as a tariff identifier.
- **Enforced on the device**: `allowed = consent ∧ right network ∧ allowed time ∧ budget left ∧ power/thermal OK`.
  Separate permissions for bulk payloads, module downloads/updates, result uploads, control traffic and recovery.
  A strict "zero data outside the window" mode stops even heartbeats and update checks.
- Scheduler ranks feasible assignments by `α·data + β·relay + γ·energy + δ·completion time + ε·failure risk`, starting
  with heuristics: cached inputs first, LAN sources first, open windows first, direct over relay, reject transfers
  that can't finish before the window closes, reserve bytes for verification/retries/uploads, stage through object
  storage when windows don't overlap. Costs at **both** ends count.
- Transfers are chunked, hashed and resumable. Budgets are reserved atomically and reconciled afterwards
  (retransmits, relay overhead and duplicates included). Stop before the boundary guard. Handle clock changes and
  cross-midnight windows. Pause at once when Wi-Fi switches to cellular. Jitter starts so not everyone begins at
  00:00. The byte counter is an estimate, not the ISP's bill.
- On iOS, overnight plans are best-effort only.

---

## 7. Owner control (helper side)

CPU budget, memory and disk caps, max concurrent tasks, charging-only and minimum battery, network restrictions and
data caps, time windows, pause on user activity, thermal stop, and **Stop that halts all jobs within 1 s**. Fuel is
not a CPU-percent limiter; combine concurrency, pacing and OS controls, and say "best effort" where the OS gives no
hard quota.

---

## 8. Phases

Team reality: the owner plus Claude Code agents. Scope is cut to a **desktop + Raspberry Pi MVP first**; Android
next, iOS last.

### Phase 0 — feasibility gates (target 4–6 weeks)

| # | Spike | Owner agent | Pass condition |
|---|---|---|---|
| 0.1 | Toolchain + Cargo workspace skeleton + CI (Windows, Linux) | app-agent | `cargo test` green in CI |
| 0.2 | Wasmtime runs a `cpu-core-v1` module with fuel, epoch, memory cap, watchdog; Windows AppContainer + Job Object around it | core-agent | malicious-job tests (loop, memory growth, huge output) all contained |
| 0.3 | Device identity + signed task manifest + SQLite lease state machine | core-agent | property tests: no double commit, leases expire |
| 0.4 | Tariff profile + window planner + simulator | core-agent | simulated nights never exceed budget or window |
| 0.5 | libp2p: two PCs on **different Sri Lankan ISPs** connect direct (DCUtR) or via our relay | net-agent | real measurements recorded: direct %, relay %, throughput |
| 0.6 | mDNS + QR pairing on one LAN | net-agent | pair, reconnect, impostor rejected |
| 0.7 | Minimal Axum coordinator + relay on a Singapore VM (or local Docker first) | net-agent | lease cycle over the internet |
| 0.8 | Flutter shell on Windows calling the agent through `flutter_rust_bridge` | app-agent | start/stop a helper session from the UI |
| 0.9 | Android: Rust + Wasm runtime footprint; foreground-service session; Play policy notes | app-agent | numbers + written policy assessment |
| 0.10 | iOS feasibility notes (Pulley/WAMR, `BGContinuedProcessingTask`, App Review) | app-agent | written go/no-go |

Results go in `spikes/phase0/REPORT.md` with real command output. **Gate:** no promise of mobile helpers until 0.9/0.10 pass.

### Phase 1 — MVP (≈ 10–14 weeks)

Windows/Linux helpers (+ basic macOS), Raspberry Pi headless agent, Flutter consent/limits UI, LAN + internet,
coordinator leases with direct transfer and relay fallback, 1–2 deterministic CPU workloads, signed modules, isolated
executor, two-helper validation, night-window transfers with resumable chunks.
**Exit test:** jobs complete repeatedly through worker crashes, disconnects and tariff boundary changes **without
ever breaking an owner limit**.

### Phase 2 — beta (≈ 4–6 months)

macOS packaging, Android helper sessions, iOS submit/monitor + approved bounded kernels, checkpoints, better
capability/reliability estimates, abuse controls and reputation, multi-network tariffs, real-device test lab,
restore drills, signed updates, security review.

### Phase 3 — production (≈ 6–9 months)

HA control plane where justified, on-call/SLOs, external pentest, tenant separation, curated GPU tier, self-hosting
docs, scoped federation, incident response, compliance operations.

---

## 9. Engineering standards

- **CI** for Windows, macOS, Linux, Android, iOS (+ ARM64 where possible). `cargo fmt`, `clippy -D warnings`,
  `cargo test`, `cargo deny` (licences/advisories), `cargo audit`, Flutter analyze/test.
- **Tests:** policy tests (battery, network changes, windows, budget edges); state-machine/property tests (leases,
  cancel, retry, idempotent commit); fuzzing (manifests, frames, chunks, checkpoints); malicious-job tests; numerical
  tests across x86-64/ARM64/interpreter; chaos tests (relay down, coordinator restart, clock jump, sleep/wake,
  partition, full disk); restore and update-recovery drills.
- **Releases:** Authenticode, Apple signing + notarisation, store delivery on mobile (no self-updating mobile
  binaries), signed desktop update manifests with staged rollout and rollback, SBOM, separate signing keys.
- **Metrics:** accepted vs rejected jobs, completion and verification latency, direct vs relayed bytes, useful vs
  recomputed work, policy pauses, tariff prediction error, crashes, cost per verified result.
- **Privacy:** data minimisation and retention, separate compute vs telemetry consent, account deletion, cross-border
  processing review, Sri Lanka **PDPA** (parts commence **1 Jan 2027**) and GDPR where applicable.

---

## 10. Lessons we reuse

BOINC (work units, replication, validation, owner controls) · Tailscale (direct first, encrypted relay fallback) ·
Syncthing (explicit device pairing) · libp2p (transport + NAT traversal) · Golem/iExec/Akash (study only; no tokens
before unit economics) · Ray (trusted clusters only) · Mesos is retired (Aug 2025); don't use · KubeEdge only for
managed Linux fleets.
