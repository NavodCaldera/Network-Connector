# First-time setup on the helper computer (i3, worker)

No administrator rights are needed. The worker opens no network ports.

## 1. Download the setup script

Open **Windows PowerShell** on the i3:

```powershell
cd $HOME\Downloads
Invoke-WebRequest https://raw.githubusercontent.com/NavodCaldera/Network-Connector/main/scripts/setup-worker.ps1 -OutFile setup-worker.ps1
```

Before running it, you can open `setup-worker.ps1` in Notepad and read it. It's short and commented.

## 2. Have the controller ready

On the i7: start the controller window, click **Start controller**, go to **Pair a worker** and
click **Create pairing code**. Keep that screen open. The code is valid for 10 minutes.

## 3. Run the setup

```powershell
powershell -ExecutionPolicy Bypass -File .\setup-worker.ps1 -RepoUrl https://github.com/NavodCaldera/Network-Connector.git
```

The script:

1. checks for Python 3.10+ and Git, and **asks** before installing either with winget (per user);
2. clones the repository into `%LOCALAPPDATA%\NetworkConnector\app` and switches to the newest
   release tag;
3. creates an isolated environment (`app\.venv`) and installs the dependencies;
4. asks for the **controller address** (e.g. `192.168.1.4:8765`), the **pairing code** and a
   **worker name** (e.g. `i3-laptop`), then pairs;
5. offers a desktop shortcut;
6. starts the worker **only if you answer `yes`**.

### Private repository?

If your GitHub repository is private, sign in once before running the script:

```powershell
winget install -e --id GitHub.cli
gh auth login            # choose GitHub.com, HTTPS, log in with a browser
gh auth setup-git
```

`Invoke-WebRequest` on a raw URL won't work for a private repo. Copy `setup-worker.ps1` over
on a USB stick instead, or run `gh repo clone NavodCaldera/Network-Connector` first and run the
script from its `scripts` folder.

## 4. Use the worker window

Open **Network Connector Worker** from the desktop shortcut, or run:

```powershell
& "$env:LOCALAPPDATA\NetworkConnector\app\.venv\Scripts\nc.exe" worker-gui
```

- **Limits:** *Max CPU threads* and *Max jobs at once* (the i3 default is threads − 1),
  *Max memory per job*, *Stop after idle*, and *Low priority*. Click **Save limits**.
- **Start worker** asks for confirmation, then shows *Online — ready for work*.
- **Pause** stops new jobs from starting; running jobs finish. **Stop** shuts down gracefully,
  and unfinished jobs go back to the queue.

Command-line equivalents:

```powershell
$nc = "$env:LOCALAPPDATA\NetworkConnector\app\.venv\Scripts\nc.exe"
& $nc worker run --max-cpu-threads 3 --max-memory-mb 2048
& $nc worker pause      # from another window
& $nc worker resume
& $nc worker stop
& $nc worker status
```

## Where things are stored

| What | Where |
|---|---|
| Program (Git checkout) | `%LOCALAPPDATA%\NetworkConnector\app` |
| Settings, token, pinned certificate | `%LOCALAPPDATA%\NetworkConnector\worker` |
| Logs | `%LOCALAPPDATA%\NetworkConnector\logs\worker.log` |

## Router-less option

Install this worker while online. In its window, **0. Connect** joins the SSID and
password displayed by the controller. **Find controller** fills in its address and
shows its certificate fingerprint. Compare it on the controller before pairing with
the one-time code. Existing paired workers accept only their saved certificate.
See [WIFI_DIRECT.md](WIFI_DIRECT.md) for setup, reconnection and adapter limitations.

To uninstall, stop the worker, revoke it on the controller, then delete
`%LOCALAPPDATA%\NetworkConnector` and the desktop shortcut.
