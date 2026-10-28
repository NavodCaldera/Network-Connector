# Creates spikes\0.3-network\.venv (separate from the project venv) and installs the pinned spike packages.
# Run once on each PC while it still has internet.
$ErrorActionPreference = 'Stop'
$venv = Join-Path $PSScriptRoot '.venv'
if (-not (Test-Path "$venv\Scripts\python.exe")) {
    & "$PSScriptRoot\..\..\.venv\Scripts\python.exe" -m venv $venv
}
& "$venv\Scripts\python.exe" -m pip install -r "$PSScriptRoot\requirements-spike.txt"
& "$venv\Scripts\python.exe" -m pip freeze
