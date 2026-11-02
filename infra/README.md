# Local server infrastructure

`docker-compose.yml` builds the coordinator and relay from one multi-stage image
and runs them with PostgreSQL 17. Published ports bind only **127.0.0.1**;
PostgreSQL has no published port. Runtime processes use the unprivileged `nc` user.

## Verify the complete stack

From the repository root, with Docker Desktop's Linux engine running and Rust/Cargo
on PATH:

```powershell
powershell -NoProfile -File infra/verify.ps1 -CheckCache
```

The script generates cryptographically random credentials in its process environment
only. It writes no secret files and prints no credentials. It builds with
`docker compose -f infra/docker-compose.yml build`, repeats the build to check cache
reuse when `-CheckCache` is given, runs `up -d --wait`, checks `/healthz` and both
runtime UIDs, then runs the existing integration check:

```powershell
cargo test --manifest-path server/Cargo.toml -p nc-coordinator --test lease_cycle -- --ignored
```

A unique Compose project isolates the run. A `finally` block always runs `down
--volumes --remove-orphans` after contacting the engine, including on build/test
failure, then restores the caller's environment. It removes only this run's volumes;
the generated credentials are disposable. Port 8080 and TCP/UDP 4001 must be free.
BuildKit caches and the finished image remain available for subsequent builds.

Use `docker compose ... --env-file ...` only for a separately managed deployment;
never commit, paste or log credentials. The verification workflow needs no `.env`.
Do not print resolved `docker compose config` output: it contains service credentials.

## Build context and cache

The Compose build context is the repository root. `Dockerfile.dockerignore` admits
only `rust-toolchain.toml`, the core workspace manifest, `nc-protocol`, `proto/`
and `server/`, and excludes host targets, local secret files and Git metadata.
Both Docker COPY operations and Cargo paths preserve the repository layout:

- `server/Cargo.toml`, `server/Cargo.lock` and both server crate manifests;
- `core/Cargo.toml` and `core/crates/nc-protocol/Cargo.toml` for workspace inheritance;
- `core/crates/nc-protocol/`, `proto/` and `server/` sources for compilation.

Manifest copies precede source copies. Empty source stubs let `cargo fetch --locked`
inspect targets without compiling application code; real source copies replace them.
BuildKit caches the Cargo registry, Git dependencies and server target directory.
`server/Cargo.lock` locks the entire server graph, including `nc-protocol`;
`core/Cargo.lock` is not used by this server build and is not copied. The build
exports only the two finished binaries from the target cache into the runtime stage.
No compiler, Cargo cache or source tree is copied into the minimal Debian runtime.

## Deployment notes

The coordinator serves HTTPS when `NC_TLS_CERT`/`NC_TLS_KEY` are configured; this
local verification stack uses HTTP on loopback. Cloud deployment still needs the
owner's approval. No cloud resources are created by this workflow.

## Fleet end-to-end test (Windows)

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File infra/fleet-e2e.ps1
```

Builds `nc-agent`, `nc` and `nc-exec-worker` (in `CARGO_TARGET_DIR`, default `C:\nc-target`), starts Postgres +
coordinator under a unique Compose project with its own image tag, runs two helper daemons (own data dir, key and
loopback control port each) and the `nc` CLI as requester, and drives real cpu-core-v1 jobs through: baseline,
helper killed mid-job and restarted, coordinator paused then restarted, two-helper validation (`--replicas 2`),
and a tariff window closing on one helper. Every job's output is compared byte for byte and the database is
checked for exactly one commit per task. Gaps from `docs/laptop-gap.md` are reported as `EXPECTED-FAIL`.
Teardown always stops the helpers, runs `down --volumes`, removes the per-run image (unless `-KeepImage`) and
deletes the run folder (unless `-KeepRunDir`). The sandbox is the real one; nothing is weakened for the test.

## Windows service test (elevated, owner-run)

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File infra\service-test.ps1 -BinDir C:\nc-target-svc\release
```

Run from an **elevated** PowerShell (it refuses otherwise, exit 2). Uses existing release `nc-agent.exe`, `nc.exe`
and `nc-exec-worker.exe` from `-BinDir` (no build). Brings up Postgres + coordinator under a unique Compose project,
installs `nc-agent` as a service (`NT SERVICE\nc-agent`) with its data dir and binaries in
`C:\ProgramData\NetworkConnector`, sets the owner policy with `nc`, and checks: C1 install/start, C2 registration,
C3 exact cpu-core-v1 output under the service's AppContainer, C4 `service stop` mid-job (< 1 s, lease failed `STOP`,
no worker left), C5 data dir ACL, C6 `sc qfailure` restart policy, C7 SCM restart after the process is killed.
It refuses to run if a `nc-agent` service or that ProgramData folder already exists. Teardown always uninstalls
the service, deletes the ProgramData folder (device key, `enroll.secret`, logs), runs `down --volumes` and removes
only the per-run image.
