# Read-only facts for the report. Run on each PC; changes nothing.
$ErrorActionPreference = 'Continue'
$out = Join-Path $PSScriptRoot 'out'
New-Item -ItemType Directory -Force $out | Out-Null
$log = Join-Path $out ("inspect-{0}-{1}.txt" -f $env:COMPUTERNAME, (Get-Date -Format 'yyyyMMdd-HHmmss'))
& {
    netsh wlan show interfaces; netsh wlan show drivers; netsh wlan show wirelesscapabilities
    & "$PSScriptRoot\.venv\Scripts\python.exe" -u "$PSScriptRoot\mcast_spike.py" selftest
    & "$PSScriptRoot\.venv\Scripts\python.exe" -u "$PSScriptRoot\radio_spike.py"
    & "$PSScriptRoot\.venv\Scripts\python.exe" -u "$PSScriptRoot\wifi_state_spike.py"
    & "$PSScriptRoot\.venv\Scripts\python.exe" -u "$PSScriptRoot\p2p_enum_spike.py"
} 2>&1 | Tee-Object -FilePath $log
Write-Host "Saved $log (contains local SSIDs/MAC addresses; review before sharing)."
