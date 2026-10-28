# Phase 6 network spikes (0.3.0): report

Scope: PLAN §21 Phase 6, network items. Date 2026-10-28, `release/0.2.0` at `1f6b665`, nothing committed. Spike code only.
Raw outputs are in `out/` (git-ignored; they contain local SSIDs, BSSIDs and MACs). Spike packages are in `spikes/0.3-network/.venv`.

**i7:** DESKTOP-VMRF6PJ, Intel Wi-Fi 6 AX203 (driver 23.160.0.4), Win 11 Pro 10.0.26200, Python 3.12.9, not elevated,
on Wi-Fi `navod` (2.4 GHz, Private, 10.63.182.8/24). Its Wi-Fi was not switched and no radio was turned off.
**No i3 result exists yet.**

## Summary

| # | Item | Verdict | Consequence |
|---|---|---|---|
| 1a | Announcement sockets on one PC | **GO** (i7) | One exclusive receive-only socket per interface; drop our own device_id |
| 1b | Across PCs on a home router | **PENDING-hardware** | Test A |
| 1c | On the Wi-Fi Direct group | **PENDING-hardware**, firewall risk | On Public links, use the Part 1 outbound gateway probe |
| 1d | On Mobile Hotspot | **PENDING-hardware** | Same as 1c |
| 2 | Auto-join DIRECT-NC-* while on another Wi-Fi | **PENDING-hardware**; i7 adapter points to **FALLBACK** | Join only when not on Wi-Fi; assume no concurrency unless measured |
| 2b | Find groups / read current Wi-Fi without netsh text | **GO** (i7) | WinRT WiFiAdapter + NetworkInformation |
| 3 | pystray next to tkinter | **GO** (API); visual check and clicks PENDING | run_detached; menu actions go to tk through a queue |
| 4 | Toast from unpackaged Python | **GO with an explicit AUMID**; button clicks PENDING | Per-user AUMID; buttons only work while the app runs |
| 5 | Turn the Wi-Fi radio on | **GO** for access and the call path (no admin); off-to-on PENDING | Turn-on button on user click only, never off |

## 1. Multicast and broadcast (§15.1–15.2)

**Question:** can we send the signed announcement to 239.255.78.67:8766 plus a broadcast, and can a receive-only socket get it?
Does it fit 1200 B? Which bind is safe? Does it cross the router, the direct group and the hotspot?

**Ran:** `mcast_spike.py selftest`, plus a router-mode dry run of `requester_spike.py` + `helper_spike.py --mode listen`.
Senders bind one socket per interface address, TTL 1, with a real ECDSA P-256 signature.

```
Interfaces: [('vEthernet (WSL (Hyper-V firewall))','172.21.224.1',20), ('Wi-Fi 2','10.63.182.8',24), ('Loopback Pseudo-Interface 1','127.0.0.1',8)]
Signed announcement size: 463 bytes (limit 1200); cryptography=yes
('bound:10.63.182.8','10.63.182.8',487,'... dest=multicast ...')  ('bound:10.63.182.8',...,491,'dest=limited-bcast')  ('bound:10.63.182.8',...,491,'dest=subnet-bcast')
('bound:127.0.0.1','127.0.0.1',483,'dest=multicast')  ('bound:172.21.224.1','172.21.224.1',487,'dest=multicast')
second bind (SO_REUSEADDR=True) refused: [WinError 10013]; second bind (SO_REUSEADDR=False) refused: [WinError 10048]
wildcard (exclusive=False) then specific bind SUCCEEDED -> more-specific socket can steal
wildcard (exclusive=True) then specific bind refused: [WinError 10013]
Wildcard socket: receives from 10.63.182.8, 127.0.0.1 and 172.21.224.1 mixed together
Dry run, sender IP_MULTICAST_LOOP=0: ('Wi-Fi 2:10.63.182.8','10.63.182.8','multicast'): 4   <- still delivered locally
```

