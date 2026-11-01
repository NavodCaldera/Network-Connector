# Phase 0 report

Results of the Phase 0 feasibility gates (PLAN.md §8). Each agent writes only its own sections, with **real command
output**. Never invent results.

## 0.1 Toolchain, workspaces, CI (app-agent)

**Status:** pass locally; CI result below.

Installed on the owner's Windows 11 Pro PC (2026-11-01):

| Tool | Version | How |
|---|---|---|
| rustup | 1.29.1 | `winget install Rustlang.Rustup` |
| rustc / cargo | 1.99.1 (58cf8f9c3 2026-10-13), host `x86_64-pc-windows-msvc` | stable, pinned in `rust-toolchain.toml` |
| MSVC Build Tools 2022 | 17.14.37827.9 at `D:\BuildTools` (MSVC x64 tools + Windows 11 SDK 10.0.22621) | winget, minimal components |
| cargo-deny | 0.20.2 | GitHub release binary, SHA-256 checked against the published `.sha256` |
| Flutter | 3.47.9 stable, Dart 3.13.7, at `C:\src\flutter` (on user PATH) | `git clone -b stable` |

The first Build Tools install (full VCTools workload, `--includeRecommended`, on C:) failed with `0x80070070`
(disk full: C: had 5.4 GB free). The minimal component set installed to D: worked. C: is now about 1.4 GB free.

`flutter doctor` summary:

```
[√] Flutter (Channel stable, 3.47.9, on Microsoft Windows [Version 10.0.26200.9711], locale en-US)
[√] Windows Version (Windows 11 or higher, 25H2, 2009)
[X] Android toolchain - develop for Android devices        (no Android SDK; not needed until 0.9)
[√] Chrome - develop for the web
[!] Visual Studio - develop Windows apps (Visual Studio Build Tools 2022 17.14.44)
    X missing components: C++ CMake tools for Windows (+ v142 tools listed; v143 is installed)
[√] Connected device (3 available)
[√] Network resources
```

The Visual Studio item blocks Flutter **Windows desktop** builds (task 0.8), not Rust. Fix before 0.8 (needs more
free space on C:).

Local checks, `core/` (11 crates) and `server/` (2 crates):

```
cargo fmt --all --check                    ok (both)
cargo clippy --all-targets -- -D warnings  Finished, no warnings (both)
cargo test                                 core: 11 passed, 0 failed; server: 2 passed, 0 failed
cargo deny check                           advisories ok, bans ok, licenses ok, sources ok (both)
```

CI: `.github/workflows/ci.yml`, matrix windows-latest + ubuntu-latest × core/server (fmt, clippy, test) plus
cargo-deny per workspace. Actions pinned by commit SHA.

## 0.2 Wasmtime executor + Windows sandbox (core-agent)

**Status:** pass on the dev PC (Windows 11 Pro 26200, i7-13620H, not elevated). Linux sandbox not built yet:
on non-Windows the executor **refuses every job** (tested), it does not run without an OS sandbox.

Branch `feat/core-exec`, crate `core/crates/nc-exec`, Wasmtime **50.0.1** pinned (`=50.0.1`, features
`cranelift, runtime, std`; no WASI, no cache, no component model).

Layers per job (all required; failure of any one → `ExecError`, job refused):

| Layer | Where | Enforces |
|---|---|---|
| SHA-256 of module = manifest hash | parent, before spawn; re-checked in worker | tampered/wrong module |
| Publisher signature (`ModuleVerifier` trait) | parent, before spawn | unsigned/untrusted module (Ed25519 wired in 0.3) |
| Import allowlist `nc.{input_size,input_read,output_write,checkpoint}` with exact types | worker, before linking | WASI, `env.*`, anything else |
| Fuel + epoch deadline (10 ms tick thread) | worker | CPU loops |
| `ResourceLimiter` (memory, tables, 1 instance/memory/table), `max_wasm_stack` | worker | memory growth, recursion |
| Output / checkpoint caps in host API, plus capped pipe reader in parent | both | huge output |
| Separate single-use `nc-exec-worker.exe` | process | crash isolation |
| AppContainer, **0 capabilities**, cwd = container folder, 4-variable env | OS | files, network, registry |
| Job Object: process+job memory cap, `ActiveProcessLimit=1`, kill-on-close, die-on-unhandled-exception, all UI limits, completion port; `CHILD_PROCESS_POLICY=restricted`; `JOB_LIST` at create; `CREATE_SUSPENDED` | OS | native memory, child processes, leftovers |
| Wall-clock watchdog (wall + grace, 5 ms tick), `CancelToken` | parent | hung worker, Stop |
| Teardown on every path (`Drop` + explicit), start-up `sweep_leftovers()` (registry `AppContainer\Mappings`, owner-PID check) | parent | leftover profiles/folders |

