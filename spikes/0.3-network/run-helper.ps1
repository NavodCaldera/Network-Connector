# HELPER side (the i3). Modes: listen | join. "join" changes the Wi-Fi connection after you type START,
# and reconnects the original Wi-Fi profile at the end. Output goes to spikes\0.3-network\out\.
param([Parameter(Mandatory)][ValidateSet('listen', 'join')][string]$Mode,
      [ValidatePattern('^DIRECT-NC-[A-Z0-9]{4}$')][string]$Ssid,
      [ValidateSet('winrt', 'netsh')][string]$Method = 'winrt', [int]$Seconds = 90)
$ErrorActionPreference = 'Stop'
$a = @("-u", "$PSScriptRoot\helper_spike.py", "--mode", $Mode, "--method", $Method, "--seconds", "$Seconds")
if ($Ssid) { $a += @("--ssid", $Ssid) }
& "$PSScriptRoot\.venv\Scripts\python.exe" @a
exit $LASTEXITCODE
