<#
.SYNOPSIS
  Install (or update) the Network Connector worker on the helper laptop (e.g. the i3).

.DESCRIPTION
  1. Checks for Python 3.10+ and Git (offers per-user winget installs; asks first).
  2. Downloads the project from GitHub into %LOCALAPPDATA%\NetworkConnector\app
     (newest release tag if one exists).
  3. Creates an isolated Python environment (.venv) and installs dependencies.
  4. Asks for the controller address, pairing code and a worker name, then pairs.
  5. Starts the worker ONLY if you type "yes".

  No administrator rights are needed. Nothing outside %LOCALAPPDATA%\NetworkConnector is
  changed, except an optional desktop shortcut that you are asked about.

  -App (0.3.0, any role): installs the one app with its tray and message extras instead, skips
  pairing (helpers join nearby requesters with one click), offers to set up the Wasm sandbox
  (no admin) and opens the app, where you choose Give performance, Ask for performance or Both.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File .\setup-worker.ps1 -RepoUrl https://github.com/NavodCaldera/Network-Connector.git
.EXAMPLE
  powershell -ExecutionPolicy Bypass -File .\setup-worker.ps1 -App
#>
param(
    [string]$RepoUrl = "",
    [string]$InstallDir = "$env:LOCALAPPDATA\NetworkConnector\app",
    [string]$ControllerAddress = "",
    [string]$WorkerName = "",
    [switch]$SkipPairing,
    [switch]$App
)
if ($App) { $SkipPairing = $true }
$ErrorActionPreference = "Stop"

# Edit this line before publishing so people do not have to type the URL:
$DefaultRepoUrl = "https://github.com/NavodCaldera/Network-Connector.git"

function Say($text, $color = "Cyan") { Write-Host $text -ForegroundColor $color }
function Ask-YesNo($question) {
    $answer = Read-Host "$question (yes/no)"
    return $answer.Trim().ToLower() -in @("y", "yes")
}
function Find-Python {
    foreach ($candidate in @(@("py", "-3"), @("python"))) {
        try {
            $exe = $candidate[0]; $extra = @($candidate | Select-Object -Skip 1)
            $ver = & $exe @extra -c "import sys; print('%d.%d' % sys.version_info[:2])" 2>$null
            if ($LASTEXITCODE -eq 0 -and $ver) {
                $parts = $ver.Trim().Split(".")
                if ([int]$parts[0] -eq 3 -and [int]$parts[1] -ge 10) { return ,$candidate }
            }
        } catch { }
    }
    return $null
}

Say "=== Network Connector worker setup ==="

# ---------------------------------------------------------------- Python
$py = Find-Python
if (-not $py) {
    Say "Python 3.10 or newer was not found." "Yellow"
    if ((Get-Command winget -ErrorAction SilentlyContinue) -and (Ask-YesNo "Install Python 3.12 for your user only with winget?")) {
        winget install -e --id Python.Python.3.12 --scope user --accept-package-agreements --accept-source-agreements
        $env:Path = [Environment]::GetEnvironmentVariable("Path", "User") + ";" + [Environment]::GetEnvironmentVariable("Path", "Machine")
        $py = Find-Python
    }
    if (-not $py) { throw "Please install Python 3.10+ from https://www.python.org/downloads/ (tick 'Add to PATH'), then run this script again." }
}
$pyExe = $py[0]; $pyArgs = @($py | Select-Object -Skip 1)
Say "Using Python: $(& $pyExe @pyArgs --version)"

# ---------------------------------------------------------------- Download / update code
if (-not $RepoUrl) { $RepoUrl = $DefaultRepoUrl }
$hasGit = [bool](Get-Command git -ErrorAction SilentlyContinue)
if (-not $hasGit -and (Get-Command winget -ErrorAction SilentlyContinue)) {
    if (Ask-YesNo "Git is not installed. It is needed for one-click updates. Install Git with winget?") {
        winget install -e --id Git.Git --accept-package-agreements --accept-source-agreements
        $env:Path = [Environment]::GetEnvironmentVariable("Path", "User") + ";" + [Environment]::GetEnvironmentVariable("Path", "Machine")
        $hasGit = [bool](Get-Command git -ErrorAction SilentlyContinue)
    }
}

if (Test-Path "$InstallDir\.git") {
    Say "Existing installation found; fetching the newest release (your settings are not touched) ..."
    git -C $InstallDir fetch --tags --quiet origin
    if ($LASTEXITCODE -ne 0) { throw "git fetch failed. Check your internet connection." }
} else {
    if (-not $RepoUrl) { $RepoUrl = Read-Host "GitHub repository URL (e.g. https://github.com/NavodCaldera/Network-Connector.git)" }
    if (-not $RepoUrl.StartsWith("https://")) { throw "Please use an https:// GitHub URL." }
    if (Test-Path $InstallDir) {
        throw "$InstallDir already exists but is not a Git checkout. Move or rename it, then run this script again."
    }
    New-Item -ItemType Directory -Force -Path (Split-Path $InstallDir) | Out-Null
    if ($hasGit) {
        Say "Cloning $RepoUrl ..."
        git clone --quiet $RepoUrl $InstallDir
        if ($LASTEXITCODE -ne 0) { throw "git clone failed. If the repository is private, sign in first (see docs\SETUP_I3.md)." }
    } else {
        Say "Git not available: downloading a ZIP instead (self-update will not be available)." "Yellow"
        $zipUrl = ($RepoUrl -replace "\.git$", "") + "/archive/refs/heads/main.zip"
        $zip = Join-Path $env:TEMP "network-connector-main.zip"
        Invoke-WebRequest -Uri $zipUrl -OutFile $zip -UseBasicParsing
        $tmp = Join-Path $env:TEMP ("nc-extract-" + [guid]::NewGuid())
        Expand-Archive -Path $zip -DestinationPath $tmp
        Move-Item -Path (Get-ChildItem $tmp | Select-Object -First 1).FullName -Destination $InstallDir
        Remove-Item $zip, $tmp -Recurse -Force
    }
}
if (Test-Path "$InstallDir\.git") {
    $tag = git -C $InstallDir tag -l "v*" --sort=-v:refname | Where-Object { $_ -match '^v\d+\.\d+\.\d+$' } | Select-Object -First 1
    if ($tag) {
        Say "Using release $tag"
        git -C $InstallDir checkout --quiet --detach $tag
    } else {
        Say "No release tag published yet; using the default branch." "Yellow"
    }
}