**Firewall (read-only):**
- All profiles are on, with NotifyOnListen True.
- The i7 already has prompt-created rules for `C:\program files\python312\python.exe`: `Python Inbound Block Public TCP/UDP Any` and `Allow Private TCP/UDP Any`.
- The venv's python.exe is a launcher, so the firewall sees `C:\Program Files\Python312\python.exe` (`sys._base_executable`).

**Verdicts:**
- 1a: **GO**.
- 1b, 1c, 1d: **PENDING-hardware**. Risk: direct links are usually Public, and a Block rule beats any Allow rule. Inbound UDP 8766 would
  then be dropped on the i7 when it acts as a helper. The i3's rules are unknown.

**Consequences:**
1. **Helper:** one UDP socket per up interface address, bound to (ip, 8766), with SO_EXCLUSIVEADDRUSE, joining the group on that
   interface. Never a non-exclusive wildcard bind. Per-interface sockets also give the link-type label and let us skip virtual adapters such as WSL.
2. **Requester:** one socket per interface (never 0.0.0.0). Send multicast plus the subnet-directed broadcast; 255.255.255.255 is redundant.
3. **Own announcements:** drop them by device_id. On Windows, IP_MULTICAST_LOOP=0 on the sender does not stop local delivery.
4. **Wi-Fi Direct and hotspot links:** also run the Part 1 outbound `candidate_controllers()` probe and compare the fingerprint with the
   announced device_id. It needs no inbound rule.
5. **Firewall rule:** don't rely on the Windows "Allow access" prompt, which allows any port for all of python.exe.
   - Add a narrow `-Discovery` rule to `add-firewall-rule.ps1`: UDP 8766, program = base python.exe, Private, plus Public
     scoped to the direct subnet.
   - Document that an existing Block rule wins.
   - Show "Discovery blocked by firewall" after 30 s with no announcements on a direct link.
6. **Busy port:** if the port is busy (10048/10013), show "Discovery port busy" and use the probe. A squatter can only hide the list.

## 2. Concurrency (§15.3)

**Ran (read-only):** `netsh wlan show wirelesscapabilities/drivers/interfaces`, `wifi_state_spike.py`, `p2p_enum_spike.py`.

```
Wi-Fi Direct Device/GO/Client : Supported   Soft AP : Not supported   Hosted network supported : No
P2P GO ports count : 1   P2P Clients Port Count : 1   P2P Max Mobile AP Clients : 8
Maximum Number of Secondary STAs Supported : 0   Number of Concurrent Channels Supported : 0
WiFiAdapter.request_access_async -> ALLOWED (not elevated); scan_async ok (1246 ms); 2 SSIDs; DIRECT-* SSIDs: []
name='navod' wlan=True ssid='navod' connectivity=INTERNET_ACCESS; netsh wlan show networks exit 0
ASSOCIATION_ENDPOINT: 0 device(s) in 5872 ms (no admin)
```

**Analysis (to confirm, not measured):**
- A legacy group is joined through the station port. With 0 secondary STAs, the join will most likely replace the current Wi-Fi.
- The only concurrent route the driver offers is the native P2P client port (mode C, not planned).
- Being GO while staying a station looks supported, which is the requester's case.

**Verdicts:**
- Concurrency: **PENDING-hardware** (Test B). The evidence points to FALLBACK.
- 2b: **GO**.

**Consequences:**
1. Auto-join only when no WLAN profile is connected. Check that with WinRT `NetworkInformation.get_connection_profiles()`
   (`is_wlan_connection_profile`, `get_connected_ssid()`), not localized netsh text. Treat "supports both" as false unless measured.
2. When the helper is on Wi-Fi, offer one click, "Join (disconnects <SSID>)". Never join automatically.
3. Find groups with `WiFiAdapter.scan_async()` filtered by `^DIRECT-NC-[A-Z0-9]{4}$`, at most every 30 s.
4. Prefer `connect_with_password_credential_async(network, MANUAL, PasswordCredential)`, which keeps the passphrase off disk, if
   Test B shows it works. Keep netsh as the fallback. `leave()` must delete the profile Windows creates.
