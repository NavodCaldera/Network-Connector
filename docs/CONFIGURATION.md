# Configuration reference

Settings are JSON files in the data folder (`%LOCALAPPDATA%\NetworkConnector`, or `NC_HOME` if set).
The windows edit them for you. Any setting can also be overridden with an environment variable
`NC_<SETTING NAME IN CAPITALS>`, or a `.env` file (see `.env.example`). Real environment variables
win over `.env`.

## Controller: `controller\controller.json`

| Setting | Default | Meaning |
|---|---|---|
| `bind_host` | auto-detected LAN IP | Address to listen on. Must be private, loopback or link-local. `0.0.0.0` and public IPs are refused |
| `port` | `8765` | TCP port |
| `lease_seconds` | `60` | A job goes back to the queue if its worker is silent this long |
| `default_timeout_s` | `600` | Per-job time limit when the submitter doesn't give one |
| `default_max_attempts` | `2` | Tries per job (first run + retries) |
| `worker_offline_after_s` | `20` | Dashboard shows a worker as offline after this many seconds without a heartbeat |
| `pairing_ttl_s` | `600` | Pairing code lifetime |
| `max_request_bytes` | `25165824` | Largest accepted request (24 MB) |

Example: [examples/controller.example.json](../examples/controller.example.json)

The controller also listens on `127.0.0.1` using the same port and certificate. The admin
client and this computer's local worker use that address, so changing the LAN address
does not change their connection. Remote workers still use the LAN address.

## Worker: `worker\worker.json`

