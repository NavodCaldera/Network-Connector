# First-time setup on the main computer (i7, controller)

All commands are PowerShell. Open **Windows PowerShell** (no administrator needed, except step 4).

## 1. Install Python and Git (if you don't have them)

```powershell
py -3 --version        # need 3.10 or newer
git --version
```

If either is missing, install it (per user, no admin):

```powershell
winget install -e --id Python.Python.3.12 --scope user
winget install -e --id Git.Git
```

Close and reopen PowerShell afterwards.

## 2. Get the code

```powershell
cd $HOME\Documents
git clone https://github.com/NavodCaldera/Network-Connector.git
cd Network-Connector
```

## 3. Install

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\setup-controller.ps1
```

This creates `.venv`, installs the dependencies, runs a self-test, and **shows** (without
changing anything) your IP address, network profile and firewall state. `-ExecutionPolicy Bypass`
applies only to this one command; it doesn't change any system setting.

## 4. Network profile and firewall (one time)

1. **Network profile.** If the setup output says your home Wi-Fi is **Public**, change it to
   **Private**: *Settings → Network & internet → Wi-Fi → (your network) → Network profile
   type → Private*. Only do this for your own trusted home network.
2. **Firewall rule.** Open PowerShell **as administrator** (right-click → *Run as administrator*):

   ```powershell
   cd $HOME\Documents\network-connector
   powershell -ExecutionPolicy Bypass -File .\scripts\add-firewall-rule.ps1 -Port 8765
   ```

   This creates one inbound rule: TCP 8765, Private profile only, local subnet only.
   Remove it any time with `scripts\remove-firewall-rule.ps1`.
3. Never set up port forwarding for 8765 on your router.

> If Windows pops up *"Windows Defender Firewall has blocked some features of Python"* the
> first time you start the controller, tick **Private networks** only and click **Allow**.
> Clicking *Cancel* creates a hidden **block** rule. See [TROUBLESHOOTING.md](TROUBLESHOOTING.md#python-block-rule).

## 5. Start the controller

```powershell
.\.venv\Scripts\nc.exe gui
```

(or use the desktop shortcut the setup offered to create.)

In the window:

1. Open **Settings & help**, check that *Listen on* is your LAN address (e.g. `192.168.1.4`),
   and click **Save**.
2. Click **Start controller**. The top bar shows `Controller running at 192.168.1.4:8765`.
3. Optional, recommended: click **Use this PC as a worker too** so the i7 also does work.
   You can set how many CPU threads it uses under *Settings & help*.

Command-line alternative: `.\.venv\Scripts\nc.exe controller start`.

## 6. Find your controller address for the i3

It's shown in the top bar, or run:

```powershell
Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -like "192.168.*" -or $_.IPAddress -like "10.*" }
```

**Tip:** give the i7 a fixed address in your router (a "DHCP reservation"), so the address the i3
was paired with doesn't change. If it does change, see [TROUBLESHOOTING.md](TROUBLESHOOTING.md#the-i7s-ip-address-changed).

## Router-less option

Install the optional controller publisher while online:
`powershell -ExecutionPolicy Bypass -File .\scripts\setup-controller.ps1 -WiFiDirect`.
Start the controller, open **Wi-Fi Direct**, start a group, and allow its narrow
firewall rule using the tab's button. Windows Mobile Hotspot is the fallback when
the publisher is unavailable. See [WIFI_DIRECT.md](WIFI_DIRECT.md) for both modes,
fingerprint comparison and the pending real-adapter acceptance checks.

Next: [set up the i3](SETUP_I3.md).