# ---------------------------------------------------------------- Virtual environment
$venvPy = "$InstallDir\.venv\Scripts\python.exe"
if (-not (Test-Path $venvPy)) {
    Say "Creating isolated Python environment ..."
    & $pyExe @pyArgs -m venv "$InstallDir\.venv"
    if ($LASTEXITCODE -ne 0) { throw "Could not create the virtual environment." }
}
Say "Installing dependencies (this can take a minute) ..."
$target = if ($App) { "$InstallDir[app]" } else { $InstallDir }
& $venvPy -m pip install --quiet --disable-pip-version-check -e $target
if ($LASTEXITCODE -ne 0) { throw "Dependency installation failed. See docs\TROUBLESHOOTING.md." }
& $venvPy -m network_connector selftest
if ($LASTEXITCODE -ne 0) { throw "Self-test failed." }
$nc = "$InstallDir\.venv\Scripts\nc.exe"

# ---------------------------------------------------------------- Pairing
if (-not $SkipPairing) {
    Say ""
    Say "On the i7, open Network Connector, click 'Start controller', then the 'Pair a worker' tab and 'Create pairing code'."
    if (-not $ControllerAddress) { $ControllerAddress = Read-Host "Controller address shown on the i7 (e.g. 192.168.1.4:8765)" }
    $code = Read-Host "Pairing code (e.g. ABCD-EFGH-JKMN)"
    if (-not $WorkerName) {
        $WorkerName = Read-Host "Name for this worker [press Enter for '$($env:COMPUTERNAME.ToLower())']"
        if (-not $WorkerName) { $WorkerName = $env:COMPUTERNAME.ToLower() }
    }
    & $nc worker pair --controller $ControllerAddress --code $code --name $WorkerName
    if ($LASTEXITCODE -ne 0) { throw "Pairing failed. See docs\TROUBLESHOOTING.md (firewall, address, expired code)." }
}

# ---------------------------------------------------------------- The one app (any role)
if ($App) {
    if (Ask-YesNo "Set up the Wasm sandbox now? Needed to help others; no administrator rights") {
        & $nc sandbox setup
        if ($LASTEXITCODE -ne 0) { Say "Sandbox setup did not finish; the app offers it again when you choose Give performance." "Yellow" }
    }
    if (Ask-YesNo "Create a desktop shortcut for Network Connector?") {
        $shell = New-Object -ComObject WScript.Shell
        $lnk = $shell.CreateShortcut([Environment]::GetFolderPath("Desktop") + "\Network Connector.lnk")
        $lnk.TargetPath = "$InstallDir\.venv\Scripts\pythonw.exe"
        $lnk.Arguments = "-m network_connector.app"
        $lnk.WorkingDirectory = $InstallDir
        $lnk.Save()
        Say "Shortcut created."
    }
    Say "Installed version: $(& $nc version)" "Green"
    Say "Opening Network Connector: choose Give performance, Ask for performance or Both." "Green"
    Start-Process "$InstallDir\.venv\Scripts\pythonw.exe" -ArgumentList "-m", "network_connector.app"
    return
}

# ---------------------------------------------------------------- Shortcut (optional)
if (Ask-YesNo "Create a desktop shortcut for the worker window?") {
    $shell = New-Object -ComObject WScript.Shell
    $lnk = $shell.CreateShortcut([Environment]::GetFolderPath("Desktop") + "\Network Connector Worker.lnk")
    $lnk.TargetPath = "$InstallDir\.venv\Scripts\pythonw.exe"
    $lnk.Arguments = "-m network_connector.worker_gui"
    $lnk.WorkingDirectory = $InstallDir
    $lnk.Save()
    Say "Shortcut created."
}

Say ""
Say "Installed version: $(& $nc version)" "Green"
Say "Settings, token and pinned certificate: $env:LOCALAPPDATA\NetworkConnector\worker (never inside the Git folder)" "Green"
if (Ask-YesNo "Start the worker now?") {
    Say "Choose: [1] worker window (recommended)  [2] in this console"
    if ((Read-Host "1 or 2") -eq "2") { & $nc worker run } else { Start-Process "$InstallDir\.venv\Scripts\pythonw.exe" -ArgumentList "-m", "network_connector.worker_gui" }
} else {
    Say "Not started. Later, use the desktop shortcut or run:  & '$nc' worker run"
}
