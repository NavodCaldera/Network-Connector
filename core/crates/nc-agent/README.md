# nc-agent

The device supervisor: the `Agent` API (AGENT.md §3) used by `nc-bridge`, the `nc-agent` daemon, and the
requester helpers used by `nc submit`.

## Helper loop

register -> (each round) finish journalled attempts -> owner policy (consent, hours, power, battery, thermal,
idle) and tariff window/budget -> heartbeat -> lease -> verify the signed job manifest (canonical bytes,
publisher signatures, coordinator caps), check transfer refs, owner trust/profile/limits, budget pre-check ->
download module and inputs (Range resume, tariff-gated per 64 KiB step, hash-checked, inputs cached in nc-blob)
-> run in nc-exec (AppContainer + Job Object, `cpu-core-v1`) with a monitor that cancels on Stop or a policy
change -> upload the output -> Complete v2. Every refusal after a lease is sent as LeaseFail with a reason
and a short code (for example `VERIFY_FAILED/MANIFEST`, `POLICY_REFUSED/UNTRUSTED_PUBLISHER`, `BUDGET/PRECHECK`,
`TARIFF_WINDOW/WINDOW_ENDED`, `STOPPED_BY_OWNER/STOP`, `CRASHED/AGENT_RESTART`).

v1 helpers accept only unencrypted transfers (the transport hash must equal the signed plaintext hash);
encrypted transfers are refused with `VERIFY_FAILED/ENCRYPTED_UNSUPPORTED`.

## Daemon

```text
set NC_ENROLL_SECRET=...        (never put it in the file)
nc-agent run --config agent.json
nc --config agent.json status | start | stop | shutdown | policy show | policy set policy.json
nc --config agent.json submit module.wasm input.bin --out result.bin
```

`agent.json`:

```json
{
  "coordinator_url": "https://coordinator.example",
  "data_dir": "C:\\Users\\me\\AppData\\Roaming\\NetworkConnector\\agent",
  "device_name": "office-pc",
  "worker_path": "C:\\Program Files\\NetworkConnector\\runtime\\nc-exec-worker.exe",
  "network_budget": "unmetered",
  "probe": {
    "network": { "interface": "ethernet", "connection_id": "home-fibre", "bytes_per_second": 2000000 },
    "declared_thermal": "nominal"
  }
}
```

For night data, `network_budget` is `{"metered": <nc-tariff TariffProfile>}`. `http://` is accepted only for
loopback (the Docker test stack). The policy lives in `<data_dir>/policy.json` (nc-policy JSON; missing file
means the safe defaults, which accept no work). The control socket listens on 127.0.0.1 only, port
`control_port` (default 47611), with a random token in `<data_dir>/control.token`.

## Platform facts

- Power and battery: Windows `Win32_Battery` through PowerShell, refreshed every 30 s off the session thread;
  Linux `/sys/class/power_supply`.
- User idle time is reported unknown (it needs `GetLastInputInfo`, an `unsafe` call not allowed in this crate),
  so set `pause_on_user_activity: false` or nc-policy refuses work.
- Thermal: Windows has no unprivileged reading; the owner may declare a level in the config, otherwise it is
  unknown and nc-policy refuses work.
- The network identity is the owner's declaration in the config (no platform adapter confirms it yet).

## Tests

`cargo test -p nc-agent` runs the fake-coordinator suite. Real-sandbox malicious jobs (Windows):

```text
cargo build -p nc-exec --bin nc-exec-worker
set NC_EXEC_WORKER=<target>\debug\nc-exec-worker.exe
cargo test -p nc-agent --test sandbox_jobs -- --ignored --nocapture --test-threads 1
```

End to end against the Docker coordinator: `--test e2e_docker -- --ignored` with `NC_TEST_URL`,
`NC_ENROLL_SECRET` and `NC_EXEC_WORKER` set (Windows host, because the sandbox is Windows-only so far).
