# Plan: open, one-click, sandboxed performance sharing

This plan has two parts:

| Part | Version | What | Status |
|---|---|---|---|
| **Part 1** (§1–§11) | 0.2.0 | Wi-Fi Direct link between the i7 and the i3, plus benchmarks that compare their performance | **Software implemented; hardware acceptance pending** |
| **Part 2** (§12–§22) | 0.3.0 | Any PC can ask for or give performance, joining takes one click with no pairing codes, helpers accept automatically or with one button, and requester code runs in a **WebAssembly sandbox** that is deleted after every job | **Planned, not started** |

Part 1 status: at the user's request, Phase 2 software was built before receiving the Phase 0 laptop reports.
Modes A and B use runtime capability checks; no hardware support, concurrency or performance result is
assumed. Automated tests use isolated TLS clusters and mocked Windows networking. Record real-laptop
evidence in [spikes/REPORT.md](spikes/REPORT.md) and complete §9 before claiming hardware acceptance.

Part 2 builds on Part 1 and starts after Part 1's code is committed. It does not need Part 1's hardware
acceptance to start. Owners: two implementation agents, see [AGENT.md](AGENT.md).

---

# Part 1 — 0.2.0: Wi-Fi Direct link + shared performance benchmarks

## 1. Goal

1. **Connect without a router.** The i7 (controller) and the i3 (worker) talk over a direct
   Wi-Fi link, while the i7 can keep its normal Wi-Fi/Ethernet at the same time.
2. **Share and compare performance on one piece of work.** Run the *same* fixed workload as
   *i7 alone*, *i3 alone* and *all machines together*, and show both computers the results: time,
   throughput, speedup, efficiency, each machine's share, CPU and memory use, and link overhead.

Everything else stays as it is: the same jobs, pairing, pinned TLS, outbound-only workers and
LAN-only controller.

### Non-goals

- Sharing internet over the link (no ICS/routing).
- Merging CPUs or RAM. Each chunk still runs entirely on one machine.
- Native Wi-Fi Direct device pairing (the Windows "Add a device" ceremony). It is a stretch goal only (§4.1, mode C).
- Updating workers over the direct link. `nc worker update` needs internet and must fail clearly offline.

## 2. Where the code is today (what this plan builds on)

| Area | Today | File |
|---|---|---|
| Bind address | One `bind_host`, auto-detected from the default route. Must be private, loopback or link-local; never `0.0.0.0` | `config.py:101-138`, `controller.py:415-447` |
| Address list in GUI | `list_private_ips()`, which **excludes 169.254.x** | `config.py:116-124`, `gui.py:279` |
| Admin access | Source IP must be in `{127.0.0.1, ::1, bind_host}`, plus `X-Admin-Token` | `controller.py:190-195, 428` |
| Worker → controller | Fixed `controller_host:port` in `worker.json`. The **cert is pinned, not the address**, so changing the host is safe | `worker.py:130-136`, `client.py:31-38` |
| Pairing | TOFU fetch of the cert, then HMAC proofs bound to the cert fingerprint, both ways | `client.py:125-160`, `controller.py:116-147, 210-222` |
| Discovery | **None.** The address is typed by hand | `worker_gui.py:45-63`, `cli.py:197-210` |
| Firewall | One rule: TCP 8765, **Private profile only**, LocalSubnet | `scripts/add-firewall-rule.ps1` |
| Per-job data | `duration` (wall time incl. Python start-up), `worker_name`, `started`/`finished`. **No CPU time or peak memory.** Retries overwrite `started`/`duration`; `attempts` has no duration | `worker.py:349-356`, `store.py:175-231` |
| Batch stats | `by_worker{chunks, busy_seconds}`, plus `wall_seconds` = max(finished) − min(started) | `store.py:362-380` |
| CPU history | In memory, 90 samples per worker, lost on restart, not linked to jobs | `controller.py:160, 228-229` |
| Demo | Hard-coded primes < 100M, client-side timer, text output only | `demo.py` |
| Job caps | Checked on the **whole** job before splitting: primes ≤ 400M wide, monte carlo ≤ 200M, so a fixed-work job longer than about 1 min is impossible | `jobs.py:109-176, 402` |
| Schema | `create table if not exists`, **no migrations** | `store.py:18-36` |

### Problems found while testing on 2026-10-25 (fixed in Phase 1)

- The GUI **Start controller** fails with "port already in use" when a CLI controller is running, and cannot attach to it.
- There is **no cancel-all**. Clearing about 3,000 test jobs needed a shell loop over 90 batches.
- The local worker re-pairs, rotating its token, whenever `bind_host` changes (`localworker.py:23`). Adding a Wi-Fi Direct address would trigger this.
- The demo pinned to one worker gives no comparison. The user has to run it three times and compare by hand.

## 3. Phases at a glance

| Phase | What | Owner | Exit gate |
|---|---|---|---|
| 0 | Hardware and API spike: can these two laptops do Wi-Fi Direct from Python? | Link agent | Spike report with a go / fallback decision |
| 1 | Fixes found in testing: attach to a running controller, cancel-all, local worker over loopback | Both | Tests green, manual check |
| 2 | Wi-Fi Direct link: group, multi-address bind, discovery, firewall, GUI/CLI | Link agent | i3 pairs and runs the demo over the direct link with no router |
| 3 | Measurement: per-job CPU time and peak memory, attempt durations, persisted samples, migrations | Perf agent | New fields visible in `nc job status` |
| 4 | Benchmark: fixed-work kind, server-side orchestrator, API, CLI, GUI tab, shared view on the worker, export | Perf agent | 5-minute benchmark comparing i7 alone, i3 alone and both |
| 5 | Integration, docs, release 0.2.0 | Both, coordinator merges | Acceptance list in §9 passes on real hardware |

Phases 2 and 3 run **in parallel**. Phase 4 depends on Phase 3. The final acceptance in Phase 5 needs both.

---

## 4. Phase 0 — Spike (Link agent, about 1 day, throwaway code in `spikes/`)

The questions must be answered on **the real i7 and i3** before Phase 2 starts.

1. **Adapter support** (both machines): run `netsh wlan show drivers` and record "Hosted network supported",
   "Wireless Display Supported (Wi-Fi Driver)" and the radio types. Run `netsh wlan show wirelesscapabilities` and record
   the "Wi-Fi Direct" lines (concurrent operation, and whether the GO and client roles are supported).
