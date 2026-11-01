# Task board

Who is doing what, so Claude agents and Codex never edit the same files. Update this file when a task starts,
finishes or changes owner. Details of each change go in [WORKLOG.md](WORKLOG.md).

Status: `todo` · `in progress` · `review` (waiting for coordinator check) · `done` · `blocked`

## Phase 0 (PLAN.md §8)

| Task | What | Owner | Branch | Files it may touch | Status |
|---|---|---|---|---|---|
| 0.1 | Toolchains, Cargo workspaces, CI | app-agent (Claude) | `feat/app-skeleton` | repo skeleton, `.github/`, `rust-toolchain.toml`, `deny.toml` | done |
| 0.2 | Wasmtime executor + Windows sandbox | core-agent (Claude) | `feat/core-exec` | `core/crates/nc-exec` | in progress |
| 0.3 | Identity, manifests, lease store | core-agent (Claude) | `feat/core-store` | `proto/`, `core/crates/nc-{identity,protocol,store}` | in progress |
| 0.4 | Tariff profiles, planner, simulator | **Codex** | `feat/codex-tariff` | `core/crates/nc-tariff` only | todo (ready to start) |
| 0.5 | libp2p direct/DCUtR/relay test kit | net-agent (Claude) | `feat/net-p2p` | `core/crates/nc-net`, `spikes/phase0/net/` | review (kit done; two-ISP owner run pending) |
| 0.6 | mDNS + QR pairing | net-agent (Claude) | `feat/net-p2p` | `core/crates/nc-net` | review |
| 0.7 | Coordinator + relay (Docker) | net-agent (Claude) | `feat/net-server` | `server/`, `infra/` | review |
| 0.8 | Flutter shell + bridge | app-agent (Claude) | `feat/app-shell` | `app/`, `core/crates/nc-bridge` | blocked (C: disk space, VS CMake component) |
| 0.9a | Android runtime footprint + foreground-service spike | app-agent (Claude) | `feat/app-android` | `spikes/phase0/android/` | in progress |
| 0.9b | Google Play policy assessment (document) | **Codex** | `feat/codex-mobile-research` | `spikes/phase0/mobile-policy.md` only | todo |
| 0.10 | iOS feasibility (document) | **Codex** | `feat/codex-mobile-research` | `spikes/phase0/ios-feasibility.md` only | todo |

## Rules for Codex work

- Same rules as the Claude agents: [AGENT.md](AGENT.md) §1 (security invariants, work log, hand-back format).
- Codex never edits `main` or files outside its row. It pushes its branch; the coordinator (Claude) reviews,
  runs the checks and merges, then records the review result in WORKLOG.md.
