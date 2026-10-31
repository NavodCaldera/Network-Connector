# Network Connector

Two Windows PCs, no router: one starts a Wi-Fi Direct group, the other joins it, and each sends the other
`Hello from <computer name>`.

This is the stripped-down version (0.4.0). The full app (jobs, sandbox, benchmarks,
pairing, GUI) is on branch `claude/great-davinci-bch39n`.

## Install (both PCs, while you still have internet)

```powershell
py -m venv .venv
.\.venv\Scripts\python.exe -m pip install -e ".[wifidirect,test]"
```

## Run

**PC 1 (host)**, once as administrator: `powershell -File scriptsdd-firewall-rule.ps1`. Then:

```powershell
.\.venv\Scripts\nc host
```

It starts a group named `DIRECT-NC-XXXX` (fixed for this PC) and waits. Turn off Windows Mobile Hotspot first, because
it uses the same adapter. If Windows says "aborted", wait about 10 seconds and run it again.

**PC 2:**

```powershell
.\.venv\Scripts\nc join
```

It finds the nearby `DIRECT-NC-` group, joins with the built-in password and sends Hello. PC 2 prints
`... replied: Hello from PC1` and PC 1 prints `... says: Hello from PC2`. Ctrl+C on PC 1 stops the group.

`nc join <name>` picks one group when several are nearby. `nc host --ssid X --passphrase Y` and `nc join X Y` use
your own name and password instead. `nc send <ip>` and `nc listen` do the exchange on any network that is
already connected.

## Security

The Wi-Fi Direct group is WPA2-protected, but its built-in password is the same in every copy of the app, so anyone nearby with the app can join. The Hello itself is plain, unencrypted TCP on port 8765, and the
firewall rule allows the local subnet only. Don't use it on networks you don't trust.
