# Task board

Who is doing what, so Claude agents and Codex never edit the same files. Update this file when a task starts,
finishes or changes owner. Details of each change go in [WORKLOG.md](WORKLOG.md).

Status: `todo` · `in progress` · `review` (waiting for coordinator check) · `done` · `blocked`

## Phase 0 (PLAN.md §8)

| Task | What | Owner | Branch | Files it may touch | Status |
|---|---|---|---|---|---|
| 0.1 | Toolchains, Cargo workspaces, CI | app-agent (Claude) | `feat/app-skeleton` | repo skeleton, `.github/`, `rust-toolchain.toml`, `deny.toml` | done |
| 0.2 | Wasmtime executor + Windows sandbox | core-agent (Claude) | `feat/core-exec` | `core/crates/nc-exec` | done |
| 0.3 | Identity, manifests, lease store | core-agent (Claude) | `feat/core-store` | `proto/`, `core/crates/nc-{identity,protocol,store}` | done |
| 0.4 | Tariff profiles, planner, simulator | **Codex** | `feat/codex-tariff` | `core/crates/nc-tariff` only | done |
| 0.5 | libp2p direct/DCUtR/relay test kit | net-agent (Claude) | `feat/net-p2p` | `core/crates/nc-net`, `spikes/phase0/net/` | review (kit done; two-ISP owner run pending) |
| 0.6 | mDNS + QR pairing | net-agent (Claude) | `feat/net-p2p` | `core/crates/nc-net` | done |
| 0.7 | Coordinator + relay (Docker) | net-agent (Claude) | `feat/net-server` | `server/`, `infra/` | done (local Docker) |
| 0.8 | Flutter shell + bridge | app-agent (Claude) | `feat/app-shell` | `app/`, `core/crates/nc-bridge`, small `nc-agent` facade | done |
| 0.9a | Android runtime footprint + foreground-service spike | app-agent (Claude) | `feat/app-android` | `spikes/phase0/android/` | in progress (host tests done; .so/APK build + phone run pending) |
| 0.9b | Google Play policy assessment (document) | **Codex** | `feat/codex-mobile-research` | `spikes/phase0/mobile-policy.md` only | done |
| 0.10 | iOS feasibility (document) | **Codex** | `feat/codex-mobile-research` | `spikes/phase0/ios-feasibility.md` only | done |

## Phase 1 prep (pure libraries, no collisions with Phase 0)

| Task | What | Owner | Branch | Files it may touch | Status |
|---|---|---|---|---|---|
| 1.P1 | Owner policy engine (`nc-policy`) | **Codex** | `feat/codex-policy` | `core/crates/nc-policy` only | done |
| 1.P2 | Chunked, hashed, resumable, encrypted blob cache (`nc-blob`) | **Codex** | `feat/codex-blob` | `core/crates/nc-blob` only | done |
| 1.P3 | Move all wire types into `proto/` | **Codex** | `feat/codex-proto` | `proto/`, `core/crates/{nc-protocol,nc-net}`, `server/crates/*` | done |
| 1.P4 | Protocol-aware, cacheable Docker builds and live lease-cycle verification | **Codex** | `feat/codex-infra` | `infra/` only | done |
| 1.P6 | Helper supervisor `nc-agent` (lib + daemon) wiring coordinator, store, policy, tariff, executor | Core agent (Claude) | `feat/agent-supervisor` | `core/crates/nc-agent`, `core/crates/nc-cli`, `core/Cargo.toml` members/deps only | review |
| 1.P5 | Ubuntu CI Docker stack build and live lease-cycle check | **Codex** | `feat/codex-ci-stack` | `.github/workflows/`, `infra/verify.sh` | done |
| 1.P6a | Real job contract: signed manifests in leases, job submit, blob up/download, lease failure | **Codex** | `feat/codex-jobs-api` | `proto/`, `core/crates/nc-protocol`, `server/`, `infra/` (migrations) | done |
| 1.A1 | Owner UI: limits, night data, settings file, Home summary, navigation | app-agent (Claude) | `feat/app-ui` | `app/`, `core/crates/nc-bridge` | done |

| 1.P7 | Make core CI green without weakening security | **Codex** | `feat/codex-ci-fix` | `core/crates/nc-{identity,exec}`, core CI test step, own task/log entries | done |

## Rules for Codex work

- Same rules as the Claude agents: [AGENT.md](AGENT.md) §1 (security invariants, work log, hand-back format).
- Codex never edits `main` or files outside its row. It pushes its branch; the coordinator (Claude) reviews,
  runs the checks and merges, then records the review result in WORKLOG.md.