5. **Location:** both scan paths worked here. Where desktop location access is off, expect DENIED and show an "Allow location access" hint (unverified).

## 3. pystray (§13)

**Ran:** `tray_spike.py --seconds 8`: tk root on the main thread, pystray via `run_detached()`.

```
pystray backend: pystray._win32
run_detached returned after 4 ms; threads: ['MainThread','Thread-1 (setup_handler)','Thread-2 (<lambda>)']
icon.visible=True HAS_NOTIFICATION=True HAS_MENU=True HAS_DEFAULT_ACTION=True
icon.notify() returned without error
icon.stop() took 1 ms; tk main loop ended normally
```

**Verdict:** **GO** for the API (pystray 0.19.5 + six + pillow, no admin). Whether the icon and balloon actually showed, and the menu clicks:
**PENDING** (`run-ui-checks.ps1 -Check tray`).

**Consequences:**
1. Use `run_detached()` in the `app.py` process. Menu callbacks only enqueue actions to a `queue.Queue` that tk polls with `after(100)`.
   Stop calls `sandbox.stop_all()` directly from the queue handler.
2. Dynamic labels and swapping the icon image work.
3. pystray is LGPL-3.0, which needs coordinator approval.

## 4. Toasts

**Ran:** `toast_spike.py --wait 12` with three cases:
- A: no AUMID;
- B: an unregistered AUMID;
- C: HKCU `Software\Classes\AppUserModelId\NetworkConnector.Spike` with a DisplayName, plus Accept/Decline buttons.

Then `toast_clean_spike.py` with a never-used AUMID.

```
[A] create_toast_notifier() without AUMID failed: OSError: [WinError -2147023728] Element not found.
[B, clean] notifier.setting failed: Element not found; show() returned; no event within 12 s
clean AUMID: absent before run; setting failed: Element not found; show returned; history items: 1; setting after show: ENABLED
[C] notifier.setting = ENABLED; show() returned; no event within 12 s; history: 2 item(s) tags=['C','B']; history cleared; key deleted
```

Windows also created `HKCU\...\CurrentVersion\Notifications\Settings\NetworkConnector.Spike`. It was deleted, and `reg query` confirms both spike keys are gone.

**Verdict:** **GO with an explicit AUMID**, no admin, and Windows accepted the toasts.
- **PENDING:** what appeared on screen, and whether clicking Accept fires Activated. Nobody clicked, and a reminder toast stays until it is clicked.
- Clicking after the process has exited (COM activation) was not tested and is not planned.

**Consequences:**
1. On first run, register `NetworkConnector.App` per user (DisplayName, IconUri). Remove it and the Notifications\Settings key on reset or uninstall.
2. Use `create_toast_notifier_with_id` (the pywinrt 3.x name). If `setting` is not ENABLED, fall back to `pystray notify()`, then to the app window.
3. Ask-me only works while the app runs (the tray guarantees that), and the request is also shown in the app. With no answer it
   times out to Decline. Accept only `accept:<id>` or `decline:<id>` as arguments.
4. Toasts show the name, short ID and threads only (invariant 11). Tag and group by requester.

## 5. Radio

**Ran:** `radio_spike.py` and `--turn-on` (Wi-Fi only; it was already ON, so nothing could change).

```
admin: False
Radio.request_access_async -> ALLOWED
radio name='Bluetooth' kind=BLUETOOTH state=OFF
radio name='Wi-Fi 2' kind=WI_FI state=ON
set_state_async(ON) on 'Wi-Fi 2': before=ON result=ALLOWED after=ON (14 ms)
```

**Verdict:** **GO** for access and the call path. Off-to-on from the Wi-Fi toggle, and from airplane mode: **PENDING**.

**Consequences:**
1. Show "Wi-Fi is off — Turn on" and call `SetStateAsync(On)` only on that click. Never turn a radio off, never touch Bluetooth.
2. If the result is not ALLOWED, or Wi-Fi is still OFF after 5 s, point to Quick Settings or airplane mode.
3. Package: `winrt-Windows.Devices.Radios==3.2.1`.

