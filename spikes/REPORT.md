# Phase 0 hardware report

Hardware decision: **pending evidence**. Neither mode A nor mode B is certified on these laptops.
Following the user's subsequent instruction to build the system, Phase 2 software is
implemented ahead of the original hardware gate. This is an explicit deviation from
PLAN §4: mocked/software tests do not establish radio support, concurrency, privileges
or offline hotspot availability. The software reports capability failures at runtime
and offers the manually started Mobile Hotspot fallback. Hardware acceptance remains open.
No adapter, IP, concurrency, privilege or throughput result has been assumed.
These scripts are development checks, not shipped application features.

On each real laptop, from the project directory:

```powershell
# i7
.\spikes\check-controller.ps1
# i3
.\spikes\check-worker.ps1
```

Inspect mode only collects read-only commands to timestamped JSON beside this report.
Review the files before sharing: they contain local adapter/network details, but no passwords.
Record exact output from both machines under the evidence table below.

For mode A, install the optional spike dependencies in each project venv before going offline:
`winrt-runtime`, `winrt-Windows.Devices.WiFiDirect`, `winrt-Windows.Security.Credentials`,
and `winrt-Windows.Foundation`. Record the installed versions with `python -m pip freeze`.
The optional software extra currently pins the 3.2.1 package family, which provides
Python 3.10-compatible wheels. These pins are provisional until tested on the laptops;
they are based on package availability, not a completed hardware spike.

Run `check-controller.ps1 -Mode direct` on the i7, then
`check-worker.ps1 -Mode direct` on the i3. These scripts ask before changing Wi-Fi.
Enter the same DIRECT-NC-xxxx SSID and alphanumeric password on both machines.
Passwords are entered privately and are not included in diagnostic reports.
Keep the controller script open while testing. The worker profile is removed when its
interactive test exits; reconnect its ordinary Wi-Fi manually afterward.

For mode B, stop the direct publisher first, then run the controller script with
`-Mode hotspot`. Start Mobile Hotspot manually in Settings, with no internet connection
to share. Run the worker script with `-Mode hotspot` if the hotspot starts.
Stop the hotspot manually afterward. Do not run both modes simultaneously.

| Required observation | i7 exact evidence | i3 exact evidence |
|---|---|---|
| Hosted network, radio types, Wireless Display driver support | pending | pending |
| GO/client/concurrent Wi-Fi Direct capabilities | pending | pending |
| Python package versions; normal vs elevated execution | pending | pending |
| Publisher status; SSID visible and join successful | pending | pending |
| Direct adapter description, IPv4, subnet and gateway | pending | pending |
| Home Wi-Fi remains connected during direct operation | pending | pending |
| Network category; commands work without admin | pending | pending |
| Publisher stop removes group | pending | pending |
| Publisher process exit / garbage collection removes group | pending | pending |
| 10-ping RTT distribution | pending | pending |
| 10 MB transfer duration and throughput both directions | pending | pending |
| Mobile Hotspot with no internet available | pending | pending |

Use an existing local file-transfer facility for the 10 MB test, recording file size,
elapsed time and direction; do not expose a new application listening port on the worker.
The scripts intentionally do not open a transfer server or modify firewall rules.
For process-exit lifetime testing, run a second publisher trial and close only that spike
process; record SSID disappearance from the worker. No production controller is stopped.

Choose A only after successful direct join and measured concurrency/lifecycle checks.
Choose B only after successful offline hotspot and join checks. If both fail, report
the failures and retain ordinary LAN operation.

API references: [Microsoft legacy settings](https://learn.microsoft.com/en-us/uwp/api/windows.devices.wifidirect.wifidirectadvertisement.legacysettings),
[Microsoft desktop legacy AP sample](https://github.com/microsoft/Windows-classic-samples/tree/main/Samples/WiFiDirectLegacyAP).
