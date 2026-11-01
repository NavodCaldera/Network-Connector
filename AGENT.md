# Agents for the Network Connector platform (v1)

> These rules replace the Python 0.x agent rules. The old rules, agents and code are kept on branch
> `legacy/python-v0.3`.

Three implementation agents carry out [PLAN.md](PLAN.md). A **coordinator** (the main Claude Code session) assigns
tasks, reviews hand-backs and merges. Definitions are in [.claude/agents/](.claude/agents/).

| Agent | File | Owns |
|---|---|---|
| **Core agent** | `.claude/agents/core-agent.md` | Everything that runs on a device apart from networking and UI: protocol types, identity, policy, tariff, store/leases, blobs, **executor + sandbox**, agent supervisor, CLI |
| **Net agent** | `.claude/agents/net-agent.md` | Getting devices talking: libp2p transport, discovery, pairing, NAT traversal, relay, **control plane server**, hosting/infra |
| **App agent** | `.claude/agents/app-agent.md` | What people touch and how it ships: Flutter app, `flutter_rust_bridge`, Android/iOS/desktop platform adapters, CI/CD, packaging, signing, updates, observability |

---

## 1. Rules for all agents

### Project facts

- Rust stable (pinned in `rust-toolchain.toml`), Cargo workspaces in `core/` and `server/`. Flutter stable in `app/`.
- Before every hand-back: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` in each
  workspace you touched; `flutter analyze` and `flutter test` if you touched `app/`.
- Wire formats live **only** in `proto/`. Never hand-write a second copy of a message type.
- Style: small modules, `thiserror` for library errors, `anyhow` only in binaries, no `unwrap()`/`expect()` outside
  tests and proven-infallible cases, `tracing` for logs. Match the surrounding code.
- The legacy Python app is **not on `main`**. It is kept on branch `legacy/python-v0.3` as the reference: read it with
  `git show legacy/python-v0.3:network_connector/<file>` to port behaviour.

### Security invariants (hard rules; see PLAN.md §5)

1. **The device has the final say.** Owner policy and tariff rules are enforced on the device. A lease or transfer
   that breaks them is refused, whatever the coordinator says.
2. **Discovery is not authorisation.** A peer is trusted only after pairing (QR/short auth string over a signed
   challenge) or a coordinator-issued certificate chained to its device root key. Authenticate keys, not names.
3. **Other people's code runs only as Wasm in the executor process**, with: module hash + publisher signature
   checked against the manifest; fuel or epoch limit **and** an external wall-clock watchdog; memory, stack, output
   and disk caps; no ambient filesystem, network or environment; only the narrow host API in `nc-exec`.
   On Windows also AppContainer (no capabilities) + Job Object (memory cap, one process, kill-on-close); on Linux
   namespaces + seccomp + cgroups. **If any required layer can't be set up, refuse the job. Never fall back to
   weaker isolation.**
4. **Every sandbox is torn down after its job** on every path (success, Stop, timeout, crash). Start-up sweeps
   leftovers.
5. **Stop halts every job within 1 s.**
6. All peer and relay traffic is end-to-end encrypted. Relays require short-lived tokens and enforce per-account and
   per-device quotas. No unauthenticated public relay.
7. Server endpoints are authenticated, input is schema-validated and size-capped, and errors use one format.
   No listening socket binds wider than it must.
8. Commit of a task result is **idempotent and attempt-checked**. Late or duplicate results never complete a task
   twice.
9. No downloaded native code, ever. No self-updating mobile binaries. Desktop updates are signed.
10. **Never log** private keys, tokens, pairing codes, job inputs/outputs or sensitive paths. Telemetry is opt-in
    and scrubbed.
11. No `unsafe` outside `nc-exec` and `nc-identity` platform adapters, and each `unsafe` block has a `// SAFETY:`
    comment. `cargo deny` must pass (licences + advisories).

### Work log (required)

Every change gets an entry at the top of [WORKLOG.md](WORKLOG.md) **in the same commit** (format at the top of that
file). Record failures and things not checked as plainly as successes. Never put secrets or job contents in it.
A hand-back is not complete until WORKLOG.md is updated.

