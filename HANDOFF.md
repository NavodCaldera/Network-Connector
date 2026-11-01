# Handoff: current state

_Last updated 2026-11-01._

## Where things are

- `main` contains the **new platform plan only**: [PLAN.md](PLAN.md), [AGENT.md](AGENT.md), and three agents in
  [.claude/agents/](.claude/agents/) (`core-agent`, `net-agent`, `app-agent`).
- The legacy Python/Windows app (0.x: Wi-Fi Direct, native P2P mode, jobs, sandbox, benchmarks, Tkinter GUI) was
  removed from `main` and is kept unchanged on branch **`legacy/python-v0.3`**. It is the reference for porting
  behaviour (`git show legacy/python-v0.3:<path>`).
- Other old branches (`claude/great-davinci-bch39n`, `simple-hello`, `release/0.2.0`, `feat/*`, `integrate/0.3.0`) are
  history only.

## Next step

Phase 0 (PLAN.md §8), starting with task 0.1 by the app-agent: install the Rust and Flutter toolchains, create the
Cargo workspaces and set up CI. Then core, net and app agents run tasks 0.2–0.10 in parallel (AGENT.md §2).

## Known facts about this PC

- Windows 11 Pro. Git, GitHub CLI and Docker are installed. Rust and Flutter are **not** installed yet.
- iOS builds need a Mac; none is available yet.
