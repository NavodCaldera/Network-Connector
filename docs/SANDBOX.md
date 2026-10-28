# The Wasm sandbox

A requester can send its own code to helpers, but only as a **WebAssembly (Wasm) job** (`kind: "wasm"`). On the helper,
every such job runs in a **fresh sandbox that is created for that job and deleted when it ends**. This page explains what
the sandbox stops, what it doesn't, how to set it up on a helper, and how to write a job.

Built-in job kinds (`prime_range`, `benchmark`, …) still run without the sandbox, but only in **closed groups**
(computers you paired with a code). **Open groups run `wasm` jobs only.**

## Set up a helper (once, no admin rights)

```powershell
nc sandbox setup            # creates the runtime folder and its one read grant
nc sandbox status --check   # runs a tiny module in a fresh sandbox
```

`setup` creates `%LOCALAPPDATA%\NetworkConnector-runtime` holding only the sandbox runner and a copy of wasmtime. It
gives Windows app containers read and execute access to **that folder only**
(`ALL APPLICATION PACKAGES:(OI)(CI)(RX)`). Nothing else is changed, and nothing is ever granted on
`%LOCALAPPDATA%\NetworkConnector`, which holds your tokens and keys. `nc sandbox setup --remove` takes the grant and
the folder away again.

Python must be installed for all users (in `C:\Program Files`), which Windows already lets app containers read.
`nc sandbox status` tells you exactly what is missing. A helper whose sandbox is not ready **doesn't take `wasm` jobs**.
If one arrives anyway, the helper refuses it with that reason. It never runs it with weaker isolation.

## What is isolated

Every job gets four layers. Each one is meant to hold even if another fails.

