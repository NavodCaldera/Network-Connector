# Connecting without a router

The controller can advertise a legacy Wi-Fi Direct group (mode A). The worker joins
it as a normal WPA2 network. An existing Windows Mobile Hotspot is supported as
mode B. Adapter support, concurrent home Wi-Fi and offline hotspot availability
still need validation on the real i7 and i3. Software tests use mocked networking.

## Prepare while online

On the i7, from the project folder:

```powershell
.\.venv\Scripts\python.exe -m pip install -e ".[wifidirect]"
.\.venv\Scripts\nc.exe gui
```

Or run `scripts/setup-controller.ps1 -WiFiDirect` for a fresh installation. Install
the normal worker on the i3 while online. Joining uses Windows `netsh` and does not
need the WinRT publisher packages. Updates require internet access.

## Mode A: controller-created group

1. Start the controller. Open **Wi-Fi Direct** and click **Start group / detect
   hotspot**. Blank SSID and passphrase fields generate fresh credentials.
2. Check the interface and actual IPv4 address. Leave **Listen on the direct link**
   checked. The controller adds that explicit address without changing its certificate.
3. Click **Allow through firewall** and approve the Windows administrator prompt.
   This adds a separate rule for that local address and its /24 peer subnet.
4. On the i3, open the worker window. In **0. Connect**, enter the displayed SSID and
   password and click **Join**. Click **Find controller** after joining.
5. Compare the displayed certificate fingerprint with the controller's Pair tab.
   Create a pairing code on the controller and enter it on the worker. Start the worker.
6. Run the Demo or a small benchmark. The dashboard identifies the link.

The publisher stays inside the controller process. Stopping the group or controller
stops its publisher. Workers retain their pinned certificate and retry known addresses
and direct-link gateway candidates after repeated connection failures.

## Mode B: Windows Mobile Hotspot

If the publisher is unavailable, open **Settings → Network & internet → Mobile
hotspot** on the i7. Configure an SSID and password there and enable it. Windows may
refuse to start a hotspot without an internet connection to share; this varies by
hardware and Windows configuration.

Refresh the controller's Wi-Fi Direct tab, then enable **Listen on the direct link**
or click **Start group / detect hotspot**. Use its detected IP and narrow firewall
button. Join from the worker with the hotspot credentials configured in Settings.
The app cannot read those credentials. Stop a manually started hotspot in Settings.
The app does not configure Internet Connection Sharing or internet routing.

## Command line

Keep the controller running in a separate terminal:

```powershell
.\.venv\Scripts\nc.exe controller start
```

In another terminal on the controller:

```powershell
.\.venv\Scripts\nc.exe wifidirect start
.\.venv\Scripts\nc.exe wifidirect status
.\.venv\Scripts\nc.exe wifidirect stop
```

On the worker:

```powershell
.\.venv\Scripts\nc.exe worker join --ssid DIRECT-NC-AB12
.\.venv\Scripts\nc.exe worker find
```

Join prompts privately for the passphrase. Group credentials are shown in the explicit
group-start output and GUI, not routine CLI status. Paired discovery refuses another
certificate. Pairing still requires the one-time code and certificate-bound proof.

Manual firewall setup, in an administrator PowerShell window:

```powershell
.\scripts\add-firewall-rule.ps1 -WiFiDirect -LocalAddress <actual-controller-IP>
```

Pass `-Port` for a nondefault port. `remove-firewall-rule.ps1` removes both LAN and
direct-link rules; `-WiFiDirect` removes only the direct rule.

## Troubleshooting and hardware acceptance

- **Support not installed:** install the controller's `wifidirect` extra while online,
  or try Mobile Hotspot. Adapter/API errors are reported without assuming support.
- **No interface/IP:** wait for Windows to assign one. Check the Wi-Fi radio and virtual
  adapter. Link-local addresses are supported on detected direct adapters, but must
  still be reachable from the peer.
- **Public/Unidentified network:** use the narrow direct rule. The existing Private
  LAN rule intentionally does not cover Public networks.
- **Home Wi-Fi drops:** record the adapter limitation. Concurrent operation needs a
  real i7 test; software cannot make unsupported hardware concurrent.
- **Discovery fails:** check the actual gateway and controller address. Enter its
  address manually when it is neither a gateway nor the subnet's first host.
- **Certificate mismatch:** verify this is the original controller. Keep its
  certificate; replacing it requires pairing again.

Run `spikes/check-controller.ps1` on the i7 and `spikes/check-worker.ps1` on the i3
for read-only diagnostics. Follow [spikes/REPORT.md](../spikes/REPORT.md) for actual
publisher/hotspot trials and record adapter, IP, privilege, concurrency, latency and
throughput results. Complete PLAN.md §9: reconnect, impostor refusal, demo, a
three-repeat Standard benchmark, shared results, offline export and cancellation.
Verify that no Network Connector worker process has a listening TCP port.
