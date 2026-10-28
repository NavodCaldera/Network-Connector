# REQUESTER side (the i7). Modes: router | direct | hotspot. Output goes to spikes\0.3-network\out\.
param([Parameter(Mandatory)][ValidateSet('router', 'direct', 'hotspot')][string]$Mode, [int]$Seconds = 240)
$ErrorActionPreference = 'Stop'
& "$PSScriptRoot\.venv\Scripts\python.exe" -u "$PSScriptRoot\requester_spike.py" --mode $Mode --seconds $Seconds
exit $LASTEXITCODE