2. **Legacy Wi-Fi Direct group from Python:** with `winrt-Windows.Devices.WiFiDirect` (pywinrt),
   create `WiFiDirectAdvertisementPublisher` with
   `Advertisement.LegacySettings.IsEnabled = True`, `Ssid = "DIRECT-NC-<4 chars>"`, and a random
   `PasswordCredential`, then start it. Check:
   - the i3 sees the SSID and can join it with `netsh wlan add profile` + `netsh wlan connect`;
   - the IP addresses both ends get (expected: GO `192.168.137.1/24`, client `192.168.137.x`) and the
     Windows adapter name (expected "Microsoft Wi-Fi Direct Virtual Adapter #n");
   - whether the i7 stays on its home Wi-Fi at the same time (concurrency);
   - the Windows network category of the new link (expected Public or Unidentified);
   - whether a Win32/desktop Python process may run the publisher without package identity, and
     whether it needs admin rights;
   - whether the group stops when the publisher object is garbage-collected or the process exits.
3. **Gateway detection on the i3:** is there a default gateway on the direct link, and how can it be read
   without admin rights? Options: `GetAdaptersAddresses` via ctypes, or `Get-NetIPConfiguration`.
4. **Throughput and latency:** ping RTT and a 10 MB transfer, i7 ↔ i3, to give the benchmark a link
   baseline.
5. **Fallback check:** the same tests with **Windows Mobile Hotspot** (Settings → Network) when there is no internet
   connection to share, because the tethering API often refuses in that case.

**Decision recorded in `spikes/REPORT.md`:**

- **Mode A (preferred):** a legacy Wi-Fi Direct group started by the controller app, which the i3 joins as a normal
  WPA2 network. No Windows pairing prompt.
- **Mode B (fallback):** Mobile Hotspot started manually, or by `NetworkOperatorTetheringManager` when it works.
  Same addressing, so everything after the link comes up is identical.
- **Mode C (stretch, not planned):** native P2P pairing through `WiFiDirectDevice`.

If neither A nor B works on this hardware, stop and report. The rest of the plan still applies to
an ordinary LAN, and Phase 3/4 do not depend on Wi-Fi Direct.

---

## 5. Phase 1 — Fixes found in testing

### 5.1 Attach to a running controller (Link agent) — `gui.py`

- On **Start controller**, first try `admin_client_from_disk()` + `health()`. If a controller is already
  running, attach to it: set the status to "Controller running (started elsewhere) at …", enable the tabs, and don't start a second runtime.
- **Stop controller** on an attached controller calls `/admin/shutdown` after a confirmation.
- Turn the "port in use" error into: "Port 8765 is used by another program (PID n, name x)". Find the
  PID with `psutil.net_connections`.

### 5.2 Cancel all (Perf agent) — `store.py`, `controller.py`, `client.py`, `cli.py`, `gui.py`

- `Store.cancel_all(statuses=("queued","running"))`: one transaction. Queued jobs become `cancelled`;
  running jobs get `cancel_requested=1`, reusing the existing per-job path.
- `POST /admin/jobs/cancel-all` (admin only) returns `{cancelled, cancelling}`.
- `AdminClient.cancel_all()`. CLI: `nc job cancel-all [--yes]`. GUI: a **Cancel all** button on the Jobs tab with
  a confirmation that shows the counts.

### 5.3 Local worker over loopback (Link agent) — `localworker.py`, `controller.py`

- The controller **always also binds `127.0.0.1`** (see §6.2). The i7's own worker and the admin client
  connect through `127.0.0.1`. A change of the LAN or Wi-Fi Direct address then no longer re-pairs the local worker.
- `ensure_paired` compares against loopback and port only.

---

## 6. Phase 2 — Wi-Fi Direct link (Link agent)

### 6.1 New module `network_connector/wifidirect.py`

```
class DirectGroup:            # controller side (group owner)
    start(ssid=None, passphrase=None) -> GroupInfo   # mode A, or detects a running hotspot (mode B)
    stop()
    status() -> GroupInfo     # {mode, ssid, passphrase, interface, ip, subnet, peers[], running}

def find_direct_interface() -> InterfaceInfo | None   # by adapter description / 192.168.137.0/24
def join(ssid, passphrase) -> InterfaceInfo           # worker side: netsh wlan profile + connect
def leave(ssid)
def candidate_controllers(iface) -> list[str]         # [gateway, <subnet>.1]
def probe(host, port, timeout=2) -> str | None        # cert SHA-256 via existing fetch_server_cert
```

- **WinRT is an optional dependency**, added as `pyproject.toml` `[project.optional-dependencies] wifidirect =
  ["winrt-runtime", "winrt-Windows.Devices.WiFiDirect", "winrt-Windows.Security.Credentials",
  "winrt-Windows.Foundation"]`. Exact pins are chosen in Phase 0. Import it lazily. Without it, the GUI shows
  "Wi-Fi Direct support not installed" and mode B still works.
- **The publisher lives in the controller process** on its own thread, and is kept referenced. Closing the
  controller stops the group.
- **The passphrase** is 16 random characters from `secrets`, regenerated on every start unless the user pins it. It is stored
  only in memory or `controller.json` (ACL-protected), never logged.
- **netsh calls** use argument lists (`subprocess.run([...])`, no shell). SSID and passphrase are checked
  against a strict pattern. The profile XML is written to a temp file with a protected ACL and deleted after
  `add profile`.

### 6.2 Several bind addresses — `config.py`, `controller.py`

- `ControllerConfig.bind_hosts: list[str]`. The old `bind_host` is kept as the **primary** address, for
  compatibility and so old config files still load. Each entry goes through `validate_bind_host`. Still **never `0.0.0.0`/`::`**.
- `ControllerRuntime` runs one uvicorn `Server` per address, sharing the same app, TLS cert and
  store. It also always binds `127.0.0.1`.
- `add_listener(host)` and `remove_listener(host)` at runtime, so turning on Wi-Fi Direct after the controller started works without a restart.
- `admin_hosts` = loopback + **the controller's own** bind addresses. Never the peer or the subnet.
- The certificate SANs are irrelevant (no hostname check). Do **not** regenerate the cert, because that would force every worker to re-pair.
- `list_private_ips()` returns labelled entries (`"192.168.137.1 — Wi-Fi Direct"`) and includes
  link-local addresses only when they belong to the direct adapter.

### 6.3 Discovery and "the address follows the certificate" — `worker.py`, `client.py`, `config.py`

There is no new listening socket on the worker. Workers stay outbound-only.

1. `WorkerConfig` gains `controller_fingerprint` (SHA-256 of the pinned cert, filled in at pairing) and
   `known_hosts: list[str]` (the last good addresses, at most 4).
2. If the worker can't reach `controller_host` for 3 backoff rounds, it tries `known_hosts` plus
   `wifidirect.candidate_controllers()` with `probe()`. It switches to an address **only if the fingerprint
   equals `controller_fingerprint`**, then saves the address. Every request still uses the pinned TLS context, so a wrong
   machine at that IP can never be trusted.
