param([ValidateSet('inspect', 'direct', 'hotspot')][string]$Mode = 'inspect')
$ErrorActionPreference = 'Stop'
& "$PSScriptRoot\..\.venv\Scripts\python.exe" "$PSScriptRoot\check-link.py" worker --mode $Mode
exit $LASTEXITCODE