Test results (`cargo test -j 1 -p nc-exec -- --nocapture`, real run on this PC):

```
tests\refusals.rs        5 passed   (wrong hash, tampered module, bad signature, bad limits, missing worker → refused)
tests\runtime_limits.rs 12 passed   (in-process Wasmtime layer, also runs on Linux CI)
tests\sandbox_windows.rs 12 passed  (full stack: AppContainer + Job Object + worker)
unit tests               5 passed
```

Malicious modules through the full sandbox: infinite loop → `OutOfFuel` (fuel 1e8) and `EpochDeadline`
(fuel ~∞, 500 ms budget: call returned in 627–758 ms incl. process start/compile); memory growth (64 MB cap) →
`MemoryLimit`, worker peak 64 MB; 64 KB-per-call output flood (1 MB cap) → `OutputLimit`, no output returned;
deep recursion (512 KB stack) → `StackOverflow`; WASI import → refused; hung worker (stands in for a blocking host
call) → `WatchdogKilled` after 614–724 ms for a 300 + 200 ms budget, kill → exit 2–13 ms. A native 1 GB allocation
inside the sandbox with a 64 MB Job cap → `PROCESS_MEMORY_LIMIT` event, killed (exit `0xE0000001`), no output.

Escape probe (a native test binary started in the sandbox in place of the worker; control = the test process):

```
read  <temp dir secret file>        READ_DENIED 5      (control: readable)
write <temp dir>\planted.txt        WRITE_DENIED 5
read  %USERPROFILE%\NTUSER.DAT      READ_DENIED 5
write %USERPROFILE%\nc-escape.txt   WRITE_DENIED 5
write E:\nc-escape.txt              WRITE_DENIED 5
tcp   127.0.0.1:<test listener>     TCP_DENIED (timed out; listener got 0 connections)
tcp   1.1.1.1:443                   TCP_DENIED 10013   (control: reachable)
dns   example.com                   DNS_DENIED 11001
spawn cmd.exe                       SPAWN_DENIED 367   ("process creation has been blocked")
env                                 ENV_COUNT 4
read  C:\Windows\win.ini            READ_OK 92         (honest limit: world-readable OS files stay readable)
```

Cancel (Stop) latency, warm worker, `cancel()` → `run()` returned (5 runs, cancel 0/20/100/400/1000 ms after
start): **6.7 ms, 150 ms, 57 ms, 40 ms, 54 ms**; kill → process exit 0–12.6 ms. All < 1 s.

Executor start time (echo job, warm binary, 6 runs): profile create 33–124 ms, Job + launch 8–43 ms, compile
16–35 ms (debug build), run < 1 ms, teardown 11–41 ms, **total 105–253 ms**. Numbers vary with load: other agents
were compiling on the same PC and free commit memory was ~1.2 GB.

**Finding: cold first launch of a new worker binary is slow.** A freshly copied `nc-exec-worker.exe` took
**2.1–4.8 s** inside `CreateProcessW` on its first launch (4 fresh copies), then 6–27 ms. Probably the anti-malware
scan of a new image (not proven). Consequences, all handled: the worker is created suspended and only resumed if
no cancel arrived, so a Stop during that window runs no guest code; `Executor::warm_up()` spawns and tears down a
suspended worker without running anything, for the agent to call at start-up and after updates. Open: `run()` still
cannot *return* before `CreateProcessW` does, so the agent's Stop must not wait on a job that is still launching.

Once, under heavy load from other builds, a warm echo run did not finish and was killed by the watchdog at
62 s (wall 60 s + 2 s grace); the worker runs at `BELOW_NORMAL_PRIORITY_CLASS`. The watchdog worked; the cause
(starvation vs. something else) is not confirmed.

Setup facts: the worker's folder needs `ALL APPLICATION PACKAGES:(OI)(CI)(RX)` (`grant_appcontainer_read`, only
that folder; no admin). Profiles are named `nc-exec-<owner pid>-<nonce>`; the sweep deletes those whose owner is gone
and the test confirms the profile and its folder disappear. No leftover profiles after any test.

