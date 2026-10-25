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
| T6 | Remote code execution through a job | No code, shell command, module name or file path is accepted. A job is a *kind* (looked up in a fixed allowlist) plus plain data, validated on the controller **and again** on the worker. Unknown keys are rejected. |
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
6. To rotate everything: stop the controller, delete `controller\server.crt`, `server.key` and
   `admin.token`, start it again, and re-pair all workers.

## Reporting a problem

Open a private security advisory on the GitHub repository, rather than a public issue.