3. **Pairing from the worker GUI:** "Find controller" probes the candidates and shows `IP — fingerprint
   AB12…`. The controller's Pair tab shows the same short fingerprint, so the user can compare the two. The pairing code still
   protects the pairing through the HMAC proofs.

### 6.4 Firewall — `scripts/add-firewall-rule.ps1`, new `-WiFiDirect` switch

- The Wi-Fi Direct link is usually Public/Unidentified, so the existing Private rule does not cover it. Add a **second, narrow** rule:
  `"Network Connector Controller (Wi-Fi Direct)"`, TCP `$Port`, `-Profile Any`,
  `-LocalAddress <GO ip>`, `-RemoteAddress <GO subnet>/24`. Do **not** widen the existing rule to Public.
- `remove-firewall-rule.ps1` removes both rules. `setup-controller.ps1` and TROUBLESHOOTING list both.
- The GUI has an **Allow through firewall** button that runs the script elevated
  (`Start-Process powershell -Verb RunAs`) with the detected address.

### 6.5 User interface

**Controller GUI, new "Wi-Fi Direct" tab:**
- Start/Stop group, plus the mode.
- SSID and passphrase, with **Copy** and a QR code if `qrcode` is installed.
- GO IP, connected peers and firewall status (rule present? network category?).
- A checkbox to listen on the direct link. It calls `add_listener`.

**Controller GUI, Pair tab:** shows "Connect the i3 to Wi-Fi `DIRECT-NC-…` (password …), then use
controller `192.168.137.1:8765`", plus the fingerprint.

**Worker GUI, new section "0. Connect" above "1. Pair":**
- SSID and password fields with a **Join** button.
- **Find controller**, which fills in the address.
- Link status (interface, IP, RTT to the controller).

**CLI:**
- `nc wifidirect start|stop|status [--ssid --passphrase]` (controller side).
- `nc worker join --ssid --passphrase`.
- `nc worker find`.

**Status bar on both machines:** the link the worker is using (LAN / Wi-Fi Direct). The controller works this out from
`request.client.host` per worker and shows it in a new `link` column on the Dashboard.

### 6.6 Offline behaviour

- `updater.check/update`: when `git fetch` fails, raise `UpdateError("No internet connection; connect
  to a normal network to update")`. Don't fail silently, and don't hang. Add a timeout.
- Docs: install both machines **before** going router-less, because setup needs internet.

### 6.7 Tests (Link agent)

- `test_security_api.py`: bind hosts `192.168.137.1` (accepted) and `169.254.10.1` (accepted, link-local).
  Admin from a peer IP is refused even when the controller listens on the direct link.
- `test_e2e.py`: a controller on `127.0.0.1` plus a second loopback alias `127.0.0.2`. One worker pairs
  through .1, then .1 is removed, and the worker must re-find the controller on .2 through the fingerprint
  check. An **impostor** with a different cert on .2 must be refused.
- `wifidirect.py` with WinRT and netsh mocked: SSID/passphrase validation, the netsh argument lists, and the
  temp profile file being deleted.
- A manual hardware checklist in `docs/WIFI_DIRECT.md`, because CI has no Wi-Fi.

---

## 7. Phase 3 — Measurement (Perf agent)

### 7.1 Schema migrations — `store.py`

- Add a `meta(key, value)` table with `schema_version`. Add `_migrate()` with numbered steps that use `ALTER TABLE ... ADD
  COLUMN`, idempotent and in one transaction. Existing `state.db` files upgrade on start. Add a test that a 0.1.0 DB still
  opens and keeps its rows.
- New columns:
  - `jobs`: `cpu_seconds REAL`, `peak_mem_mb REAL`, `queue_wait_s REAL` (claim − created),
    `link TEXT`.
  - `attempts`: `duration REAL`, `cpu_seconds REAL`, `peak_mem_mb REAL`.
- `release` keeps the attempt row with outcome `released` instead of deleting it.

### 7.2 Worker reports more per job — `jobrunner.py`, `worker.py`, `winlimits.py`, `controller.py`

- `jobrunner` prints a final `METRICS {"cpu_s": process_time(), "wall_s": …}` line on stderr.
- The worker reads the Job Object's `PeakProcessMemoryUsed` (the struct already exists at `winlimits.py:51`), with psutil
  `memory_info().peak_wset` as a fallback.
- The `complete` and `fail` payloads gain optional `cpu_seconds` and `peak_mem_mb`, validated by Pydantic
  `confloat(ge=0, le=86400)` and `ge=0, le=1_048_576`. The attempt-number check stays as it is.