## 0.3 Identity, manifests, lease store (core-agent)

**Status:** pass. Branch `feat/core-store` (based on `feat/core-exec`, because it wires the real publisher check
into the executor).

- `proto/nc/v1/manifest.proto` (`TaskManifest`: task id, profile, module hash, publisher key + signature, input
  hashes, limits, verifier, expiry; `SignedManifest`) and `proto/nc/v1/identity.proto` (`SessionCert`,
  `SignedSessionCert`, `KeyAssurance`). Generated by `prost-build` 0.14 in `nc-protocol/build.rs` with the
  vendored `protoc` (`protoc-bin-vendored` 3.3), so no system install.
- `nc-protocol`: Ed25519 (`ed25519-dalek` 3, `verify_strict`) over `domain || 0 || exact bytes`; three domains
  (`nc.v1.TaskManifest`, `nc.v1.module`, `nc.v1.SessionCert`). `verify_manifest` order: 64 KB cap → issuer
  trusted → signature over received bytes → decode → version → structure → publisher trusted → module signature →
  expiry.
- `nc-identity`: `DeviceKeyStore` adapter trait; `SoftwareKeyStore` = Ed25519 seed sealed with **DPAPI**
  (current user, app entropy, UI forbidden) on Windows, 0600 file elsewhere; atomic create, corrupt file refused
  (never silently replaced), key and path kept out of `Debug`. Session keys: ephemeral Ed25519, cert signed by the
  root, lifetime ≤ 24 h, 5 min skew, optional pinned root.
- `nc-store`: SQLite (`rusqlite` 0.41, bundled), WAL, `synchronous=FULL`, migrations via `user_version` (newer
  schema refused), every transition one `BEGIN IMMEDIATE` transaction. States `queued → leased → running →
  uploaded → verified → committed`, plus `failed`; attempts table with `active/expired/failed/rejected/committed`.
  Random 16-byte attempt ids; expired leases are ended when touched and by `expire_leases`; exponential backoff
  with cap; max attempts.
- `nc-exec`: `Ed25519Publishers` (real `ModuleVerifier`) and `Job::from_manifest`; replaces the 0.2 TODO.

Test results (`cargo test`, this PC):

```
nc-protocol tests\manifest.rs  11 passed (2 proptests × 256 cases: any bit flip refused, garbage never verifies)
nc-identity tests\identity.rs   9 passed (persist, DPAPI scheme on disk, corrupt refused, cert window/TTL/root/tamper)
nc-store    tests\lease.rs     10 passed (8 unit + 2 proptests × 300 cases), 2.7 s
nc-exec     tests\refusals.rs   9 passed (incl. 4 with real Ed25519 publisher keys)
```

Property `lease_invariants_hold` (random sequences of up to 80 lease/start/renew/upload/verify/commit/fail/
advance-clock/expire operations over 3 tasks) checks after every step: a task is committed at most once and stays
committed by the same attempt; only the current attempt in `verified` commits; every successful transition came from
the current attempt with a live lease; late work on an expired lease gets `LeaseExpired` and the task is re-queued
or failed; a stale attempt gets `StaleAttempt`; attempt ids are never reused; attempts ≤ max; after a sweep nothing
is leased past its expiry. At the end, replaying every commit returns `AlreadyCommitted` and changes nothing, and
every other attempt of a committed task is refused. `untouched_leases_always_expire` checks that any lease left
alone is re-queued once its time passes.

Hardware fact for the TPM adapter (`tpmtool getdeviceinformation`, not elevated):

```
-TPM Present: True   -TPM Version: 2.0   -TPM Manufacturer Full Name: Intel   -TPM Vendor Id: ADL
-Is Initialized: True   -Ready For Storage: True   -Ready For Attestation: True
```

Not checked: whether this TPM (through the Windows Platform Crypto Provider) can hold an Ed25519 key. If it only
offers ECDSA P-256, a hardware root needs a second algorithm in `SessionCert`/`SignedManifest` (the plan already asks
for algorithm agility). Until then the device root is software-only and says so (`KEY_ASSURANCE_SOFTWARE`).

## 0.4 Tariff profiles, planner, simulator

## 0.5 libp2p across two ISPs (net-agent)