### Working method

- Work on your own branch off `main` (`feat/core-*`, `feat/net-*`, `feat/app-*`) in small commits. Push your
  branch; **never push or merge into `main`**. The coordinator merges.
- Stay inside your ownership area (§3). Need something elsewhere? Put it under **Requests** in your hand-back.
- Tests for every behaviour change. Hardware-, network- or store-policy facts must come from **real output**. When
  you need the owner's machines, phones or ISPs, write a script and say exactly what to run. **Never invent results.**
- If a plan step proves wrong (a spike fails), stop and report with evidence. Never weaken a security invariant to
  make something work.
- Installing toolchains or crates is fine. Creating cloud resources that cost money needs the owner's approval
  through the coordinator.

### Hand-back format

```
## Done
- <change> — <files> — <test that covers it>
## Verified
- cargo/flutter: <real results, failures pasted>
- manual: <what was run, output excerpt>
## Security invariants checked
- <numbers from §1, and how>
## Not done / deviations from PLAN.md
## Requests to other agents / coordinator
## WORKLOG.md entry added (required)
```

---

## 2. Phase 0 task split (PLAN.md §8)

| Order | Core agent | Net agent | App agent |
|---|---|---|---|
| 1 | — | — | **0.1** toolchain, workspaces, `rust-toolchain.toml`, CI (Windows + Linux), `cargo deny` config |
| 2 | **0.2** `nc-exec` Wasmtime executor + Windows sandbox + malicious-job tests | **0.6** `nc-net` mDNS + QR pairing | **0.8** Flutter shell + `nc-bridge` |
| 3 | **0.3** `nc-identity`, `nc-protocol` manifests, `nc-store` lease state machine | **0.5** libp2p direct/DCUtR/relay test kit for two ISPs | **0.9** Android footprint + foreground-service spike |
| 4 | **0.4** `nc-tariff` profiles, planner, simulator | **0.7** `nc-coordinator` + `nc-relay` minimal, Docker first | **0.10** iOS feasibility notes |

Task 0.1 goes first because the others need the workspace. After that the three columns run in parallel.

**Codex** (an outside agent the owner runs) has taken 0.4, 0.9b (Play policy document) and 0.10. The live assignment
of every task is in [TASKS.md](TASKS.md); it overrides this table. Codex follows the same rules and hand-back format,
and the coordinator reviews its branches before merging.

---

## 3. Ownership and shared contracts

| Path | Owner |
|---|---|
| `proto/` | **Core** (Net and App send requests) |
| `core/crates/nc-{protocol,identity,policy,tariff,store,blob,exec,agent,cli}` | Core |
| `core/crates/nc-net`, `server/`, `infra/` | Net |
| `core/crates/nc-bridge`, `app/`, `.github/`, packaging, `rust-toolchain.toml`, `deny.toml` | App |
| Root `Cargo.toml` of each workspace | App (others request members) |
| `docs/` | each agent writes its own topics |
| `spikes/phase0/REPORT.md` | each agent writes its own sections |

Shared contracts (changes need a coordinator-approved request):
- `nc-agent` exposes a typed API (`start_helper_session(policy)`, `stop_helper_session()`, `submit_job(manifest)`,
  `watch_status()`, `approve_pairing(request)`, `update_network_budget(profile)`). `nc-bridge` wraps it 1:1; job
  payloads and high-frequency events do not go through Flutter.
- `nc-net` gives Core an authenticated stream API keyed by **peer device key**, never by address.
- The coordinator speaks only `proto/` messages over HTTPS/WSS.

---

## 4. How to launch them

```
Use the app-agent to do Phase 0 task 0.1 of PLAN.md.
```
then, in parallel:
```
Use the core-agent to do Phase 0 tasks 0.2–0.4.
Use the net-agent to do Phase 0 tasks 0.5–0.7.
Use the app-agent to do Phase 0 tasks 0.8–0.10.
```
The coordinator reviews each hand-back, runs the checks, merges into `main`, and starts the next step.
