# Network Connector

Two Windows PCs, no router: one starts a Wi-Fi Direct group, the other joins it, and each sends the other
`Hello from <computer name>`.

This is the stripped-down version (0.4.0, branch `simple-hello`). The full app (jobs, sandbox, benchmarks,
pairing, GUI) is on branch `claude/great-davinci-bch39n`.

## Install (both PCs, while you still have internet)

```powershell
py -m venv .venv
.\.venv\Scripts\python.exe -m pip install -e ".[wifidirect,test]"
```

## Run

**PC 1 (host)**, once as administrator: `powershell -File scripts\add-firewall-rule.ps1`. Then:

```powershell
.\.venv\Scripts\nc host
```

It prints the group's SSID and password and waits. Turn off Windows Mobile Hotspot first, because it uses the
same adapter.

**PC 2:**

```powershell
.\.venv\Scripts\nc join DIRECT-XXXX <password>
```

PC 2 prints `... replied: Hello from PC1` and PC 1 prints `... says: Hello from PC2`. Ctrl+C on PC 1 stops the group.

`nc send <ip>` and `nc listen` do the same exchange on any network that is already connected.

## Security

The Wi-Fi Direct group is WPA2-protected. The Hello itself is plain, unencrypted TCP on port 8765, and the
firewall rule allows the local subnet only. Don't use it on networks you don't trust.
