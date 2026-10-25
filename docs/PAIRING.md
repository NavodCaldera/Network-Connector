# Pairing the i3 with the i7

Pairing gives the worker its own secret token and makes it trust **only** your controller's
certificate. You do it once per worker.

## Using the windows (recommended)

1. **i7:** in the controller window, click **Start controller** → **Pair a worker** tab →
   **Create pairing code**. You'll see:
   - the code, e.g. `K7QM-2XRA-9FTD` (also copied to the clipboard);
   - the controller address to type, e.g. `192.168.1.4:8765`.
2. **i3:** in the worker window, fill in **Controller address**, **Pairing code** and **Name**,
   then click **Pair**.
3. The i3 shows *Paired as 'i3-laptop' with 192.168.1.4:8765*.
4. **i3:** click **Start worker**. Within a few seconds the i7 dashboard lists the worker as **online**.

## Using the command line

```powershell
# i7
.\.venv\Scripts\nc.exe pair-code

# i3
& "$env:LOCALAPPDATA\NetworkConnector\app\.venv\Scripts\nc.exe" worker pair --controller 192.168.1.4:8765 --code K7QM-2XRA-9FTD --name i3-laptop
```

## Rules

- A code works **once** and expires after **10 minutes**.
- **5 wrong attempts** lock the code. Create a new one.
- Creating a new code cancels the previous one.
- Pairing again under the same name gives that worker a new token (the old token stops working).
- The code is never saved to disk, and the worker never sends the code itself over the
  network. It sends a cryptographic proof that it knows the code (see
  [ARCHITECTURE.md](ARCHITECTURE.md#pairing-handshake)).

## If pairing fails

| Message | Meaning and fix |
|---|---|
| `No valid pairing code: it expired, was used, or was locked` | Create a new code on the i7 |
| `Pairing code incorrect, or the connection is not to the real controller` | Re-type the code. If it keeps failing with a fresh code, something on the network may be intercepting: stop and check |
| `controller failed to prove it knows the pairing code` | You connected to something that is **not** your controller. Check the address |
| Timeout / `ConnectError` | Firewall or address problem: see [TROUBLESHOOTING.md](TROUBLESHOOTING.md) |