- Heartbeat `metrics` gains `rtt_ms` (the worker's last request round trip), `on_ac_power` and
  `battery_percent` (`psutil.sensors_battery()`), and `cpu_freq_mhz`.

### 7.3 Persisted samples — `store.py`, `controller.py`

- Add a `worker_samples(worker_id, ts, cpu_percent, mem_percent, running_jobs, rtt_ms)` table. Write one sample per heartbeat,
  kept 24 h, pruned on start and every hour. The in-memory `history` stays for the sparklines.
- Add `Store.samples(worker_id, t0, t1)` for the benchmark statistics.

### 7.4 Tests

The migration from a 0.1.0 DB, metrics flowing end to end in `test_e2e.py` (cpu_seconds > 0 for primes),
stale reports with metrics still rejected, and sample pruning.

---

## 8. Phase 4 — Benchmark and performance sharing (Perf agent)

### 8.1 Workload: new job kind `benchmark` — `jobs.py`

```
params: {"workload": "hash_chain" | "prime_sieve", "units": 1..20000, "seed": 0..2^32}
split : by units (contiguous unit ranges); the cap applies per CHUNK (≤ 500 units), the total is capped at 20000
run   : hash_chain  — unit i = 2,000,000 sha256 rounds from seed‖i      (CPU-bound, ~0 RAM)
        prime_sieve — unit i = segmented sieve of [i·10M, (i+1)·10M)     (cache/RAM-heavier)
result: {"units": n, "digest": xor of unit digests | "primes": count}
combine: xor digests / sum primes  → identical answer in every scenario = correctness check
```

- The total cap is checked in a new `JobSpec.validate_total`, so the existing kinds and their limits don't change.
- **Size presets** are calibrated, not guessed: before the run, the orchestrator times 2 units on every worker
  (about 5 s), then picks `units` so that the **fastest single machine** takes about the target time. Presets: Quick ≈ 30 s,
  Standard ≈ 5 min, Long ≈ 15 min on the fastest machine. Slower machines take proportionally longer.
- Default `chunks` = 4 × the total slots of the machines in that scenario. This keeps the tail short, which
  was the reason the combined demo did not beat the i7 alone.

### 8.2 Orchestrator on the controller — new `network_connector/benchmark.py`

It runs on a thread inside `ControllerRuntime`, so it keeps going if the GUI is closed, and CLI and GUI share it.

```
BenchmarkRunner.start(spec) -> benchmark_id
spec: {label, workload, size|units, scenarios: ["each", "all"] | [worker names..., "all"],
       repeats: 1..5, order: "fixed"|"shuffled"}
```

For each scenario and repeat:
1. **Settle:** wait until every target worker is online, has no running jobs, and has CPU < 20 % for 10 s
   (time out after 120 s and write a warning on the result).
2. **Snapshot settings** of every participant: threads, slots, low priority, memory cap, power state, link, and version.
3. **Submit** one batch (`target` = the worker for the "alone" scenarios, none for "all"), with label
   `bench:<id>:<scenario>:<rep>`.
4. **Wait** until the batch is done. Take wall time **from the store** (`max(finished) − min(started)`), not from a client timer.
5. **Collect:** per worker, the chunks, busy seconds, CPU seconds, peak memory, average and max CPU % from
   `worker_samples`, average queue wait (overhead), and RTT.
6. **Check** that the combined answer is identical across all scenarios. If not, mark the result **INVALID**.

**Derived results:**
- throughput = units / wall.
- speedup(all) = wall(fastest alone) / wall(all).
- ideal speedup = Σ throughput(alone) / max throughput(alone).
- parallel efficiency = speedup / ideal.
- relative speed per machine = throughput / throughput(fastest).
- share of the work per worker in the "all" scenario.
- overhead = wall − busy / slots.
- Repeats report the median and the spread (min–max).

**Stop:** cancel the current batch (§5.2 path) and mark the benchmark `cancelled`. On a controller restart,
an unfinished benchmark is marked `interrupted`. It does not resume, because the numbers would be mixed.

### 8.3 Storage — `store.py`

- `benchmarks(id, created, label, spec JSON, status, result JSON, error)`.
- `benchmark_runs(benchmark_id, scenario, repeat, batch_id, started, finished, wall_s, settings JSON,
  per_worker JSON, answer TEXT, valid INT)`.

### 8.4 API — `controller.py`, `client.py`

**Admin:**
- `POST /admin/benchmarks` (validated spec, idempotency key).
- `GET /admin/benchmarks?limit` and `GET /admin/benchmarks/{id}`.
- `POST /admin/benchmarks/{id}/cancel`.

**Worker, read-only:** `GET /worker/benchmarks/latest` returns the most recent finished benchmark that **this worker took part in**.
- Contents: aggregate numbers, worker names, CPU model, threads and RAM only. Size-capped at 64 KB.
- This is how the i3 sees the comparison ("share their performance"). Workers still open no ports.

### 8.5 CLI — `cli.py`

```
nc bench run [--workload hash_chain] [--size quick|standard|long | --units N]
             [--scenarios each,all | i7-local,i3-laptop,all] [--repeats 3] [--wait]
nc bench list
nc bench show <id>                 # table like the one below
nc bench export <id> --format json|csv|html [-o file]
nc bench cancel <id>
nc worker perf                     # on the i3: latest shared result
```

Example output of `nc bench show`:

```
Benchmark 7c1e…  hash_chain, 9,600 units, standard, 3 repeats, link: Wi-Fi Direct
SCENARIO     WALL (median)  RANGE        UNITS/S  SPEEDUP  CPU avg  CHUNKS (i7/i3)
i7-local     301.2s         298–305s     31.9     1.00     97%      96/0
i3-laptop    1188.4s        1180–1197s    8.1     0.25     99%      0/96
all          243.0s         240–247s     39.5     1.24     96%/98%  77/19
ideal speedup 1.25   efficiency 99%   overhead 1.8%   answers identical ✓
```

### 8.6 GUI

**Controller, new "Benchmark" tab** (next to Demo):
- Inputs: workload, size, scenarios (a checkbox per worker plus "all together"), repeats, and Run/Stop.
- **Live view:** the current scenario, a progress bar, and each machine's CPU %.
- **Results:** a table (as above) and a bar chart on a `tk.Canvas` of wall time per scenario, plus a stacked bar of work share in
  "all". Same colours as the Dashboard sparklines.
- **History:** a list of past benchmarks. Open, Export (JSON/CSV/HTML), Delete.

The Demo tab stays as it is (quick check). Its text points to Benchmark for real comparisons.

**Worker GUI, new section "Performance":** the latest shared result from `/worker/benchmarks/latest`, with
"this computer: 0.25× the i7, did 19 of 96 chunks in the combined run". Refreshed every 30 s.

**HTML export:** a single self-contained file (inline CSS/SVG, no external links), so it can be sent to someone.

### 8.7 Fairness, so the numbers mean something

Record and show these with every result:
- **AC power:** warn if either laptop is on battery.
- **Settings:** `low_priority`, threads and slots.
- **Foreground load:** CPU before settling.
- **Link type and RTT.**
- **Repeats:** default 3, report the median. Option: shuffle the scenario order to avoid thermal bias.
- **Caveat:** the i7 also runs the controller, so its numbers include that small cost (shown as a note).

### 8.8 Tests

- **`benchmark` kind:** split then combine equals the whole, the per-chunk and total caps, identical digests across chunkings.
- **Orchestrator against `Cluster`** with 2 workers and tiny units, scenarios each and all:
  - statuses, answers identical, speedup computed, cancel mid-run;
  - interrupted after a restart.
- **API:** admin-only routes; `/worker/benchmarks/latest` returns only benchmarks the caller took part in, and returns 401
  without a token.
- **CLI:** a smoke test of `nc bench run --units 4 --wait` and `nc bench export --format csv`.

---

## 9. Phase 5 — Integration, docs and acceptance

### Documentation

- **New `docs/WIFI_DIRECT.md`:** requirements check, mode A and B steps on each laptop, firewall, fingerprint check,
  troubleshooting (adapter missing, Public profile, the i7 dropping home Wi-Fi, 169.254 address).
- **New `docs/BENCHMARK.md`:** how to run, how to read speedup and efficiency, fairness checklist.
- **Update:** README (features, quick start), SETUP_I7/I3 (the router-less option), SECURITY (new threat rows:
  open radio link, rogue group with the same SSID, discovery probe), CONFIGURATION (`bind_hosts`,
  `controller_fingerprint`, `known_hosts`), TROUBLESHOOTING, CHANGELOG 0.2.0.

### Acceptance, on the real i7 + i3 with no router

1. [ ] The i7 GUI starts the Wi-Fi Direct group, and the home Wi-Fi stays connected (or the doc says it can't on this adapter).
2. [ ] The i3 worker GUI joins with SSID and password, **Find controller** fills in `192.168.137.1:8765`, the fingerprints match, and pairing works.
3. [ ] The demo runs over the direct link. The Dashboard shows the i3 with link = Wi-Fi Direct.
4. [ ] Turning the group off and on again: the i3 reconnects by itself without re-pairing.
5. [ ] An impostor controller on the same IP with another cert is refused (manual check with a second controller home).
6. [ ] A Standard benchmark, scenarios each + all, 3 repeats: finishes, the answers are identical, and the result table and chart appear on the i7.
7. [ ] The i3 shows the same result under "Performance", and in `nc worker perf`.
8. [ ] The HTML export opens offline.
9. [ ] Cancel all and the benchmark Stop leave 0 queued and 0 running jobs.
10. [ ] With a CLI controller already running, the GUI attaches to it instead of failing.
11. [ ] All tests pass: `.\.venv\Scripts\python.exe -m pytest`.
12. [ ] No new listening port on the i3: `Get-NetTCPConnection -State Listen` shows no `nc` processes.

## 10. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| The i3 or i7 adapter does not support the GO role, or concurrent use with home Wi-Fi | No mode A | Phase 0 gate. Fall back to mode B, or document "disconnect home Wi-Fi" |
| The WinRT publisher needs package identity or admin rights from desktop Python | Mode A blocked | Phase 0 check. Run it in a small helper started elevated, or use mode B |
| Windows marks the link Public | Controller unreachable | Narrow second firewall rule (§6.4). The GUI shows the rule status |
| The group IP is not 192.168.137.1 | Discovery misses it | Read it from the adapter at runtime and never hard-code it. Candidates = gateway + `.1` |
| A rogue access point with the same SSID | The i3 joins the wrong group | Fingerprint-pinned TLS: the worker never trusts it. Pairing still needs the code + HMAC |
| Several uvicorn servers in one process | Shutdown or ordering bugs | One `Server` per address with a shared `should_exit`. e2e test with 2 loopback aliases |
| Benchmark noise (turbo, heat, battery) | Misleading speedups | Settle step, AC power check, 3 repeats with median, shuffle option |
| Schema migration on an existing `state.db` | Lost history | Versioned, idempotent migrations. Back up `state.db` to `state.db.bak-0.1.0` first |

## 11. Branches and order of merging

- `feat/phase1-fixes` → `feat/wifi-direct` (Link agent) and `feat/measurement` → `feat/benchmark`
  (Perf agent), each rebased on `main` after every merge.
- **Merge order:** phase1 → measurement → wifi-direct → benchmark → docs/release.
- **Shared files** (`controller.py`, `client.py`, `cli.py`, `gui.py`, `worker_gui.py`, `worker.py`) have
  agreed areas of ownership, listed in [AGENT.md](AGENT.md) §4. Each change stays in its own functions or sections, to keep conflicts small.

---

# Part 2 — 0.3.0: open, one-click, sandboxed performance sharing

## 12. Goal, in simple words

1. **Anyone can be either role.** Every PC with the app can **ask for performance** (requester, today's
   "controller"), **give performance** (helper, today's "worker"), or both, from one app.
2. **One click, no pairing codes.** Open the app and nearby requesters show up in a list, like the Wi-Fi menu.
   Joining takes one click or none.
3. **Always ready.** The app runs in the background (tray icon) while Wi-Fi is on. When a nearby requester asks
   for performance:
   - **Auto-accept (default):** the helper joins by itself and the owner only gets a message:
     "Navod's i7 is using 4 of your CPU threads — [Stop]".
   - **Ask me first (setting):** the owner gets **Accept / Decline** for each new request.
4. **Fully isolated work.** The requester's code runs in a **WebAssembly (Wasm) sandbox** on the helper. It has
   no network, no files and no access to the owner's devices, and it has hard CPU, memory and time limits.
   **A fresh sandbox is created for every job and deleted when the job ends.** Nothing stays behind.
5. **Shared performance results.** The Part 1 benchmarks work across any number of helpers, and every
   participant sees the results.

### Why pairing codes are no longer needed

Part 1 trusts *who* the other computer is, so it needs a code to prove it. Part 2 makes safety stop depending on trust:

- **The helper is protected by the sandbox,** whoever the requester is. A requester can only send Wasm code, and that
  code can't reach anything outside the sandbox.
- **The requester is protected by checking results** (§18). A helper that returns wrong answers is caught and
  dropped.
- **Traffic is still encrypted** with TLS. Each device has a permanent identity (§14), so "trusted" and "blocked"
  lists work without codes.

Pairing codes stay available as an option for **closed groups**, for example the Part 1 i7 + i3 setup, and for the
command line.

### Honest limits (shown in the docs and on first start)

- No sandbox can promise *zero* risk. Wasm plus a separate low-privilege process plus no network makes the risk very
  low, and Windows updates fix the rare escape bugs. The owner can stop everything instantly.
- Job **inputs are visible to the helper's computer.** Don't send private data to people you don't trust.
- Helpers can't be stopped from returning wrong results, only caught (§18).
- Wi-Fi Direct groups are small (about 8 devices). Bigger groups need a normal router or hotspot.
- The app can't force Wi-Fi on. It stays ready while Wi-Fi is on, and it can ask Windows to turn the Wi-Fi radio on.

### Non-goals for 0.3.0

- Internet-wide sharing. Only devices on the same Wi-Fi, LAN or Wi-Fi Direct group.
- Running native programs (.exe), containers or virtual machines for requesters. **Only Wasm.**
- GPU work inside the sandbox.
- macOS and Linux helpers. The design keeps them possible later (Wasm is portable); only the Windows layers are built now.
- Payments or credits.

## 13. Roles and the one app

| Old name (Part 1) | New name in the UI | Meaning |
|---|---|---|
| Controller | **Requester** ("Ask for performance") | Starts a group, sends work, collects results |
| Worker | **Helper** ("Give performance") | Joins groups, runs work in the sandbox |

The code can keep the old names internally (`controller.py`, `worker.py`); only the UI and docs change.

- **One window**, `network_connector/app.py`, launched by `network-connector`. It has three big buttons on first start:
  **Give performance**, **Ask for performance**, **Both**. The two Part 1 windows become tabs of this app.
  `nc gui` and `nc worker-gui` keep working.
- **Background service:** a tray icon (`pystray`), starting at login with a per-user Startup entry (no admin rights).
  Tray menu: status, Pause, Stop helping, Open, Quit.
- **Switching roles** keeps the device identity, the trusted and blocked lists and the settings. When a PC stops
  requesting, it first stops handing out work, lets running chunks finish (or re-queues them), and announces
  "closing". Helpers then leave cleanly.
- **Helping several requesters:** a helper can belong to several groups. Default: one at a time, first come first
  served. Option: split slots between groups, with leases and limits kept separately per requester.

## 14. Device identity (replaces the pairing code)

- On first start each install creates a **device key and self-signed certificate** (ECDSA P-256 through
  `cryptography`, as the controller cert already does), stored ACL-protected in `%LOCALAPPDATA%\NetworkConnector\identity\`.
  - **Device ID** = SHA-256 of the certificate.
  - **Short ID** = the first 8 characters, shown as `AB12-CD34`.
  - **Friendly name:** the owner's choice, default the computer name.
- The **requester's server certificate is the device certificate**. Existing controller certs are migrated by reusing
  the current `controller.crt`/key as the device identity, so Part 1 workers don't have to re-pair.
- A helper proves its identity at join time by signing the requester's challenge with its device key (§16).
- **Trusted list** and **blocked list** are keyed by device ID, on both sides.
- **Optional "Verify" (Bluetooth-style), only to mark a device Trusted:**
  - Both screens show the same 6-digit number. It is computed with a **commit-then-reveal** of random nonces
    (as in Bluetooth LE Secure Connections numeric comparison), so an attacker in the middle can't choose keys to get the same number.
  - The owner checks that the numbers match and clicks **Trust**. This is never required to help, only to unlock "Trusted only" policies.

## 15. Discovery: "nearby groups" list

### 15.1 Announcements (requester → everyone nearby)

- The requester sends a small **UDP announcement** every 2 s to the multicast group `239.255.78.67:8766`, and on the
  Wi-Fi Direct subnet also as a broadcast. Contents (JSON, ≤ 1200 bytes):
  `{v, device_id, name, host, port, open, need: {threads, est_minutes}, job_label, sandbox: "wasm", ts}`.
  It is **signed with the device key.** The signature doesn't prove a stranger is honest, but it stops anyone faking an
  announcement from a device you already trust or blocked.
- `open` is true only while the requester is accepting helpers.

### 15.2 Listening (helper)

- The helper has **one receive-only UDP socket** (bound to 8766, multicast join). It never sends replies on it
  and never takes commands from it.
  - Packets that are too big, badly formed, wrongly signed or old (> 10 s) are dropped, with a rate limit per source.
  - The only thing an announcement can cause is a line in the nearby list or, with auto-accept, an **outbound** TLS
    connection to the announced address.
  - **This replaces Part 1 invariant 5** ("workers open no listening ports"): helpers still open **no TCP
    ports** and accept no commands.
- The nearby list shows: name, short ID, Trusted/Blocked badge, number of helpers, what it needs, and link type.

### 15.3 Connecting without a router

- **Requester:** "Ask for performance" with no router starts the Part 1 Wi-Fi Direct group (mode A) or hotspot (mode B). The SSID is
  `DIRECT-NC-<short id>`.
- **Helper:** with the setting **"Join nearby groups automatically"** on, the helper scans Wi-Fi networks for
  `DIRECT-NC-*` and joins only when it is **not connected to another Wi-Fi network**, unless the adapter supports
  both at once.
- **The Wi-Fi password is not the security boundary.** Sandbox, signatures, TLS and result checks are. So the passphrase is
  derived from the SSID (`PBKDF2(ssid, app salt)`), and every copy of the app can join with one click. Say
  this plainly in docs/SECURITY.md.
- Phase 6 must confirm this works on real hardware. If it doesn't, the fallback is: everyone on the same router or
  hotspot, with discovery by announcements only.

## 16. One-click join and consent

### 16.1 Join protocol (replaces `POST /pair` for open groups)

1. **Helper → requester:** `POST /join/hello` returns `{challenge, requester_device_id}`. The helper checks that the
   TLS certificate matches the announced `device_id` and that it isn't blocked.
2. **Helper → requester:** `POST /join` with `{name, device_cert, signature(challenge), info, policy_summary}`.
3. **The requester decides,** by its **join policy:** *Anyone nearby* (default) / *Trusted only* /
   *Ask me*. It then issues a token bound to the helper's device ID, stored only as a hash (as today).
4. If the helper has **Ask me first** on, it shows **Accept / Decline** *before* step 1. With auto-accept it shows a
   message afterwards.
5. On leave, decline, block or revoke the token is dropped. A device that comes back joins again in one step.

`POST /pair` with codes stays for closed groups. A requester setting "Require pairing code" turns `/join` off.

### 16.2 Helper settings (the owner's control panel)

| Setting | Default |
|---|---|
| When a requester asks | **Accept automatically and tell me** / Ask me each time / Trusted only |
| Max CPU threads / jobs at once / RAM per job | Threads − 1 / threads − 1 / 25 % of RAM |
| Only when plugged in | On |
| Only when I'm idle (no input for N min) | Off |
| Hours | Any time |
| Daily limit per requester (CPU-minutes) | 120 |
| Join nearby Wi-Fi Direct groups automatically | Off |
| Blocked devices | — |

- The **Stop** button (tray and message) kills every sandbox within 1 s and leaves the group.
- The helper shows **what is running:** requester name and short ID, job label, Wasm module hash, CPU and RAM now, time used today.

## 17. Wasm sandbox (the heart of Part 2)

### 17.1 Job type `wasm`

```
params: {
  "module_sha256": "<hex>",            # module bytes sent once, cached by hash (§17.4)
  "input_b64":     "<base64 ≤ 8 MB>",  # given to the module on stdin
  "args":          ["<≤ 16 short strings>"],
  "limits": {"memory_mb": ≤ helper cap, "seconds": ≤ 3600, "fuel": optional}
}
result: {"stdout_b64": ≤ 1 MB, "exit_code": int, "fuel_used": int | null, "cpu_seconds": float}
        # fuel is only on when the requester sets limits.fuel (+8 % cost, Phase 6 spike)
split:  the requester sends one job per chunk (each with its own input); the module is shared
```

- **ABI = WASI preview 1 "command", stdin → stdout,** which most languages compile to directly:
  - Rust `wasm32-wasip1`;
  - C/C++ with `wasi-sdk`;
  - Go `GOOS=wasip1`;
  - Zig;
  - AssemblyScript.
- Built-in native kinds (`prime_range`, `benchmark`, …) stay, for trusted groups. **Open groups accept `wasm` jobs only**, so
  nothing but sandboxed code runs for strangers.

### 17.2 Layers of isolation (defence in depth)

Each job is a new child process: `python -m network_connector.sandbox_runner`. Every layer below must hold on its own.

1. **Wasm engine:** `wasmtime` (the official `wasmtime` Python package).
   - Memory is capped through `Store` limits, fuel (instruction count) or epoch interruption is used for time,
     and the stack is limited.
   - **WASI with nothing pre-opened:** no directories, no environment variables, no sockets (preview-1 has none, and
     preview-2 sockets stay off). Only stdin, stdout, stderr, clocks and random.
   - Imports outside this list are refused when the module is linked.
2. **AppContainer process** (Windows' built-in app sandbox, the one used by Edge/Chrome renderers), started through
   `CreateProcess` with `SECURITY_CAPABILITIES` and **no capabilities**:
   - no network at all, even if the Wasm layer failed;
   - no access to the user's files;
   - a fresh AppContainer profile per job, deleted afterwards.
   - The runner itself (Python + wasmtime) runs inside it.
3. **Job Object** (already used in Part 1, `winlimits.py`):
   - memory cap, `ActiveProcessLimit = 1` (no child processes), kill-on-close;
   - CPU-rate cap when the owner set one, below-normal priority.
4. **Low integrity level and a restricted token** as a fallback layer if AppContainer creation fails. In that case
   **refuse open-group wasm jobs** rather than run them with less isolation, and say so in the UI.
5. **Input/output limits:** module ≤ 10 MB and validated by `wasmtime.Module.validate` before use, input ≤ 8 MB,
   output ≤ 1 MB, stderr ≤ 64 KB (cut off). All sizes are checked by both the requester API and the helper.

**Confirmed by the Phase 6 spike** ([spikes/0.3-sandbox/REPORT.md](spikes/0.3-sandbox/REPORT.md); all items GO, `wasmtime==50.0.1`):

- **Time limit:** the time limit is the **helper's wall-clock kill** (`TerminateJobObject`, < 20 ms).
  - Fuel and epoch can't stop a module that sleeps or does a lot of I/O.
  - Epoch (one-shot deadline) is optional: it only gives a clean "time limit" message, at +17 % cost.
  - Fuel is optional, on only with `limits.fuel`, at +8 % cost.
- **Launch recipe:**
  - Start the **base** `python.exe -I -S -B` directly, never the venv launcher (blocked by `ActiveProcessLimit=1`).
  - Pass `LOCALAPPDATA` and `PROCESSOR_ARCHITECTURE` in the environment.
  - Use `DETACHED_PROCESS`, plus `JOB_LIST` and `CHILD_PROCESS_POLICY` in `STARTUPINFOEX`.
  - Set `platform.machine` before importing wasmtime. This cuts start-up from about 565 ms to about 295 ms. No pool is needed.
- **Runtime folder:**
  - The runner and wasmtime live in their own folder, `%LOCALAPPDATA%\NetworkConnector-runtime`.
  - That folder gets the one ACL grant `ALL APPLICATION PACKAGES:(OI)(CI)(RX)` (no admin).
  - **Never in `%LOCALAPPDATA%\NetworkConnector`**, which holds secrets and must stay unreadable from the sandbox.
  - `sandbox.available()` checks the grant.
- **Import allowlist:** `wasi_snapshot_preview1` only, without `path_open` or sockets. C code that uses `fopen` is refused; say so in SANDBOX.md.
- **Honest limits for the docs:**
  - Escaped code can bind a port, but nothing can reach it.
  - It can read world-readable Windows files (for example `C:\Windows\win.ini`), but not user files.
  - Stricter LPAC mode doesn't work with Python yet.

### 17.3 "Created per job, deleted after"

- **Before:** create the AppContainer profile and an empty working folder inside the container's own storage
  (only for the runner's temp files; the Wasm module never sees it).
- **During:** stream progress (fuel used / time) to the helper through a pipe.
- **After** (success, failure, timeout, Stop or crash):
  - kill the Job Object;
  - delete the working folder and the AppContainer profile (`DeleteAppContainerProfile`);
  - wipe any in-memory copy of the input.
  - A **cleanup sweep at start-up** removes anything left by a crash or power loss.
- **Test:** after 100 jobs, no `NC-Sandbox-*` profiles or folders remain.

### 17.4 Module distribution

- **Requester:** `POST /admin/modules` uploads a module (≤ 10 MB) and returns its SHA-256.
- **Helper:** `GET /worker/modules/{sha256}` fetches it once, **checks the hash**, and caches it (LRU, 200 MB, per-user
  folder). A module is only a file. It never runs outside the sandbox and is never marked executable.
- The helper shows the hash, so a blocked module can be refused everywhere.

### 17.5 Spike questions (Phase 6)

These must be answered before building:
- Can the `wasmtime` Python package (and Python itself) run **inside an AppContainer** with no capabilities? What does it need
  (DLL load paths, ACL grants on the venv folder for `ALL APPLICATION PACKAGES`)?
- Start-up cost per job: target ≤ 300 ms. If it's too slow, keep a **pool of fresh sandboxes** ready, each used for exactly one job and then destroyed.
- Speed of Wasm compared with native for the benchmark workload, published in the benchmark results.
- Is fuel or epoch interruption more accurate for the time limit?

### 17.6 Python jobs inside Wasm (optional, Phase 10)

- Helpers ship a pinned **CPython WASI build** (`python.wasm`, about 25 MB, installed once, hash-checked).
- Requesters can then send **Python scripts** as `wasm` jobs with `module = python.wasm`, `args = ["-c", ...]`
  or a script on stdin. They still run in the same sandbox, so nothing changes in security.
- This lets people use the system without learning Rust or C.

## 18. Checking results and trust (protects requesters)

- **Redundancy:**
  - In open groups, 10 % of chunks (setting: 0–100 %) also go to a second, different helper, and the outputs are compared byte for
    byte. Wasm without clocks/random in the output is deterministic, so the outputs should be identical.
  - Requesters can also add a **check function**: a small Wasm verifier run on the requester's own machine.
- **On a mismatch:** run it a third time on another helper or on the requester itself, take the majority, and mark the loser.
- **Reliability score** per helper device ID: matches minus mismatches, timeouts and abandoned jobs. Low-score helpers get fewer chunks, and a score below the threshold means
  they are dropped and auto-blocked for that requester.
- **Helper side:** a per-requester reputation (jobs killed for limits, outputs over the limit). The owner can block a requester in one click.

## 19. Sharing performance with many helpers

The Part 1 benchmark (§8) is extended:
- Scenarios: each helper alone, **top N**, everyone. With many helpers, "each alone" becomes optional.
- Workload `wasm_bench`: the Part 1 `hash_chain` compiled to Wasm (Rust source in `examples/wasm/bench/`), so
  the results show **sandbox speed**. It is reported next to the native `benchmark` kind, giving a "Wasm overhead" column.
- Results are shared with every participant (`/worker/benchmarks/latest`, as in §8.4) and show device names and short IDs, not IPs.
- Every helper keeps its **own speed score** locally (from calibration). It shares the score with requesters only if the owner allows, so a
  new requester can size chunks straight away.

## 20. API and data changes (summary)

| New / changed | Who calls it | Notes |
|---|---|---|
| `POST /join/hello`, `POST /join` | helper | §16.1; rate limited per source IP; turned off by "Require pairing code" |
| `POST /worker/leave` | helper | Drops the token, re-queues the helper's jobs |
| `GET /worker/modules/{sha256}` | helper | Hash-checked download, ≤ 10 MB |
| `POST /admin/modules`, `GET /admin/modules` | requester (local) | Upload and list modules |
| `POST /admin/jobs` with `kind: "wasm"` | requester (local) | Validated as in §17.1 |
| `GET/PUT /admin/policy` | requester (local) | Join policy, redundancy %, require-code |
| Store tables | — | `devices(device_id, name, cert, trusted, blocked, score, first_seen, last_seen)`, `modules(sha256, size, uploaded, label)`, `job_checks(job_id, twin_job_id, match)` and `helper_usage(requester_id, day, cpu_seconds)` on the helper side |
| Heartbeat response | — | Adds `closing: bool`, so helpers leave cleanly |

## 21. Phases for Part 2

| Phase | What | Owner | Exit gate |
|---|---|---|---|
| 6 | **Spikes:** wasmtime inside an AppContainer from Python (§17.5), sandbox start-up time, Wasm vs native speed, multicast on Wi-Fi Direct and hotspot, auto-joining `DIRECT-NC-*` while on another network, `pystray` and toast notifications | Both (Link: network spikes; Compute: sandbox spikes) | `spikes/REPORT-0.3.md` with go / fallback per item |
| 7 | **Identity and join:** device identity, announcements and listener, nearby list, `/join` protocol, join and helper policies, trusted/blocked lists, Verify (numeric comparison), leave/closing | Link agent | A helper joins with one click (or none) on a LAN; no code needed; blocked devices refused |
| 8 | **Sandbox:** `sandbox_runner`, wasmtime limits, AppContainer + Job Object layers, per-job create/delete and start-up sweep, `wasm` job kind, module upload and cache | Compute agent | Escape test suite (§22) passes; 0 leftovers after 100 jobs |
| 9 | **One app and the background service:** `app.py` with role buttons, tray, autostart, auto-accept or ask messages, Stop within 1 s, helper settings panel, quotas (plugged in, idle, hours, daily limit) | Link agent (UI, tray, notifications) + Compute agent (quota enforcement in the scheduler and helper) | Owner can accept, decline and stop from the tray; limits are enforced |
| 10 | **Result checks and trust:** redundancy, majority, reliability score, auto-block, requester-side verifier; optional `python.wasm` | Compute agent | A lying test helper is caught and dropped |
| 11 | **Many-helper benchmarks, `wasm_bench`, docs, security review, release 0.3.0** | Both | Acceptance list in §22 passes on real hardware |

Phases 7 and 8 run in parallel after Phase 6. Phase 9 needs 7 and 8. Phase 10 needs 8.

## 22. Tests and acceptance for Part 2

### 22.1 Sandbox escape suite

Every one of these must **fail safely** (killed or refused, with nothing written outside the sandbox):

1. A Wasm module that tries to open a file, list a folder, read environment variables, or open a socket (both WASI calls and unknown imports).
2. Memory bomb (grow until the limit), infinite loop (time/fuel limit), deep recursion (stack limit), huge stdout or stderr.
3. A module that is not valid Wasm, or is too big, or whose hash doesn't match.
4. **Layer tests without Wasm:** a native test program started *as if* it had escaped Wasm (same AppContainer + Job Object launch).
   It must not be able to:
   - reach the network (`connect` to the requester, to 8.8.8.8, or to 127.0.0.1);
   - read `%USERPROFILE%` or write outside its folder;
   - start a child process;
   - exceed the memory cap.
5. Stop during a run: the process is gone within 1 s and the profile is deleted.
6. Power-loss simulation: kill the helper mid-job, restart, and the sweep removes the leftovers.

### 22.2 Join and discovery tests

- Badly formed, oversized, old or wrongly signed announcements are ignored. A flood of announcements is rate limited.
- An announcement claiming a trusted device's ID but with another key is refused.
- `/join` with a wrong signature is refused. A blocked device is refused. "Require pairing code" turns `/join` off.
- An open group refuses native job kinds from the requester to helpers (only `wasm`).
- Helpers still have **no TCP listening port** (`Get-NetTCPConnection -State Listen`).

### 22.3 Acceptance on real hardware (i7 + i3, then 3 or more PCs if available)

1. [ ] Install on a new PC, choose **Give performance**, and the tray icon appears and starts at login.
2. [ ] The i7 clicks **Ask for performance**, and the i3 shows it in the nearby list within 5 s.
3. [ ] With auto-accept, the i3 joins by itself and shows the message. With "Ask me first" it waits for Accept.
4. [ ] A Rust `wasm` job (example in `examples/wasm/primes/`) runs on both machines with the right answer.
5. [ ] **Stop** on the i3 kills the work within 1 s, and the i7 re-queues it.
6. [ ] The escape suite (§22.1) passes on both machines.
7. [ ] After the run, no sandbox profiles or folders remain on the i3.
8. [ ] With no router: the i7's group is joined by the i3 in one click (or automatically, if the setting is on).
9. [ ] A test helper that returns wrong answers is caught by redundancy and dropped.
10. [ ] The many-helper benchmark shows the native vs Wasm speed, and every participant sees the results.
11. [ ] Daily limit, "only when plugged in" and hours are enforced on the helper.
12. [ ] All tests pass. Part 1 closed groups with pairing codes still work.

### 22.4 Risks for Part 2

| Risk | Impact | Mitigation |
|---|---|---|
| Python + wasmtime can't run inside an AppContainer | Weaker isolation | Phase 6 spike. Fallback: low-integrity restricted token, and **refuse open-group jobs** if neither layer is available |
| A wasmtime or Windows sandbox escape bug | Harm to a helper | Several independent layers, pinned and quickly updated wasmtime, no network even after an escape, Stop button, docs say the risk is very low, not zero |
| Helpers used for abuse (for example crypto mining) | Owner's power and CPU wasted | Daily limits, visibility of what runs, blocking, "plugged in only", Trusted-only option |
| Lying helpers | Wrong results | Redundancy, majority, reliability score, verifier hook |
| Discovery spam or fake requesters | Annoyance, wasted joins | Signed announcements, rate limits, block list, Ask-me-first mode |
| Auto-joining `DIRECT-NC-*` drops the helper's home Wi-Fi | Owner loses internet | Off by default; only join when not connected elsewhere, unless the adapter supports both |
| Sandbox start-up too slow for small jobs | Poor speedups | Pool of fresh one-use sandboxes; bigger chunks |
| Private data sent to strangers | Privacy leak | Clear warning in the requester UI; "Trusted only" join policy |