## 6. Hardware steps

Run these in a normal PowerShell in the project folder. Steps 0 and 1 go on both PCs while online. Outputs go to `spikes\0.3-network\out\`.
Report what you saw, too.

```
# 0. Spike venv, once per PC
powershell -NoProfile -ExecutionPolicy Bypass -File spikes\0.3-network\setup-spike-venv.ps1
# 1. Read-only facts
powershell -NoProfile -ExecutionPolicy Bypass -File spikes\0.3-network\run-inspect.ps1

# A (router): i3 first, then the i7 within 20 s; then swap roles once
powershell -NoProfile -ExecutionPolicy Bypass -File spikes\0.3-network\run-helper.ps1 -Mode listen -Seconds 120      # i3
powershell -NoProfile -ExecutionPolicy Bypass -File spikes\0.3-network\run-requester.ps1 -Mode router -Seconds 100    # i7

# B (direct + concurrency): the i3 starts on its normal Wi-Fi and types START when asked
powershell -NoProfile -ExecutionPolicy Bypass -File spikes\0.3-network\run-requester.ps1 -Mode direct -Seconds 300    # i7, prints the SSID
powershell -NoProfile -ExecutionPolicy Bypass -File spikes\0.3-network\run-helper.ps1 -Mode join -Ssid DIRECT-NC-XXXX              # i3
powershell -NoProfile -ExecutionPolicy Bypass -File spikes\0.3-network\run-helper.ps1 -Mode join -Ssid DIRECT-NC-XXXX -Method netsh  # i3

# C (hotspot): the i7 starts Mobile Hotspot when prompted; the i3 joins from the Wi-Fi menu, then listens
powershell -NoProfile -ExecutionPolicy Bypass -File spikes\0.3-network\run-requester.ps1 -Mode hotspot -Seconds 200   # i7
powershell -NoProfile -ExecutionPolicy Bypass -File spikes\0.3-network\run-helper.ps1 -Mode listen -Seconds 120       # i3

# D (each PC)
powershell -NoProfile -ExecutionPolicy Bypass -File spikes\0.3-network\run-ui-checks.ps1 -Check tray
powershell -NoProfile -ExecutionPolicy Bypass -File spikes\0.3-network\run-ui-checks.ps1 -Check toast
powershell -NoProfile -ExecutionPolicy Bypass -File spikes\0.3-network\run-ui-checks.ps1 -Check radio   # Wi-Fi off first; repeat with airplane mode on
```

- Test B also records whether the i7's own home Wi-Fi survives while the group runs.
- At the end of Test B the helper script deletes its temporary profile and reconnects the original Wi-Fi. If the i3 has no internet
  afterwards, reconnect it by hand.
- No script opens a TCP port, changes a firewall rule or turns a radio off.

## 7. Proposed `tray` extra

Tested on Python 3.12.9 only; 3.10 wheels are not verified:
`pystray==0.19.5, winrt-runtime, winrt-Windows.Foundation, winrt-Windows.Foundation.Collections, winrt-Windows.UI.Notifications,
winrt-Windows.Data.Xml.Dom, winrt-Windows.Devices.Radios, winrt-Windows.Devices.WiFi, winrt-Windows.Networking.Connectivity` (all 3.2.1).
The full frozen set is in `requirements-spike.txt`. Imports stay lazy and degrade to a message.

## 8. Security findings

- **Invariant 5:** the listener is recvfrom-only, per interface and exclusive.
  - **Challenge:** Windows Firewall rules match by program, and that program is the shared base python.exe. Any rule must therefore
    be scoped to UDP 8766 only, never "allow python.exe".
  - Consider a dedicated launcher exe later.
  - The docs should tell users not to tick Public in the Windows prompt.
- **Invariant 1:** the senders bind explicit addresses.
- **Invariant 7:** argument lists only. The SSID and profile name are validated, and PowerShell command strings are constant.
- **Invariant 11:** the derived passphrase is never printed. It only goes into the WinRT credential or an ACL-protected temp file that is deleted at once.
- No invariant was weakened.
