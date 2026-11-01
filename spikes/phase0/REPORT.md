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

## 0.3 Identity, manifests, lease store (core-agent)

## 0.4 Tariff profiles, planner, simulator

## 0.5 libp2p across two ISPs (net-agent)

## 0.6 mDNS + QR pairing (net-agent)

## 0.7 Minimal coordinator + relay (net-agent)

## 0.8 Flutter shell + nc-bridge (app-agent)

## 0.9 Android footprint + foreground service (app-agent)

## 0.10 iOS feasibility (app-agent)
