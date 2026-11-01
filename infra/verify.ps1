param([switch]$CheckCache)
$ErrorActionPreference = 'Stop'
$env:CARGO_TARGET_DIR = 'C:\dev\nc-target'
$env:TEMP = 'C:\dev\tmp'
$env:TMP = 'C:\dev\tmp'

# A unique project owns only this run's containers/volumes. Never print secrets.
$projectName = 'nc-infra-check-' + [Guid]::NewGuid().ToString('N').Substring(0, 8)
$composeArgs = @('compose', '--project-name', $projectName, '--file', 'infra/docker-compose.yml', '--progress', 'plain')
$secretNames = @('NC_PG_PASSWORD', 'NC_ENROLL_SECRET', 'NC_RELAY_TOKEN_KEY', 'NC_TEST_ENROLL', 'NC_TEST_URL')
$previous = @{}
$cleanupNeeded = $false

function Invoke-Native {
    param([string]$Exe, [string[]]$CommandArgs)
    & $Exe @CommandArgs
    if ($LASTEXITCODE -ne 0) { throw "$Exe exited with code $LASTEXITCODE" }
}

function New-Secret {
    $bytes = New-Object byte[] 32
    $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
    try { $rng.GetBytes($bytes) } finally { $rng.Dispose() }
    return [BitConverter]::ToString($bytes).Replace('-', '').ToLowerInvariant()
}

Push-Location (Split-Path $PSScriptRoot -Parent)
try {
    foreach ($name in $secretNames) { $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
    $env:NC_PG_PASSWORD = New-Secret
    $env:NC_ENROLL_SECRET = New-Secret
    $env:NC_RELAY_TOKEN_KEY = New-Secret
    $env:NC_TEST_ENROLL = $env:NC_ENROLL_SECRET
    $env:NC_TEST_URL = 'http://127.0.0.1:8080'

    Invoke-Native -Exe docker -CommandArgs @('info', '--format', '{{.ServerVersion}}')
    $cleanupNeeded = $true
    Invoke-Native -Exe docker -CommandArgs ($composeArgs + @('build'))
    if ($CheckCache) { Invoke-Native -Exe docker -CommandArgs ($composeArgs + @('build')) }
    Invoke-Native -Exe docker -CommandArgs ($composeArgs + @('up', '-d', '--wait', '--wait-timeout', '120'))

    $healthy = $false
    for ($attempt = 0; $attempt -lt 120; $attempt++) {
        try {
            if ((Invoke-RestMethod 'http://127.0.0.1:8080/healthz' -TimeoutSec 2).ok -eq $true) {
                $healthy = $true
                break
            }
        } catch { }
        Start-Sleep -Seconds 1
    }
    if (-not $healthy) { throw 'Coordinator health check timed out.' }
    Write-Output 'Coordinator /healthz: ok'
    foreach ($service in @('coordinator', 'relay')) {
        $uid = & docker @composeArgs exec -T $service id -u
        if ($LASTEXITCODE -ne 0 -or [int]$uid -eq 0) { throw "$service must run as non-root." }
        Write-Output "$service uid: $uid"
    }
    # This is the existing HTTP/database integration test. Keep captured output off logs.
    Invoke-Native -Exe cargo -CommandArgs @('test', '--manifest-path', 'server/Cargo.toml', '-p', 'nc-coordinator', '--test', 'lease_cycle', '--', '--ignored')
} finally {
    try {
        if ($cleanupNeeded) {
            Invoke-Native -Exe docker -CommandArgs ($composeArgs + @('down', '--volumes', '--remove-orphans'))
        }
    } finally {
        foreach ($name in $secretNames) { [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process') }
        Pop-Location
    }
}
