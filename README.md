# Network Connector

Share CPU-heavy work between Windows computers on your own home network.

One computer (the **controller**, e.g. an i7 laptop) hands out jobs. Other computers
(**workers**, e.g. an i3 laptop) connect to it, pull jobs, run them, and send results back.
The controller computer can run a worker too, so **every machine works at the same time**.

> **What this is not:** it does not merge CPUs or RAM into one big computer. It splits work
> into independent pieces. Each piece runs entirely on one machine, using that machine's
> CPU and memory. A job that needs 20 GB of RAM in one piece will not fit on a 16 GB laptop.
> See [What kind of work benefits](#what-kind-of-work-benefits).

## Features

- **Measured comparisons:** run identical fixed work on each computer and all together,
  compare throughput and efficiency, and share results with participating workers.
  See [Benchmark guide](docs/BENCHMARK.md).
- **Router-less connection:** create a Wi-Fi Direct group or use an existing Windows
  Mobile Hotspot, join from the worker, and find the controller by its pinned identity.
  See [Wi-Fi Direct guide](docs/WIFI_DIRECT.md). Actual adapter support and concurrent
  home Wi-Fi still require tests on the two laptops.

- **Your own code, safely sandboxed (0.3.0):** send WebAssembly programs (Rust, C, Go, …) as `wasm` jobs.
  Each one runs on the helper in a fresh Windows AppContainer + Job Object with no network and no access to
  the owner's files, and is deleted afterwards. See [Sandbox guide](docs/SANDBOX.md) and
  [examples/wasm/primes](examples/wasm/primes/README.md).

- **Desktop windows (GUI)** for both computers, plus a command line (`nc`) for scripting.
- **Secure pairing** with a one-time code (`ABCD-EFGH-JKMN`). The code expires after 10 minutes,
  works once, and locks after 5 wrong tries. No passwords and no hard-coded secrets.
- **Encrypted and authenticated:** HTTPS with the controller's own certificate, pinned by every
  worker. Each worker has its own revocable token. Admin actions are accepted only on the controller
  computer itself.
- **LAN-only:** the controller binds to your private LAN address (never `0.0.0.0`), and refuses
  public addresses. Workers open **no ports at all**; they only connect out.
- **Safe jobs only:** a fixed allowlist of job types with strict input checks. No shell
  commands, no code, and no file paths are accepted over the network.
- **Reliable:** heartbeats, automatic reconnect, job leases (a job from a vanished worker is
  re-queued), timeouts, retries, cancellation, duplicate-submit protection, graceful shutdown,
  and recovery after a controller restart (all state is in SQLite).
- **Keeps the laptop usable:** limits for CPU threads, jobs at once, memory per job (enforced
  by Windows), idle timeout, pause and disable. Jobs run at below-normal priority.
- **Live dashboard:** workers, CPU and RAM graphs, last heartbeat, and queued, running, completed
  and failed jobs.
- **Proof-of-work demo:** shows which computer did which part, how long it took, and checks the
  answer.
- **Safe updates:** workers install only tagged releases from GitHub, run a self-test, and
  roll back automatically if the new version fails. Settings and secrets are never touched.

## Built-in job types

| Kind | What it does | Splits into chunks |
|---|---|---|
| `prime_range` | Counts the primes in a range | yes |
| `number_batch` | Computes the sum of squares over a range of numbers | yes |
| `monte_carlo_pi` | Estimates pi by random sampling | yes |
| `cpu_burn` | Keeps a CPU thread 100% busy for N seconds (load demo) | yes (copies) |
| `hash_files` | Hashes files whose bytes are sent inside the job | one job per file |
| `image_resize` | Resizes an image sent inside the job (protected against decompression bombs) | one job per file |
| `python_function` | Runs one function from a fixed list: `fibonacci`, `collatz_longest`, `nqueens` | – |
| `benchmark` | Fixed SHA-256 chain or segmented prime-sieve work for comparisons | yes |
| `wasm` | Runs your WebAssembly module (stdin → stdout) in the helper's sandbox | one job per input |

To add your own job type, see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#adding-a-job-type).

## Quick start

| Step | Guide |
|---|---|
| 1. Set up the main computer (i7) | [docs/SETUP_I7.md](docs/SETUP_I7.md) |
| 2. Set up the helper computer (i3) | [docs/SETUP_I3.md](docs/SETUP_I3.md) |
| 3. Pair them | [docs/PAIRING.md](docs/PAIRING.md) |
| 4. Submit your first job and run the demo | [docs/FIRST_JOB.md](docs/FIRST_JOB.md) |
| Update a worker | [docs/UPDATING.md](docs/UPDATING.md) |
| Remove or revoke a worker | [docs/REVOKING.md](docs/REVOKING.md) |
| Firewall and connection problems | [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) |
| Security model | [docs/SECURITY.md](docs/SECURITY.md) |
| Settings reference | [docs/CONFIGURATION.md](docs/CONFIGURATION.md) |
| Publishing this repo to GitHub | [docs/PUBLISHING.md](docs/PUBLISHING.md) |

Shortest path, in PowerShell, on the i7 inside the cloned folder:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\setup-controller.ps1
.\.venv\Scripts\nc.exe gui
```

On the i3:

```powershell
Invoke-WebRequest https://raw.githubusercontent.com/NavodCaldera/Network-Connector/main/scripts/setup-worker.ps1 -OutFile setup-worker.ps1
powershell -ExecutionPolicy Bypass -File .\setup-worker.ps1 -RepoUrl https://github.com/NavodCaldera/Network-Connector.git
```

## What kind of work benefits

Good fits are tasks made of many independent pieces, each taking at least a few seconds of
CPU time and needing little data: batch image processing, simulations, parameter sweeps,
frame-by-frame rendering, number crunching, and processing many separate files.

Poor fits:

- one huge task that cannot be split;
- anything needing more RAM than a single machine has;
- GPU work;
- interactive apps and games;
- tasks where sending the data over Wi-Fi takes longer than computing it.

With an i7-13620H (16 threads) plus a typical i3 (4–8 threads), expect roughly **20–40% more
throughput** on suitable work, not double.

## Project layout

```
network_connector/
  controller.py   HTTPS API, pairing, lease reaper, server runtime
  store.py        SQLite state: workers, batches, jobs, attempts
  worker.py       outbound-only worker agent (heartbeat, claim, limits, reconnect)
  jobrunner.py    child process that runs one allowlisted job
  jobs.py         the job allowlist + validation
  sandbox.py      per-job AppContainer + Job Object sandbox for wasm jobs, sweep, Stop
  sandbox_runner.py  runs one Wasm module inside the sandbox (wasmtime, WASI stdio only)
  modules.py      Wasm module size/hash/import checks and the helper's module cache
  client.py       pinned-TLS clients, pairing handshake
  security.py     tokens, pairing codes and proofs, certificates, file ACLs
  winlimits.py    Windows Job Object memory cap + low priority
  updater.py      release-tag updates with self-test and rollback
  gui.py          controller window        worker_gui.py  worker window
  cli.py          `nc` command line        demo.py        proof-of-work demo
scripts/          setup-controller.ps1, setup-worker.ps1, add/remove-firewall-rule.ps1
docs/             all guides        examples/   configuration examples
tests/            unit, API-security and real end-to-end tests
```

## Development

```powershell
py -3 -m venv .venv
.\.venv\Scripts\python.exe -m pip install -e ".[test]"
.\.venv\Scripts\python.exe -m pytest
```

Requirements: Windows 10/11 (other OSes work, minus the Windows memory cap), and Python 3.10+.
Dependencies: fastapi, uvicorn, httpx, psutil, pillow, cryptography.

## Assumptions

- The default port is 8765/TCP. Every address and port is configurable.
- Both computers are on the same trusted home network (same subnet).
- Pairing codes are about 59 bits, live 10 minutes, and are kept in memory only.
- Job inputs are limited to 8 MB per file and 24 MB per request; results to 1 MB.
- The only step that needs administrator rights is creating the firewall rule on the i7.
- A worker installed from a ZIP (without Git) works, but cannot self-update.

## License

MIT, see [LICENSE](LICENSE).
