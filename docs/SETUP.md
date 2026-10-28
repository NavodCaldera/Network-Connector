# Install Network Connector on any PC (0.3.0)

One installer for every role. You choose what the PC does when the app opens: **Give performance** (helper),
**Ask for performance** (requester) or **Both**. You can change it later with **Change roles**.

No administrator rights are needed, except for the optional firewall rules at the end.

## 1. Install

In **Windows PowerShell**:

```powershell
cd $HOME\Downloads
Invoke-WebRequest https://raw.githubusercontent.com/NavodCaldera/Network-Connector/main/scripts/setup-worker.ps1 -OutFile setup-worker.ps1
powershell -ExecutionPolicy Bypass -File .\setup-worker.ps1 -App
```

The script checks for Python 3.10+ and Git (and asks before installing them), downloads the newest release into
`%LOCALAPPDATA%\NetworkConnector\app`, installs it with the app extras (tray icon, Windows messages, Wi-Fi controls),
offers to set up the Wasm sandbox and a desktop shortcut, and opens the app. Read the script first if you like; it is
short and commented. Already have a checkout? `pip install -e .[app]` and run `network-connector`.

## 2. Choose a role

- **Give performance:** the app asks to set up the sandbox if it isn't yet, then lists nearby requesters. With the
  default setting it joins open requesters by itself and tells you ("Navod's i7 (AB12-CD34) is using 4 of your CPU
  threads"), with a **Stop** button. Set *When a requester asks* to **Ask me each time** for Accept / Decline instead.
- **Ask for performance:** start the requester; nearby helpers see it within a few seconds. Choose who may join in the
  **Devices** tab ([JOINING.md](JOINING.md)).
- **Both:** both tabs in one window.

The app starts in the tray at login (this user only; untick **Start at login** or run `nc app autostart off`). Closing
the window keeps it in the tray. The tray menu has the status, **Pause**, **Stop helping**, **Open** and **Quit**.
**Stop helping** kills every running job within a second and leaves an open group.

## 3. Optional: firewall rules (administrator)

- **Helpers:** nearby announcements arrive on UDP 8766. If the list stays empty on a direct link or hotspot, click
  **Allow discovery through firewall** in the Nearby box, or run as administrator:
  `scripts\add-firewall-rule.ps1 -Discovery -Program <base python.exe> [-LocalAddress <direct-link IP>]`.
  It allows UDP 8766 for that one Python only, on Private networks and, with `-LocalAddress`, on the direct link's
  subnet. **Search** works without it.
- **Requesters:** `scripts\add-firewall-rule.ps1` (TCP 8765, Private, local subnet), as in [SETUP_I7.md](SETUP_I7.md).

## Undo

`nc app reset` removes the login entry and the message registration. `scripts\remove-firewall-rule.ps1` (as
administrator) removes the rules. Your identity, pairings and settings stay in `%LOCALAPPDATA%\NetworkConnector`.

The Part 1 guides ([SETUP_I7.md](SETUP_I7.md), [SETUP_I3.md](SETUP_I3.md), [PAIRING.md](PAIRING.md)) still work for
a closed group with pairing codes.