| Layer | What it does |
|---|---|
| **wasmtime** (pinned `wasmtime==50.0.1`) | The module only sees **stdin, stdout, stderr, its arguments, clocks and random numbers** (WASI preview 1). No folders are opened for it, it gets no environment variables, and there are no sockets. A module that imports anything else (`path_open`, `fd_readdir`, `sock_*`, or a non-WASI function) is refused before it starts, by the helper and again inside the sandbox. Memory is capped by the job's `memory_mb`, the stack by 512 KB, and fuel is used when the requester sets `limits.fuel`. |
| **AppContainer** with no capabilities | The runner process (Python + wasmtime) runs as a new, empty Windows app container: **no network** and **no access to your files**. A new profile is created for every job and deleted afterwards. |
| **Job Object** | Memory cap (the job's limit plus about 160 MB for the runtime; the process is killed the moment it hits it), **one process only** (no child processes, also blocked by the child-process policy), killed if the helper exits, no access to the desktop, clipboard or other windows, below-normal priority. |
| **The helper's own clock** | The time limit is enforced by the helper killing the whole job, so a module that sleeps or waits can't run longer. Output is capped too: stdout at 1 MB, stderr at 64 KB. |

After every job, on every path (success, failure, time limit, memory limit, cancel, **Stop**, crash), the helper kills the
Job Object and deletes the AppContainer profile and its folder. The in-memory copy of the input is wiped.
When the helper starts, a **sweep** removes any sandbox left behind by a crash or power loss. Sandboxes of a helper that is
still running are left alone. `nc sandbox sweep` runs it by hand.

**Stop** (tray or window, see the helper settings) kills every running sandbox within a second and hands the jobs back
to the requester, which queues them again.

### What the tests show

These checks run in `tests/test_sandbox.py` on every test run, with a real AppContainer and Job Object. The *native*
checks start a plain Python program in the same container and job, as if code had already escaped Wasm:

| Attempt | Result |
|---|---|
| Wasm: open a file, list a folder, open a socket, call a host function | Refused before linking |
| Wasm: read environment variables | None exist (0 variables, nothing pre-opened) |
| Wasm: grow memory past `memory_mb` | `memory.grow` fails at the limit |
| Wasm: endless loop, or sleep in a host call | Killed at the time limit |
| Wasm: deep recursion | Stack overflow trap |
| Wasm: flood stdout or stderr | Cut off at 1 MB and 64 KB |
| Native: connect to the requester (loopback or LAN), to 8.8.8.8 or 1.1.1.1, resolve a name | All fail; the test listeners receive nothing |
| Native: read your profile, the NetworkConnector folder (tokens) or the project files | Access denied |
| Native: write to your profile, temp folders, the data folder or the runtime folder | Access denied |
| Native: start `python` or `cmd` | Blocked ("process creation has been blocked") |
| Native: allocate 2 GB with a 32 MB limit | Killed by the Job Object |
| Native: plant a folder junction in its own folder to trick the cleanup | Windows refuses it; the target is untouched |
| Stop during a run | Gone in under 1 s, profile deleted |
| Helper killed mid-job | The sandbox process dies with it; the start-up sweep removes the profile |
| 100 jobs with mixed endings | 0 profiles or folders left |

## Honest limits

No sandbox can promise zero risk. These are the known limits:

- **Escaped code could open a listening port, but nothing can reach it.** Binding succeeds inside an app container; no
  connection or packet from outside, or from the helper itself, ever arrives.
- **Escaped code could read world-readable Windows files**, for example `C:\Windows\win.ini` or files under
  `C:\Program Files`. It can't read your user files, your profile or the NetworkConnector data folder.
- **Job inputs and outputs are visible to the helper's computer.** Don't send private data to people you don't trust.
- **A helper can return wrong answers.** The sandbox protects the helper, not the requester. Result checks (redundancy)
  are a separate feature.
- The stricter "LPAC" app-container mode would also hide world-readable system files, but Python doesn't start under it
  yet, so it isn't used.
- A bug in wasmtime or in Windows could in theory break a layer. That is why there are several layers, why wasmtime is
  pinned and updated quickly, and why the owner can stop everything at once.
- Start-up costs about 0.15–0.3 s per job (a new profile and process each time). Make chunks at least a few seconds long.

## Writing a job

A job is a **WASI preview 1 command**: a `.wasm` file with a `_start` function that reads its input from **stdin** and
writes its answer to **stdout**. Most languages compile to this directly:

| Language | Target |
|---|---|
| Rust | `cargo build --release --target wasm32-wasip1` |
| C / C++ | `wasi-sdk` (`clang --target=wasm32-wasip1`) |
| Go | `GOOS=wasip1 GOARCH=wasm go build` |
| Zig | `zig build-exe -target wasm32-wasi` |
| AssemblyScript | `asc` with the WASI shim |

Rules:

- Read everything from stdin and write the result to stdout. The exit code is returned to the requester.
- **No files.** C code that calls `fopen` (and Rust code that uses `std::fs`) imports `path_open` and is refused. Put
  the data in the input instead.
- **No network**, no environment variables, no threads.
- For the same input, print the same output (don't print the time or random numbers). Then two helpers can be compared
  byte for byte.
- Sizes: module ≤ 10 MB, input ≤ 8 MB, stdout ≤ 1 MB, stderr ≤ 64 KB (cut off), at most 16 arguments.
- Limits per job: `memory_mb` (1–4096, default 256, and at most the helper's "RAM per job"), `seconds` (1–3600, default
  60), and optionally `fuel` (an instruction budget; about 8 % slower, gives `fuel_used` in the result).

The full example, in Rust and in WebAssembly text, is in [examples/wasm/primes](../examples/wasm/primes/README.md).

### Test it on your own computer

```powershell
nc sandbox run my_job.wasm --input data.txt --memory-mb 64 --seconds 30
```

This runs the module in the same sandbox a helper would use, prints its stdout, and shows the outcome on stderr.

### Send it to helpers

```powershell
nc module upload my_job.wasm                       # prints the sha256; the module is sent to each helper once
nc job submit wasm -p module_sha256=<sha256> --input chunk1.txt --input chunk2.txt --wait
nc job submit wasm --module my_job.wasm --input-text "0 1000000" --arg=--verbose --wait
```

Each `--input` becomes one job. A `wasm` job is never split by the requester: send one job per chunk, each with its own
input. Over the API:

```
POST /admin/modules        {"module_b64": "<base64 of the .wasm>", "label": "primes"}   -> {"sha256", "size", "created"}
GET  /admin/modules
POST /admin/jobs/batch     {"jobs": [{"kind": "wasm", "params": {
                              "module_sha256": "<hex>", "input_b64": "<base64 ≤ 8 MB>", "args": ["…"],
                              "limits": {"memory_mb": 64, "seconds": 60, "fuel": 1000000000}}}]}
```

Result of a finished job: `{"stdout_b64", "exit_code", "fuel_used", "cpu_seconds", "stderr_b64"}`.
A job fails (and is not retried) when the module is refused or invalid, traps, runs out of fuel or memory, or writes too
much. A time-limit kill or a sandbox start-up problem is retried, up to the job's maximum attempts.

## How modules reach helpers

- The requester stores uploaded modules in its database, keyed by SHA-256, after checking size, imports and validity.
- A helper downloads a module from `GET /worker/modules/<sha256>` **only while it runs a job that uses it**. It checks
  the size, the hash and the imports, and keeps it in a per-user cache (`<worker folder>\modules`, 200 MB, least
  recently used are removed first). A module is only a file: it is never run outside the sandbox and never made executable.
- The helper shows the module hash of each running job, so a module can be recognised and refused everywhere.

## Troubleshooting

| Message | Fix |
|---|---|
| `The sandbox is not set up yet. Run: nc sandbox setup` | Run it once on the helper. |
| `Python at … is not readable from an app container` | Install Python for all users, or use the installer's "for all users" option. |
| `The sandbox runtime folder … overlaps the Network Connector data folder` | `NC_RUNTIME_HOME` must point to a separate folder. |
| `module imports functions the sandbox does not offer: wasi_snapshot_preview1.path_open` | The program opens files. Pass the data through stdin instead. |
| `wasm timeout` | Raise `limits.seconds` or send smaller chunks. |
| `wasm memory` | Raise `limits.memory_mb` (up to the helper's limit). |
