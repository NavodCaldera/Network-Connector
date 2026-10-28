# Security guide and threat model

## What we protect

| Asset | Why it matters |
|---|---|
| The ability to make a computer run work | It must not become a remote-control tool for strangers |
| Worker tokens, admin token, TLS private key | Whoever holds them can act as a worker or as the admin |
| Files on both computers | Jobs must never read or write arbitrary files |
| Usability of the laptops | A runaway job must not freeze the i3 |

## Trust assumptions

- You own and control both computers and their Windows user accounts.
- The home network is semi-trusted. Other devices on it (TVs, phones, guests) may be
  curious or compromised. They must not be able to submit jobs, control a worker, or read traffic.
- The internet is untrusted. Nothing is ever exposed to it.
- GitHub (your account) is trusted to host the code. A release tag counts as approval.

## Threats and mitigations

| # | Threat | Mitigation |
|---|---|---|
| T1 | A device on the LAN submits jobs | Admin endpoints need the admin token **and** a request coming from the controller computer itself (`admin_local_only`). Workers' tokens can't call admin endpoints. |
| T2 | A device on the LAN impersonates a worker | Worker endpoints need a per-worker 256-bit token, issued only through pairing. Only its SHA-256 is stored. Revoke works instantly. |
| T3 | A device impersonates the controller (man-in-the-middle) | Workers pin the controller's certificate. The pairing proofs are bound to the certificate fingerprint, so a different certificate makes pairing fail on both sides (tested). |
| T4 | Someone guesses the pairing code | About 59-bit code, single use, 10-minute lifetime, locked after 5 wrong tries, kept in memory only and never written to disk. |
| T5 | Eavesdropping on Wi-Fi | All traffic is TLS 1.2+ using the controller's EC P-256 certificate. |
| T6 | Remote code execution through a job | No code, shell command, module name or file path is accepted. A job is a *kind* (looked up in a fixed allowlist) plus plain data, validated on the controller **and again** on the worker. Unknown keys are rejected. The one exception is the `wasm` kind (0.3.0): requester code that runs only inside the sandbox described under [Wasm sandbox](#wasm-sandbox-030). |
| T7 | Jobs reading local files | Runners never open files. File-type jobs carry the file **contents** (base64, max 8 MB), chosen by you on the controller. File names are checked to be plain names. Job processes run in an empty `work\` folder. |
| T8 | Malicious images (decompression bombs) | Pillow's pixel limit is set to 40 MP, and its warnings are treated as errors. Only PNG, JPEG, WEBP and BMP are decoded. |
| T9 | A job exhausting the worker | Per-job memory cap enforced by a Windows Job Object, with a psutil fallback. Thread caps, limits on jobs at once, below-normal priority, timeouts, no new claims above 92% RAM, and pause/disable. |
| T10 | Oversized requests or results | 24 MB request limit (413). Results over 1 MB are rejected. Pydantic validates every request. |
| T11 | Exposure to the internet | Binds only to a private, loopback or link-local IP, and refuses `0.0.0.0`, `::` and public IPs. The firewall rule is limited to the Private profile and LocalSubnet. Never port-forward. |
| T12 | Secrets leaking into Git | All state lives in `%LOCALAPPDATA%\NetworkConnector`, outside the repo. `.gitignore` covers `.env`, `*.db`, keys and certificates. Secret files are ACL-restricted to your user. |
| T13 | A malicious update | Updates install only version tags (`vX.Y.Z`) from your repo, optionally requiring a signed tag (`--verify-signature`). A self-test must pass, otherwise the update rolls back. |
| T14 | Stale or replayed job results | Results are tied to the job id **and** attempt number. Stale ones are rejected (409). |
| T15 | A stolen worker laptop | Revoke the worker on the controller. Its token is deleted and its running jobs re-queued. |

## What is NOT protected (residual risk)

- **Malware already running as your user** on either computer can read the token files and
  act as you. Keep Windows updated and use antivirus. This tool can't defend against a
  compromised account.
- **A compromised controller** can make workers run any *allowlisted* job, but nothing
  outside that list.
- **Denial of service from the LAN** (flooding the port) is not prevented. It can only slow things
  down; it can't get anyone in.
- **Brute force by an active man-in-the-middle during pairing:** someone who intercepts a pairing
  attempt could try to guess the code offline from the proof. With about 59 bits and a 10-minute,
  single-use code, that isn't practical for home threats. Pair on a network you trust.
- **The admin token lets anyone on the i7 user account control the system.** That's intentional:
  the i7 owner is the admin.

## Good practice

1. Keep the controller network set to **Private** only when you're on your own home network.
   On public Wi-Fi, stop the controller.
2. Never forward port 8765 on your router, and never bind to `0.0.0.0`.
3. Revoke workers you no longer use (see [REVOKING.md](REVOKING.md)).
4. Protect your GitHub account with 2FA. Consider signed release tags and
   `nc worker update --verify-signature`.
5. Don't copy `%LOCALAPPDATA%\NetworkConnector` to other machines or share it.
6. To rotate everything: stop the controller, delete `controller\server.crt`, `server.key`,
   `admin.token` **and** the `identity\` folder (since 0.3.0 the controller certificate is the device identity, and a
   new controller would copy it back), start it again, and re-pair or re-join all workers.

## Direct radio links and discovery

| Threat | Protection |
|---|---|
| Nearby devices see the advertised SSID | WPA2 passphrase, generated with cryptographic randomness; paired HTTPS still authenticates application traffic |
| Rogue group advertises the same SSID | The SSID is not an identity. Workers retain the pinned controller certificate and certificate-bound pairing proofs |
| Rogue controller answers a discovery probe | Probes send no token. A paired worker switches address only when the certificate matches its saved pin; every later request still validates pinned TLS |
| Public/Unidentified direct network | Separate inbound rule restricted to the controller's exact IPv4 address, TCP port and peer /24; the Private LAN rule is unchanged |
| Remote worker attempts an admin command | Admin token plus source address restricted to controller-owned binds and loopback; peers/subnets are never admin hosts |

Workers open no TCP listening ports. Since 0.3.0 the only listener on a helper is the receive-only UDP discovery
socket described below. Group credentials remain
in controller memory and are shown only through authenticated local admin controls.
Wi-Fi profile XML is ACL-protected before its passphrase is written and removed after
installation. Windows retains the saved WPA2 profile for reconnecting. Stop/delete
that profile in Windows Wi-Fi settings if the link is no longer needed.

Wi-Fi Direct capabilities and concurrent operation require acceptance on the actual
adapters. Neither a successful mocked test nor an advertised SSID establishes support.

## Open groups: joining without a code (0.3.0)

See [JOINING.md](JOINING.md). In an open group, safety must not depend on trusting the other computer: the helper is
protected by the sandbox (see "Wasm sandbox" below; a helper joins automatically only when the sandbox is available),
the requester by result checks (Phase 10), and both by device identities, pinned TLS and signatures.

| Threat | Protection |
|---|---|
| A stranger joins an open group | That is what "Anyone nearby" allows. The requester chooses Trusted only, Ask me or Require pairing code instead. Joined helpers are marked `group_mode: open` by the requester (never by the helper), so open-group rules can be enforced per helper. Helpers see job inputs: don't send private data to untrusted helpers |
| A helper joins under someone else's identity | `/join` needs an ECDSA signature by the device key over a fresh, single-use, 60-second challenge, bound to the requester's and the helper's device IDs, from the same source address that asked for it. A replayed or relayed signature fails |
| A joined helper takes over a paired worker's name | The requester picks the name (`<name>-<first 8 of device ID>`) and refuses one already used by another device |
| A helper changes its own group mode or identity later | The heartbeat can't change `device_id`, `device_name` or `group_mode`; they are kept from the join |
| Fake or spoofed announcements | Signed by the device key; the device ID is the certificate's hash, so a trusted or blocked device's ID can't be claimed with another key. Size ≤ 1200 bytes, exact fields, age ≤ 10 s, no replay of older packets, the announced address must be the sender, private addresses only. Bad packets are dropped after a per-sender and overall rate limit |
| Announcement makes the helper do something | It can only add a list line. Joining is a click (later: an explicit auto-accept setting) and then an **outbound** TLS connection whose certificate must hash to the announced ID. Nothing is sent to a mismatched certificate |
| The discovery socket as an attack surface | Receive-only: one socket per interface address with `SO_EXCLUSIVEADDRUSE`, never a wildcard bind, no send path. A program squatting on port 8766 can only hide the list |
| Flooding `/join` | Per-source and overall rate limits (429), a 32 KB body cap, bounded challenge and request tables |
| A blocked device keeps working | Blocking revokes its tokens at once (running jobs are re-queued) and refuses its future joins |
| Man in the middle when trusting a device | Optional Verify: commit-then-reveal numeric comparison over the pinned connection; both screens must show the same 6 digits before Trust |
| Leaving | `/worker/leave` drops the token on the requester; the helper forgets it too |
| Announcements leak information | Everyone nearby sees the requester's name, short ID, helper count, queued work and optional label. Nothing else |

The sandbox and "open groups run only `wasm` jobs" are in place (see the next section). Not protected yet: result
checks (Phase 10), and a narrow firewall rule for UDP 8766 (pending approval). Joining `DIRECT-NC-*` groups automatically is off.

## Wasm sandbox (0.3.0)

Requester code runs on helpers only as `wasm` jobs, and only inside the full sandbox. See [SANDBOX.md](SANDBOX.md) for the
layers, the test results and the honest limits.

| # | Threat | Mitigation |
|---|---|---|
| S1 | A requester sends code that reads the helper's files | WASI gets no pre-opened folders; `path_*`/`fd_readdir` imports are refused before linking (on the helper, then again in the runner). Even escaped code runs in a no-capability AppContainer that can't read the user profile or `%LOCALAPPDATA%\NetworkConnector` (tested). |
| S2 | Code reaches the helper's network, the requester or the internet | No sockets in WASI preview 1; `sock_*` imports refused. The AppContainer has no network capability: connects to loopback, LAN, 8.8.8.8 and DNS all fail in the tests, and test listeners receive nothing. |
| S3 | Code starts other programs | The restricted child-process policy blocks it (tested: "process creation has been blocked"), and `ActiveProcessLimit=1` backs it up; if the Job Object ever reports a process-limit hit, the helper treats it as an escape attempt and kills the job. |
| S4 | Code exhausts memory, CPU time or disk | Store memory limit, 512 KB stack, optional fuel; a Job Object memory cap killed on the limit message; the helper's wall-clock kill (works even on sleeping modules); stdout 1 MB, stderr 64 KB; below-normal priority. Only the container's own folder is writable, and it is deleted after the job. |
| S5 | A sandbox survives its job, or a crash leaves one behind | One profile per job, deleted on every path; kill-on-close takes the process down with the helper; a start-up sweep (registry `Mappings` by moniker) removes leftovers, skipping sandboxes of helpers still running. Tested after 100 jobs and after killing a helper mid-job. |
| S6 | The runtime folder becomes a way into the secrets | The only ACL change is `ALL APPLICATION PACKAGES:(OI)(CI)(RX)` on `%LOCALAPPDATA%\NetworkConnector-runtime`, which holds only the runner and wasmtime. Setup refuses a runtime folder overlapping the data folder, and `sandbox.available()` refuses if the data folder is readable by app containers. |
| S7 | Escaped code plants a junction so the cleanup deletes user files | Windows refuses mount points created from the container (tested); cleanup only removes folders named like sandboxes, and Python's `rmtree` does not follow junctions. |
| S8 | A tampered or swapped module | Modules are referenced by SHA-256. The helper checks size (≤ 10 MB), hash and imports before caching; the cache re-checks the hash on every read. Modules are never executed outside the sandbox. |
| S9 | A helper downloads modules it has no business with | `GET /worker/modules/{sha256}` needs a worker token **and** a running `wasm` job of that worker that uses the module; otherwise it answers 404. Uploads (`/admin/modules`) are admin-only and validated. |
| S10 | Native job kinds run for strangers | Open groups hand out `wasm` jobs only (claim filter on the requester), and helpers refuse native kinds when they are in an open group. The requester can only make a helper stricter, never looser. |
| S11 | A sandbox layer is missing on some PC | `sandbox.available()` checks Windows, the Python and runtime grants, the runtime copy and profile creation. If anything is missing the helper does not take `wasm` jobs and refuses one that arrives, with the reason. There is no weaker fallback. |
| S12 | Stale or replayed wasm results | The same job-id + attempt checks as every other kind (409 on stale reports). Owner Stop releases the job so it is re-queued, without using up an attempt. |

Residual risks for the sandbox: a wasmtime or Windows bug could break a layer (several independent layers, pinned
wasmtime, Stop); escaped code could read world-readable OS files and could bind a port nobody can reach; job inputs
are visible to the helper.

## Reporting a problem

Open a private security advisory on the GitHub repository, rather than a public issue.
