# Network Connector

A platform where any capable device (PCs, Raspberry Pi, Android, iOS) can give or ask for compute, on the local
network and across the internet, with the device owner always in control. It is built with a Rust core, a Flutter
app and WebAssembly jobs.

**Status:** being rebuilt. Only the plan exists so far; no code has been written yet.

- [PLAN.md](PLAN.md): design, technology stack and phases
- [AGENT.md](AGENT.md): rules, security invariants and how the three agents split the work
- [HANDOFF.md](HANDOFF.md): current state
- [WORKLOG.md](WORKLOG.md): what was done, when and by whom

The previous Python/Windows app (Wi-Fi Direct, jobs, sandbox, benchmarks, GUI) is kept on branch
`legacy/python-v0.3`:

```powershell
git switch legacy/python-v0.3
```
