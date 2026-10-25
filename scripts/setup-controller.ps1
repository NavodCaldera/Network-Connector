<#
.SYNOPSIS
  Prepare the main computer (i7) to run the Network Connector controller.

.DESCRIPTION
  Run from inside the cloned repository. Creates .venv, installs dependencies, runs the
  self-test, and REPORTS (does not change) your network profile and firewall state.
  No administrator rights needed. The firewall rule is a separate, explicit step:
  scripts\add-firewall-rule.ps1 (run as administrator).

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File .\scripts\setup-controller.ps1
#>
param([int]$Port = 8765)
$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
function Say($text, $color = "Cyan") { Write-Host $text -ForegroundColor $color }

Say "=== Network Connector controller setup ==="
$py = $null
foreach ($c in @(@("py", "-3"), @("python"))) {
    try {
        $ok = & $c[0] @($c | Select-Object -Skip 1) -c "import sys; print(sys.version_info >= (3, 10))" 2>$null
        if ($ok -eq "True") { $py = $c; break }
    } catch { }
}
if (-not $py) { throw "Python 3.10+ not found. Install it (winget install -e --id Python.Python.3.12 --scope user) and retry." }

if (-not (Test-Path "$repo\.venv\Scripts\python.exe")) {
    Say "Creating virtual environment ..."
    & $py[0] @($py | Select-Object -Skip 1) -m venv "$repo\.venv"
}
Say "Installing dependencies ..."
& "$repo\.venv\Scripts\python.exe" -m pip install --quiet --disable-pip-version-check -e "$repo[test]"
if ($LASTEXITCODE -ne 0) { throw "pip install failed" }
& "$repo\.venv\Scripts\python.exe" -m network_connector selftest

Say ""
Say "Network check (read-only):"
Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -notlike "127.*" -and $_.IPAddress -notlike "169.254.*" } |
    ForEach-Object { Say ("  {0,-20} {1}" -f $_.InterfaceAlias, $_.IPAddress) "White" }
Get-NetConnectionProfile | ForEach-Object {
    $color = if ($_.NetworkCategory -eq "Public") { "Yellow" } else { "Green" }
    Say ("  Network '{0}' on {1}: {2}" -f $_.Name, $_.InterfaceAlias, $_.NetworkCategory) $color
    if ($_.NetworkCategory -eq "Public") {
        Say "    Public networks block incoming connections. If this is YOUR home network, set it to Private:" "Yellow"
        Say "    Settings > Network & internet > Wi-Fi > $($_.Name) > Network profile type > Private" "Yellow"
    }
}
$rule = Get-NetFirewallRule -DisplayName "Network Connector Controller (LAN only)" -ErrorAction SilentlyContinue
if ($rule) { Say "  Firewall rule: present" "Green" } else {
    Say "  Firewall rule: missing. Run once, as administrator:" "Yellow"
    Say "    powershell -ExecutionPolicy Bypass -File `"$repo\scripts\add-firewall-rule.ps1`" -Port $Port" "Yellow"
}

$answer = Read-Host "Create a desktop shortcut for the controller window? (yes/no)"
if ($answer.Trim().ToLower() -in @("y", "yes")) {
    $shell = New-Object -ComObject WScript.Shell
    $lnk = $shell.CreateShortcut([Environment]::GetFolderPath("Desktop") + "\Network Connector.lnk")
    $lnk.TargetPath = "$repo\.venv\Scripts\pythonw.exe"
    $lnk.Arguments = "-m network_connector.gui"
    $lnk.WorkingDirectory = $repo
    $lnk.Save()
}
Say ""
Say "Done. Open the controller window with:  & `"$repo\.venv\Scripts\nc.exe`" gui" "Green"
