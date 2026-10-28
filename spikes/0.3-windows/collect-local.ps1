<#
.SYNOPSIS
  Collect local Windows acceptance evidence using isolated app and runtime folders.
  Does not start a controller, change Wi-Fi/firewall/login settings, or sweep profiles.
#>
param(
    [Parameter(Mandatory=$true)][ValidateSet('i7','i3')][string]$PC,
    [switch]$FullTests
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
$python = Join-Path $repo '.venv\Scripts\python.exe'
$nc = Join-Path $repo '.venv\Scripts\nc.exe'
if (-not (Test-Path -LiteralPath $python)) { throw 'Create .venv and install .[test,app] first.' }
$runName = '{0}-{1}-{2}' -f $PC, (Get-Date -Format 'yyyyMMdd-HHmmss'), ([guid]::NewGuid().ToString('N').Substring(0,8))
$out = Join-Path $PSScriptRoot "out\$runName"
New-Item -ItemType Directory -Path $out -Force | Out-Null
$session = [ordered]@{ pc=$PC; home=(Join-Path $out 'home'); runtime=(Join-Path $out 'runtime'); output=$out }
$oldHome, $oldRuntime = $env:NC_HOME, $env:NC_RUNTIME_HOME
$oldNoProxy = $env:NO_PROXY
$failures = 0
# Windows PowerShell 5.1 turns native stderr text (nc prints its "[ok] exit code" line there) into a terminating
# error when $ErrorActionPreference is 'Stop'. Run native commands with 'Continue' and log stderr as plain text.
function Invoke-Logged {
    param([string]$Log, [scriptblock]$Command)
    $saved = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & $Command 2>&1 | ForEach-Object { "$_" } | Tee-Object -FilePath $Log -Append }
    finally { $ErrorActionPreference = $saved }
}
Push-Location $repo
try {
    $env:NC_HOME = $session.home
    $env:NC_RUNTIME_HOME = $session.runtime
    $env:NO_PROXY = (@($oldNoProxy, '127.0.0.1', '127.0.0.2', '127.0.0.3', 'localhost') | Where-Object { $_ }) -join ','
    $session | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $PSScriptRoot "out\$PC-latest-session.json") -Encoding UTF8
    & $python -c 'import sys,network_connector; print(sys.version); print(network_connector.__file__)' |
        Tee-Object -FilePath (Join-Path $out "$PC-environment.txt")
    git rev-parse HEAD | Tee-Object -FilePath (Join-Path $out "$PC-commit.txt")
    & $python -c 'import json; from network_connector import sandbox; print(json.dumps(sandbox.leftovers()))' |
        Set-Content -LiteralPath (Join-Path $out "$PC-profiles-before.json") -Encoding UTF8
    if ($FullTests) {
        Invoke-Logged (Join-Path $out "$PC-tests.txt") { & $python -m pytest -q }
        if ($LASTEXITCODE -ne 0) { $failures++ }
    }
    $sandboxLog = Join-Path $out "$PC-sandbox.txt"
    foreach ($arguments in @(
        @('sandbox','setup'),
        @('sandbox','status','--check'),
        @('sandbox','run','examples\wasm\primes\primes.wasm','--input-text','0 1000000'),
        @('sandbox','run','examples\wasm\bench\bench.wasm','--input-text','7 3 1')
    )) {
        ('nc ' + ($arguments -join ' ')) | Tee-Object -FilePath $sandboxLog -Append
        Invoke-Logged $sandboxLog { & $nc @arguments }
        if ($LASTEXITCODE -ne 0) { $failures++ }
    }
    & $python -c 'import json; from network_connector import sandbox; print(json.dumps(sandbox.leftovers()))' |
        Tee-Object -FilePath (Join-Path $out "$PC-profiles.json")
    Write-Host "Evidence: $out"
    Write-Host "Failed commands: $failures"
    Write-Host 'Next: docs\ACCEPTANCE_0.3.0.md section 3 (isolated app session).'
} finally {
    $env:NC_HOME, $env:NC_RUNTIME_HOME, $env:NO_PROXY = $oldHome, $oldRuntime, $oldNoProxy
    Pop-Location
}
if ($failures) { exit 1 }
