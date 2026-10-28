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

Set address lists in environment variables as JSON, for example
`NC_BIND_HOSTS=["192.168.1.4","127.0.0.1"]`. Only use addresses assigned to this
computer. See [WIFI_DIRECT.md](WIFI_DIRECT.md) for runtime link setup and firewall rules.
