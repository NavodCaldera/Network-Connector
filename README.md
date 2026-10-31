# Network Connector

Two Windows PCs, no router: one starts a Wi-Fi Direct group, the other joins it, and each sends the other
`Hello from <computer name>`.

This is the stripped-down version (0.4.0). The full app (jobs, sandbox, benchmarks,
pairing, GUI) is on branch `claude/great-davinci-bch39n`.

## The easy way (window)

1. On both PCs, double-click **`Start.cmd`** in this folder. The first time, it installs the app, which needs
   internet once.
2. **PC 1:** click **Host**. If Windows asks for permission (firewall), click **Yes**.
3. **PC 2:** click **Join the other PC**.

Both windows show each Hello as it passes (`->` sent, `<-` received). On PC 2, **Send Hello** sends another one.
Closing the window on PC 1 stops the group.

## Command line

Install (both PCs, while you still have internet):

```powershell
py -m venv .venv
.\.venv\Scripts\python.exe -m pip install -e ".[wifidirect,test]"
```

**PC 1 (host):**

```powershell
.\.venv\Scripts\nc host
```

It starts a group named `DIRECT-NC-XXXX` (fixed for this PC) and waits. Turn off Windows Mobile Hotspot first, because
it uses the same adapter. If Windows says "aborted", wait about 10 seconds and run it again.
If Windows Firewall would block the other PC, it asks to fix that: click **Yes** on the Windows admin prompt.

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
