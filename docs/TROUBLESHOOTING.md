# Troubleshooting

Logs are in `%LOCALAPPDATA%\NetworkConnector\logs` on each computer. Open them with:

```powershell
notepad "$env:LOCALAPPDATA\NetworkConnector\logs\worker.log"       # on the i3
notepad "$env:LOCALAPPDATA\NetworkConnector\logs\controller.log"   # on the i7 (CLI); controller-gui.log for the window
```

## Step-by-step connection check

Run these **on the i3**, replacing the address with your i7's:

```powershell
ping 192.168.1.4                                     # 1. can the i3 see the i7 at all?
Test-NetConnection 192.168.1.4 -Port 8765            # 2. is the port reachable? Look for TcpTestSucceeded : True
```

| Result | Likely cause | Fix |
|---|---|---|
| `ping` fails | Different networks, wrong IP, or "AP isolation" / guest Wi-Fi | Put both on the same (non-guest) Wi-Fi. Check the i7's IP |
| `ping` works, port test `False` | Controller not running, firewall, or the network is set to Public | See the next sections |
| Port test `True`, but pairing or connecting fails | Certificate, code or token problem | See [Pairing](#pairing-errors) |

> `ping` can fail even when everything is fine, because Windows blocks ping by default on some
> profiles. The port test is the one that matters.

## On the i7: is the controller listening?

```powershell
Get-NetTCPConnection -LocalPort 8765 -State Listen -ErrorAction SilentlyContinue
```

- **Nothing listed:** the controller isn't running. Click **Start controller**.
- **Listed with LocalAddress `127.0.0.1`:** it's bound to loopback only. In *Settings & help*, choose
  your LAN IP (e.g. `192.168.1.4`), **Save**, then stop and start the controller.
- **"Port already in use" error:** another program uses 8765. Choose another port in Settings, and
  update the firewall rule with `add-firewall-rule.ps1 -Port <new>`.

## On the i7: firewall and network profile

```powershell
Get-NetConnectionProfile                                                     # must be Private for your home network
Get-NetFirewallRule -DisplayName "Network Connector Controller (LAN only)"   # must exist and be Enabled
```

- **NetworkCategory is Public:** the rule (Private only) doesn't apply. Change the network to
  Private: *Settings → Network & internet → Wi-Fi → your network → Private*.
- **Rule missing:** run `scripts\add-firewall-rule.ps1` as administrator.

### Python block rule

If you once clicked **Cancel** on a Windows Firewall pop-up for Python, Windows created a **block**
rule, and block rules win over allow rules. List them:

```powershell
Get-NetFirewallRule -Direction Inbound -Action Block | Where-Object DisplayName -like "*python*" |
    Format-Table DisplayName, Profile, Enabled
```

To fix it, open *Windows Defender Firewall with Advanced Security* → *Inbound Rules*, find the
"Python" rules with a red block icon, and disable them or set them to *Allow* (needs admin).
Alternatively, as administrator: `Get-NetFirewallRule -Direction Inbound -Action Block | Where-Object DisplayName -like "*python*" | Disable-NetFirewallRule`

### Third-party antivirus

Antivirus suites with their own firewall (Norton, McAfee, Kaspersky, …) ignore Windows rules.
Add an allow rule there for TCP 8765 from your local network only.

## Pairing errors

See the table in [PAIRING.md](PAIRING.md#if-pairing-fails). Most often: the code expired (10 min),
the code was already used, or 5 wrong attempts locked it. Create a new code.

## Worker shows "Offline — retrying connection"

The worker keeps retrying (1 s up to every 30 s) and keeps running its current jobs. Check:

1. Is the controller running? Is the i7 asleep? (*Settings → System → Power*: set "when plugged in, put my
   device to sleep" to Never while working.)
2. Has the i7's IP changed? See below.
3. Are you on the same Wi-Fi?

### The i7's IP address changed

The worker connects to the address it was paired with. Either:

- reserve a fixed IP for the i7 in your router (recommended), or
- on the i3, edit `controller_host` in `%LOCALAPPDATA%\NetworkConnector\worker\worker.json`. The
  certificate is pinned, not the address, so this is safe. Then restart the worker.

## "Unknown or revoked worker. Pair this worker again."

The worker was revoked, or the controller's data folder was reset. Pair again with a new code.

## Certificate errors (`CERTIFICATE_VERIFY_FAILED`)

The controller's certificate changed: it was regenerated, or the data folder was deleted. That's
exactly what pinning is designed to catch. If **you** caused it, re-pair the worker: click *Forget pairing…*,
then pair. If you didn't, something may be impersonating your controller. Don't re-pair until
you know why.

## Jobs fail with "exceeded memory limit" or "exited unexpectedly"

Increase *Max memory per job* on that worker, or make the chunks smaller (more chunks).

## The laptop feels slow while working

Lower *Max CPU threads* / *Max jobs at once* by one, and keep *Low priority* ticked. Use **Pause**
while you need the laptop for something else.

## Jobs stay "queued"

- No worker is online, or all workers are paused or disabled (check the Dashboard).
- The job is pinned to a specific worker (*Run on*) that's offline.
- The worker's RAM is above 92%; it won't start new jobs until memory frees up.

## Setup script problems

- **"running scripts is disabled on this system":** start it the way the guide shows:
  `powershell -ExecutionPolicy Bypass -File .\setup-worker.ps1`
- **pip errors / no internet:** the first install downloads packages from PyPI. Retry
  on a working connection.
- **`git clone` asks for a password:** the repository is private. See [SETUP_I3.md](SETUP_I3.md#private-repository).

## Reset

To start fresh on one computer, stop everything and rename `%LOCALAPPDATA%\NetworkConnector`
(for example to `NetworkConnector.old`). On the i7 this creates a new certificate, so all workers must re-pair.
