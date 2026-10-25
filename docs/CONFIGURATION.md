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