**Status:** test kit ready and self-tested on one PC; **the two-ISP run is pending (owner run)**. No two-ISP numbers
exist yet. None are estimated here.

Kit: `nc-netprobe` (bin in `core/crates/nc-net`), modes `id`, `relay`, `listen`, `dial` (the throughput test is
part of `dial`: N seconds upload, then N seconds download, over the authenticated stream API). Steps for PC A
(ISP 1) and PC B (ISP 2) are in [`net/RUN.md`](net/RUN.md). The dev `relay` mode serves only the peer ids given with
`--allow` (relay v2 reservation and circuit limiters), so it is never an open relay.

Ladder rungs recorded per connection: `lan_direct`, `internet_direct`, `hole_punched` (DCUtR), `relayed`. TLS/443
fallback is **not implemented** in Phase 0. Bytes on `PeerStream`s are counted as direct or relayed.

Local self-test, 2026-11-01, this PC (Windows 11, loopback only, debug build, `--bind 127.0.0.1`): dev relay on
127.0.0.1:4101; A `listen --relay`; B `dial <offer> --relay --yes --test-secs 5`. Excerpts of real output:

```
A: relay reservation accepted by 12D3KooWPeMd...
B: connected 12D3KooWLKNt... rung=Relayed via /ip4/127.0.0.1/udp/4101/quic-v1/p2p/12D3KooWPeMd.../p2p-circuit/p2p/12D3KooWLKNt...
B: SHORT AUTHENTICATION STRING: <6 digits, same on A>   paired ... in 0.07s
B: dcutr 12D3KooWLKNt... ok=true
relay: ReservationReqAccepted {A}, CircuitReqAccepted {src B, dst A}
B result: connections_at_test = [hole_punched, lan_direct x3] (relay circuit closed after the direct path came up)
          upload_mbit_s 773.78, download_mbit_s 770.31, traffic {direct_bytes 982056978, relayed_bytes 0}
```

This shows the pieces work together (pairing over a circuit, DCUtR upgrade, rung and byte accounting). Loopback numbers
say nothing about real NAT or ISP behaviour.

Integration test `core/crates/nc-net/tests/relayed.rs` (in-process relay, loopback): B's first connection to A is
`Relayed`, DCUtR succeeds, the stream then runs direct and `TrafficStats` = `{direct_bytes: 100000, relayed_bytes: 0}`.

**Pending owner run:** tests 1-3 of `net/RUN.md` (LAN baseline; two ISPs without relay; two ISPs via relay, 5 runs
per direction). Test 3 needs a relay reachable from both ISPs: a machine with a public IPv4 + port-forward, or a
cloud VM, which needs owner approval (cost).

## 0.6 mDNS + QR pairing (net-agent)

**Status:** pass locally (loopback, in-process peers). Not yet run between two physical PCs on one LAN (that is
test 1 of `net/RUN.md`).

What was built (`core/crates/nc-net`, rust-libp2p 0.57.1 pinned):
- Transport: QUIC v1, with TCP + Noise + Yamux as fallback, plus relay v2 client, AutoNAT (v1), DCUtR, identify,
  ping, mDNS (on/off by config).
- Pairing: QR text `nc-pair:v1:<base64url JSON>` = (peer id, addresses, 32-byte one-time secret, expiry). The
  scanner dials **by peer id**, so the Noise/TLS handshake proves the key. Both sides exchange nonces and
  HMAC-SHA256 proofs bound to both peer keys and nonces; the secret never crosses the wire. Both show a 6-digit
  SAS from the same transcript; trust is stored only after **both** users confirm. Offer: single use, 10 min,
  locked after 5 bad proofs (same rules as legacy 0.x pairing).
- Trust store keyed by peer id only, optionally persisted (JSON file) for reconnect.
- Gate: a connection from/to a peer that is not trusted, not an infrastructure (relay) peer and not inside a
  pairing window is closed at once; untrusted connections during an open offer are closed after 150 s. mDNS
  discoveries are only dialled if already trusted.
- Core API (AGENT.md §3): `Node::open_stream(peer_id, protocol)` and `Node::accept(protocol)`, both refuse
  untrusted peers; `PeerStream::peer()` is the authenticated key. Pairing/relay-auth protocol ids are reserved.