| Setting | Default | Meaning |
|---|---|---|
| `controller_host`, `controller_port` | set by pairing | Where the controller is |
| `name`, `worker_id`, `token` | set by pairing | Identity. **`token` is secret** |
| `max_cpu_threads` | threads − 1 | Upper bound on simultaneous jobs (one thread each) |
| `max_concurrent_jobs` | threads − 1 | Jobs at once; the effective limit is `min(max_cpu_threads, max_concurrent_jobs)` |
| `max_memory_mb` | 60% of RAM ÷ jobs | Per-job memory cap (Windows Job Object). A job using more is stopped |
| `idle_timeout_minutes` | `0` | Stop the worker after this many idle minutes (0 = never) |
| `heartbeat_s` | `5` | Heartbeat interval |
| `poll_s` | `2` | How often to ask for work when slots are free |
| `shutdown_grace_s` | `20` | On stop, how long to let running jobs finish before returning them to the queue |
| `low_priority` | `true` | Run jobs at below-normal priority |
| `ac_only` | `true` | Claim work only while plugged in; desktops with no battery are treated as plugged in |
| `idle_min` | `0` | Claim work only after this many minutes without keyboard or mouse input; 0 disables the gate |
| `allowed_hours` | empty | Local `HH:MM-HH:MM` window for claiming work; overnight windows are supported |
| `daily_cpu_min` | `120` | Daily CPU-minute allowance per requester; 0 disables the cap |
| `blocked_requesters` | empty | Device IDs this helper refuses to serve (the Nearby list's Block uses `identity\peers.json`; both are checked) |
| `limit_stop_now` | `false` | When a limit is reached while jobs run: `false` lets the running chunk finish, `true` stops it at once and hands it back. Blocking a requester always stops its jobs at once |
| `cpu_rate_percent` | `0` | Hard cap on the CPU all jobs together may use, in % of the whole PC (Windows Job Object CPU-rate control), shared out between job slots. `0` = no cap |
| `share_speed_score` | `false` | Tell requesters this PC's measured single-thread speed (one benchmark unit, measured once per version) so they can size work without a calibration run |

These limits are checked on the helper before every claim **and** about once a second while jobs run. CPU time of
running jobs counts against the daily limit before they finish. Together they are the `HelperPolicy` contract
(`accept`, `threads`, `jobs`, `mem_mb`, `ac_only`, `idle_min`, `hours`, `daily_cpu_min`, `blocked`, `stop_now`,
`cpu_rate`) in `network_connector/helper_policy.py`.

The helper checks these settings before each claim. A running chunk can finish after a
limit is reached. CPU usage is stored locally in `helper_usage.db` and counted by
requester and local calendar day. **Stop** immediately pauses claims and kills active
sandboxes; unfinished jobs return to the requester queue.

Example: [examples/worker.example.json](../examples/worker.example.json) (without a token; a real
token only ever comes from pairing).

## Choosing limits for an i3 with 12 GB RAM

| Use | Threads / jobs | Memory per job |
|---|---|---|
| i3 sits idle, maximum throughput | all threads (e.g. 4) | 2048 MB |
| You're using the i3 lightly (browser, documents) | threads − 1 (e.g. 3) | 2048 MB |
| You're actively using the i3 | 1–2 | 1024 MB |

On the i7 (16 threads, 16 GB), 14 threads with 768 MB each leaves room for the controller and your apps.

## Direct-link addresses and discovery

`ControllerConfig.bind_hosts` is a JSON array of additional explicit private,
link-local or loopback IPs. The existing `bind_host` remains the primary address.
The controller always listens on `127.0.0.1`; wildcard/public binds are rejected.
The direct-link tab adds/removes its detected address at runtime without replacing
the TLS certificate. Workers and subnets are never added to the admin allowlist.

Worker pairing stores `controller_fingerprint` and up to four `known_hosts`.
Old workers can derive the fingerprint from their existing pinned certificate.
After three failed connection rounds, the worker probes saved addresses and the
detected direct interface's gateway/first host. It switches only when the observed
certificate matches the pin. `nc worker find` uses the same identity requirement
for paired workers; an unpaired worker must compare the fingerprint before pairing.

## Joining and discovery (0.3.0)

Controller (`controller\controller.json`):

| Setting | Default | Meaning |
|---|---|---|
| `join_policy` | `anyone` (new installs), `code` (upgraded from 0.2.0) | `anyone`, `trusted`, `ask` or `code`. `code` turns `/join` off: pairing codes only |
| `announce` | `true` (new), `false` (upgraded) | Send signed nearby announcements while the group is open |
| `announce_label` | empty | Up to 60 characters shown to everyone nearby |
| `discovery_port` | `8766` | UDP port for announcements |
| `redundancy_percent` | `10` | Share of open-group `wasm` results also run on a second, different helper and compared byte for byte (0–100). Also in the Devices tab and `nc policy --redundancy` |
| `block_score` | `-10` | A helper device whose reliability score falls to this is dropped and blocked for this requester |

Worker (`worker\worker.json`, and `requesters\<id>\worker.json` for each joined requester):

| Setting | Default | Meaning |
|---|---|---|
| `group_mode` | `closed` | `open` for a connection made with Join instead of a pairing code. Set by joining |
| `requester_device_id`, `requester_name` | set by joining | Who this connection is with |
| `join_mode` | `auto` | What to do when a nearby requester asks: `auto` (join and show a message with Stop), `ask` (Accept / Decline, no answer in 60 s = Decline) or `trusted` (join trusted requesters only). Joining by itself needs a working sandbox |
| `discovery` | `true` | Listen for nearby announcements while the window is open (receive-only UDP) |
| `discovery_port` | `8766` | UDP port to listen on |
| `auto_join_direct` | `false` | Scan for `DIRECT-NC-XXXX` groups (at most every 30 s) and join one by itself, but only when this PC is on no Wi-Fi network. While on Wi-Fi it only offers "Join (disconnects <SSID>)" |

## The app: `app.json`

| Setting | Default | Meaning |
|---|---|---|
| `role` | chosen on first start | `give`, `ask` or `both` |
| `autostart` | `true` | Start in the tray at login, for this user only (`HKCU\...\Run`). Turn off with the **Start at login** box or `nc app autostart off` |
| `close_to_tray` | `true` | Closing the window keeps the app in the tray; **Quit** in the tray menu exits |

The device identity (`identity\device.crt`, `device.key`, `identity.json` with the name) and this computer's own
trusted/blocked list (`identity\peers.json`) are ACL-protected. See [JOINING.md](JOINING.md).

Set address lists in environment variables as JSON, for example
`NC_BIND_HOSTS=["192.168.1.4","127.0.0.1"]`. Only use addresses assigned to this
computer. See [WIFI_DIRECT.md](WIFI_DIRECT.md) for runtime link setup and firewall rules.