`cargo test -p nc-net` (2026-11-01, real output): unit 6 passed; `tests/loopback.rs` 9 passed
(`pair_and_stream_quic`, `pair_and_stream_tcp_fallback`, `offer_is_single_use`,
`unpaired_peer_is_rejected_even_if_it_knows_the_address`, `wrong_secret_fails_and_locks_after_five`,
`impostor_at_the_offered_address_is_detected`, `sas_mismatch_means_no_trust`,
`reconnect_after_restart_with_persisted_trust`, `reserved_protocols_are_not_available_to_core`);
`tests/relayed.rs` 1 passed.

Found and fixed during testing: (1) a race where the pairing stream dialled before the peer's addresses were
registered ("no addresses for peer"); (2) closing the relay circuit as soon as DCUtR succeeded killed a pairing
exchange still running on it. Circuits are now closed 3 s after a direct path exists, and never during pairing.

Not covered: real multicast mDNS between two machines (CI and loopback cannot show it), Wi-Fi Direct / BLE.

## 0.7 Minimal coordinator + relay (net-agent)

**Status:** pass on **local Docker** (lease cycle over HTTP against the compose stack). Not run over the internet:
no cloud VM was created (needs owner approval).

What was built:
- `server/crates/nc-coordinator` (Axum 0.8, sqlx 0.8 + PostgreSQL, embedded migrations). `POST` endpoints
  `/v1/devices/register`, `/v1/devices/heartbeat`, `/v1/leases/request`, `/v1/leases/complete`,
  `/v1/relay/token`, plus open `GET /healthz`. Every `/v1` request is signed by the device's Ed25519 key over
  method, path, timestamp (±300 s), nonce and body hash; nonces are stored, so replays are refused. Phase 0 has
  no OIDC accounts: `register` also needs the owner's enrolment secret. Bodies capped at 16 KiB, JSON with
  `deny_unknown_fields` + per-field validation, one error format
  `{"error":{"code","message"}}` for every failure including 404/405/413/415. Lease commit is idempotent and
  attempt-checked (same attempt again = `duplicate: true`; other attempt = 409 `already_committed`; expired =
  409 `lease_expired`). HTTPS when `NC_TLS_CERT`/`NC_TLS_KEY` are set; default bind 127.0.0.1:8080.
- `server/crates/nc-relay` (libp2p relay v2). Refuses to start without `NC_RELAY_TOKEN_KEY`. Clients present a
  coordinator-issued token (HMAC-SHA256, bound to peer id + account + device, at most 15 min) on
  `/nc/relay-auth/1.0.0`; the token's peer id must match the connection's authenticated key. Reservations and
  circuits are allowed only for granted peers, with per-peer and per-account reservation and byte quotas (each
  circuit is charged its full byte cap, so quotas are conservative). Default bind loopback.
- `infra/docker-compose.yml` (postgres 17 + coordinator + relay, one image from `infra/Dockerfile`). Published ports
  bind 127.0.0.1; PostgreSQL is not published.

Real results (2026-11-01, this PC, Docker Desktop 29.3.1):

```
cargo test (server)          nc-coordinator: unit 1 + validation 7 passed, lease_cycle 1 ignored (needs stack)
                             nc-relay: unit 4 + relay_auth 1 passed
docker compose up -d         postgres Healthy, coordinator running 127.0.0.1:8080, relay running 127.0.0.1:4001 tcp+udp
curl /healthz                {"ok":true}
relay log                    StatusChanged { status: Enable }
lease_cycle -- --ignored     registered device 5f7cc0b2-...; lease {... "input":"phase0-dummy-5082c8bf" ...};
                             committed task "5082c8bf-..."; relay token issued; test result: ok. 1 passed
psql                         tasks: committed 1, leased 1; leases: completed 1, active 1; devices 2; migration 1 ok
```

`relay_auth` test: no token = reservation denied; valid token for own key = accepted; someone else's token and a
token signed with the wrong key = refused at auth and at reservation.

Found during testing: libp2p 0.57 relay v2 serves reservations only once the server has an external address, so
`NC_RELAY_EXTERNAL` was added (compose sets the loopback address).

Not done: the relay was not exercised end to end from `nc-net` through the compose port mapping (covered in-process
by `relay_auth`); no TLS certificate was tested; WebSocket endpoints; internet run (needs a VM, owner approval).

## 0.8 Flutter shell + nc-bridge (app-agent)

## 0.9 Android footprint + foreground service (app-agent)

## 0.10 iOS feasibility (app-agent)
