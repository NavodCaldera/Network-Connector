# Windows service end-to-end test (task 1.L12b): proves `nc-agent service` works for real under the virtual
# account NT SERVICE\nc-agent, with the real AppContainer + Job Object sandbox (nothing is weakened).
#
# Must run ELEVATED (installing a service and reading the admin-only control token need it). Uses release
# binaries already built in -BinDir (nc-agent.exe, nc.exe, nc-exec-worker.exe); it does not build Rust.
# Needs Docker Desktop (Linux engine) and free ports 8080, 47710, 47720. Refuses to run if a service called
# nc-agent or the folder C:\ProgramData\NetworkConnector already exists (it never touches a real install).
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File infra\service-test.ps1 [-BinDir C:\nc-target-svc\release]
#
# Checks (each PASS / FAIL):
#   C1 install+start   service installs and reaches Running
#   C2 register        the service device registers and heartbeats after the owner policy is set
#   C3 job             a cpu-core-v1 job run by the service returns exact output (AppContainer under the service)
#   C4 stop            `service stop` while the worker runs: stopped in < 1 s, lease failed with STOP, no worker left
#   C5 acl             data dir ACL holds only SYSTEM, Administrators and NT SERVICE\nc-agent; no inheritance
#   C6 failure-policy  `sc qfailure nc-agent`: restart, restart, none; reset after 86400 s
#   C7 scm-restart     service process killed; SCM starts a new process that reaches Running
#
# Teardown always runs: service uninstalled, C:\ProgramData\NetworkConnector deleted (device key, enroll.secret,
# logs), compose down --volumes, the per-run image removed, run folder deleted, caller env restored. Secrets are
# random per run, kept in this process env (and in enroll.secret inside the data dir, which is deleted), never printed.
param(
    [string]$BinDir = 'C:\nc-target-svc\release',
    [switch]$KeepImage,
    # Loop counts: short job (correctness) and long job (still running when Stop is sent).
    [long]$ShortLoop = 1000000,
    [long]$LongLoop = 300000000
)
$ErrorActionPreference = 'Stop'

$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Output 'REFUSED: infra/service-test.ps1 must run in an elevated PowerShell (Run as administrator).'
    exit 2
}

$repo = Split-Path $PSScriptRoot -Parent
$runId = [Guid]::NewGuid().ToString('N').Substring(0, 8)
$runDir = Join-Path ([IO.Path]::GetTempPath()) "nc-svc-$runId"
$projectName = "nc-svc-$runId"
$image = "nc-server:svc-test-$runId"
$override = Join-Path $runDir 'compose.override.yml'
$composeArgs = @('compose', '--project-name', $projectName, '--file', (Join-Path $repo 'infra/docker-compose.yml'), '--file', $override, '--progress', 'plain')
$secretNames = @('NC_PG_PASSWORD', 'NC_ENROLL_SECRET', 'NC_RELAY_TOKEN_KEY', 'RUST_LOG')
$svcName = 'nc-agent'
$dataDir = Join-Path $env:ProgramData 'NetworkConnector'
$svcBin = Join-Path $dataDir 'bin'
$svcCfg = Join-Path $dataDir 'agent.json'
$previous = @{}
$dockerUp = $false
$weCreated = $false
$submits = New-Object System.Collections.ArrayList
$results = New-Object System.Collections.ArrayList

function Invoke-Native {
    param([string]$Exe, [string[]]$CommandArgs)
    $ErrorActionPreference = 'Continue'
    & $Exe @CommandArgs 2>&1 | ForEach-Object { "$_" }
    if ($LASTEXITCODE -ne 0) { throw "$(Split-Path $Exe -Leaf) exited with code $LASTEXITCODE" }
}
function Get-Native {
    param([string]$Exe, [string[]]$CommandArgs)
    $ErrorActionPreference = 'Continue'
    $o = & $Exe @CommandArgs 2>$null
    $script:lastExit = $LASTEXITCODE
    return $o
}
function New-Secret {
    $bytes = New-Object byte[] 32
    $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
    try { $rng.GetBytes($bytes) } finally { $rng.Dispose() }
    return [BitConverter]::ToString($bytes).Replace('-', '').ToLowerInvariant()
}
function Log([string]$m) { Write-Output ("[{0:HH:mm:ss}] {1}" -f (Get-Date), $m) }
function Add-Result([string]$Id, [string]$Result, [string]$Detail) {
    [void]$results.Add([pscustomobject]@{ Check = $Id; Result = $Result; Detail = $Detail })
    Log "$Result $Id - $Detail"
}
function Sql([string]$q) {
    $ErrorActionPreference = 'Continue'
    $all = $q | & docker @composeArgs exec -T postgres psql -X -q -U nc -d nc -At -F '|' -v ON_ERROR_STOP=1 2>&1
    $code = $LASTEXITCODE
    $err = @($all | Where-Object { $_ -is [Management.Automation.ErrorRecord] } | ForEach-Object { "$_" })
    $out = @($all | Where-Object { $_ -isnot [Management.Automation.ErrorRecord] } | ForEach-Object { "$_" })
    if ($code -ne 0) { throw "psql failed ($code): $($err -join ' ')" }
    return , @($out | Where-Object { $_ -ne '' })
}
function Wait-Until([scriptblock]$Cond, [int]$Seconds, [string]$What) {
    $t0 = Get-Date
    while (((Get-Date) - $t0).TotalSeconds -lt $Seconds) {
        try { if (& $Cond) { return } } catch { }
        Start-Sleep -Milliseconds 250
    }
    throw "timed out after $Seconds s waiting for: $What"
}
function Test-Healthy {
    try { return (Invoke-RestMethod 'http://127.0.0.1:8080/healthz' -TimeoutSec 2).ok -eq $true } catch { return $false }
}
function B64UrlToHex([string]$s) {
    $t = $s.Replace('-', '+').Replace('_', '/')
    while ($t.Length % 4) { $t += '=' }
    return ([BitConverter]::ToString([Convert]::FromBase64String($t))).Replace('-', '').ToLowerInvariant()
}
function Write-Json($obj, [string]$path) {
    [IO.File]::WriteAllText($path, ($obj | ConvertTo-Json -Depth 20), (New-Object Text.UTF8Encoding $false))
}
function HexToBytes([string]$h) {
    $b = New-Object byte[] ($h.Length / 2)
    for ($i = 0; $i -lt $b.Length; $i++) { $b[$i] = [Convert]::ToByte($h.Substring(2 * $i, 2), 16) }
    return , $b
}
function Get-ServiceExists { $null = Get-Native "$env:SystemRoot\System32\sc.exe" @('query', $svcName); return ($script:lastExit -eq 0) }
function Get-ServicePid { [int](Get-CimInstance Win32_Service -Filter "Name='$svcName'").ProcessId }
function Get-ServiceState { (Get-CimInstance Win32_Service -Filter "Name='$svcName'").State }
function Get-Workers {
    @(Get-CimInstance Win32_Process -Filter "Name='nc-exec-worker.exe'" | Where-Object { $_.ExecutablePath -like "$svcBin*" })
}

# cpu-core-v1 module: identical bytes to infra/fleet-e2e.ps1 (see the WAT there). Output = input ++ loop count (LE).
$divStep = '200320017c20028021 03'.Replace(' ', '')
$moduleHex = '0061736d01000000' +
    '011203' + '60037e7f7f017f' + '60027f7f017f' + '6000017f' +
    '022302' + '02' + '6e63' + '0a' + '696e7075745f72656164' + '0000' +
    '02' + '6e63' + '0c' + '6f75747075745f7772697465' + '0001' +
    '03020102' + '0503010001' +
    '071002' + '066d656d6f72790200' + '0372756e0002' +
    '0a7b0179' + '02017f037e' +
    '42004100418020100021 00'.Replace(' ', '') +
    '4100290300' + '2101' +
    '02402001500d00' + '0340' +
    '200242017c2102' + ($divStep * 4) + '200142017d2201' + '4200520d00' + '0b0b' +
    '418020200337 0300'.Replace(' ', '') +
    '2000200237 0300'.Replace(' ', '') +
    '410020004108 6a10011a'.Replace(' ', '') + '41000b'

function Submit-Job([string]$tag, [long]$loop) {
    $dir = Join-Path $runDir "jobs\$tag"
    New-Item -ItemType Directory -Force $dir | Out-Null
    $text = [Text.Encoding]::UTF8.GetBytes("service-test $tag $([Guid]::NewGuid())")
    $inp = New-Object byte[] (8 + $text.Length)
    [BitConverter]::GetBytes($loop).CopyTo($inp, 0)
    $text.CopyTo($inp, 8)
    $expected = New-Object byte[] ($inp.Length + 8)
    $inp.CopyTo($expected, 0)
    [BitConverter]::GetBytes($loop).CopyTo($expected, $inp.Length)
    [IO.File]::WriteAllBytes("$dir\input.bin", $inp)
    $p = Start-Process -FilePath $ncExe -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput "$dir\stdout.log" -RedirectStandardError "$dir\stderr.log" `
        -ArgumentList @('--config', $reqCfg, 'submit', $modulePath, "$dir\input.bin", '--out', "$dir\output.bin",
            '--timeout', '300', '--fuel', '10000000000', '--wall-ms', '120000', '--replicas', '1')
    $null = $p.Handle
    $j = [pscustomobject]@{ Tag = $tag; Dir = $dir; Proc = $p; Expected = $expected; JobId = $null }
    [void]$submits.Add($j)
    Wait-Until {
        $line = Get-Content "$dir\stderr.log" -ErrorAction SilentlyContinue | Select-String 'submitted job ([0-9a-f-]{36})'
        if ($line) { $j.JobId = $line.Matches[0].Groups[1].Value }
        [bool]$j.JobId
    } 120 "$tag submission"
    return $j
}

Push-Location $repo
try {
    foreach ($n in $secretNames) { $previous[$n] = [Environment]::GetEnvironmentVariable($n, 'Process') }
    foreach ($exe in @('nc-agent.exe', 'nc.exe', 'nc-exec-worker.exe')) {
        if (-not (Test-Path (Join-Path $BinDir $exe))) { throw "missing $exe in $BinDir (build release first)" }
    }
    if (Get-ServiceExists) { throw "a service named $svcName already exists; refusing to touch it" }
    if (Test-Path $dataDir) { throw "$dataDir already exists; refusing to touch it" }
    $env:NC_PG_PASSWORD = New-Secret
    $env:NC_ENROLL_SECRET = New-Secret
    $env:NC_RELAY_TOKEN_KEY = New-Secret
    $env:RUST_LOG = 'info'
    New-Item -ItemType Directory -Force $runDir | Out-Null
    Log "run $runId, folder $runDir"

    # ---- 1. Docker: Postgres + coordinator ----
    [IO.File]::WriteAllText($override, "services:`n  coordinator:`n    image: $image`n  relay:`n    image: $image`n", (New-Object Text.UTF8Encoding $false))
    Invoke-Native -Exe docker -CommandArgs @('info', '--format', '{{.ServerVersion}}')
    $dockerUp = $true
    Invoke-Native -Exe docker -CommandArgs ($composeArgs + @('build', 'coordinator'))
    Invoke-Native -Exe docker -CommandArgs ($composeArgs + @('up', '-d', '--wait', '--wait-timeout', '120', 'postgres', 'coordinator'))
    Wait-Until { Test-Healthy } 120 'coordinator /healthz'
    Log 'coordinator healthy'

    # ---- 2. ProgramData layout ----
    $weCreated = $true
    New-Item -ItemType Directory -Force $svcBin | Out-Null
    foreach ($exe in @('nc-agent.exe', 'nc.exe', 'nc-exec-worker.exe')) { Copy-Item (Join-Path $BinDir $exe) $svcBin }
    Invoke-Native -Exe "$env:SystemRoot\System32\icacls.exe" -CommandArgs @($svcBin, '/grant', '*S-1-15-2-1:(OI)(CI)(RX)', '/Q')
    $agentExe = Join-Path $svcBin 'nc-agent.exe'; $ncExe = Join-Path $svcBin 'nc.exe'
    $probe = @{ network = @{ interface = 'ethernet'; connection_id = 'svc-lan'; bytes_per_second = 100000000 }; declared_thermal = 'nominal' }
    $deviceName = "svc-helper-$runId"
    Write-Json @{ coordinator_url = 'http://127.0.0.1:8080'; data_dir = $dataDir; device_name = $deviceName
        worker_path = (Join-Path $svcBin 'nc-exec-worker.exe'); network_budget = 'unmetered'; probe = $probe
        control_port = 47720; poll_seconds = 1 } $svcCfg
    $modulePath = Join-Path $runDir 'svc.wasm'
    [IO.File]::WriteAllBytes($modulePath, (HexToBytes $moduleHex))
    $reqDir = Join-Path $runDir 'requester'
    New-Item -ItemType Directory -Force $reqDir | Out-Null
    $reqCfg = Join-Path $reqDir 'agent.json'
    Write-Json @{ coordinator_url = 'http://127.0.0.1:8080'; data_dir = $reqDir; device_name = "svc-req-$runId"
        worker_path = (Join-Path $svcBin 'nc-exec-worker.exe'); network_budget = 'unmetered'; probe = $probe; control_port = 47710 } $reqCfg
    $publisherHex = B64UrlToHex ((Get-Native $ncExe @('--config', $reqCfg, 'key')) | Select-Object -First 1)
    if ($publisherHex.Length -ne 64) { throw 'nc key did not return a 32-byte key' }

    # ---- 3. Install + start (C1) ----
    try {
        Invoke-Native -Exe $agentExe -CommandArgs @('service', 'install', '--config', $svcCfg) | ForEach-Object { Log "install: $_" }
        $env:NC_ENROLL_SECRET = $null   # stored in the data dir now; not needed in this env any more
        Invoke-Native -Exe $agentExe -CommandArgs @('service', 'start') | ForEach-Object { Log "start: $_" }
        if ((Get-ServiceState) -ne 'Running') { throw "state $(Get-ServiceState)" }
        $acct = (Get-CimInstance Win32_Service -Filter "Name='$svcName'").StartName
        if ($acct -ne 'NT SERVICE\nc-agent') { throw "service account is '$acct'" }
        Add-Result 'C1 install+start' 'PASS' "running as $acct, pid $(Get-ServicePid)"
    } catch { Add-Result 'C1 install+start' 'FAIL' $_.Exception.Message; throw }

    # ---- 4. Owner policy + start (C2) ----
    try {
        Wait-Until { Test-Path (Join-Path $dataDir 'control.token') } 30 'service control token'
        Wait-Until { $null = Get-Native $ncExe @('--config', $svcCfg, 'status'); $script:lastExit -eq 0 } 30 'service control socket'
        $pol = (Get-Native $ncExe @('--config', $svcCfg, 'policy', 'show')) -join "`n" | ConvertFrom-Json
        $pol.accept_jobs = $true
        $pol.charging_only = $false
        $pol.min_battery_percent = 0
        $pol.pause_on_user_activity = $false
        $pol.time_windows = @(
            @{ days = @('Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'); start = '00:00:00'; end = '12:00:00' },
            @{ days = @('Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'); start = '12:00:00'; end = '00:00:00' })
        $pol.trusted_peers = @($publisherHex)
        $pol.allowed_execution_profiles = @('cpu-core-v1')
        $polFile = Join-Path $runDir 'policy.json'
        Write-Json $pol $polFile
        Invoke-Native -Exe $ncExe -CommandArgs @('--config', $svcCfg, 'policy', 'set', $polFile) | Out-Null
        $null = Get-Native $ncExe @('--config', $svcCfg, 'start')
        Wait-Until { (Sql "SELECT count(*) FROM devices WHERE name='$deviceName' AND heartbeat_at IS NOT NULL")[0] -eq '1' } 60 'service device heartbeating'
        $devId = (Sql "SELECT id FROM devices WHERE name='$deviceName'")[0]
        Add-Result 'C2 register' 'PASS' 'policy set through nc; service device registered and heartbeating'
    } catch { Add-Result 'C2 register' 'FAIL' $_.Exception.Message; throw }

    # ---- 5. Job through the service (C3) ----
    try {
        $j = Submit-Job 'short' $ShortLoop
        if (-not $j.Proc.WaitForExit(300000)) { throw 'nc submit did not finish in 300 s' }
        $j.Proc.WaitForExit()
        if ($j.Proc.ExitCode -ne 0) { throw "nc submit exit $($j.Proc.ExitCode): $(Get-Content "$($j.Dir)\stderr.log" | Select-Object -Last 1)" }
        $got = [IO.File]::ReadAllBytes("$($j.Dir)\output.bin")
        if ([Convert]::ToBase64String($got) -ne [Convert]::ToBase64String($j.Expected)) { throw 'output bytes differ from expected' }
        $by = (Sql "SELECT count(*) FROM task_results r JOIN tasks t ON t.id=r.task_id WHERE t.job_id='$($j.JobId)' AND r.device_id='$devId'")[0]
        if ($by -ne '1') { throw "result not from the service device ($by rows)" }
        Add-Result 'C3 job' 'PASS' 'cpu-core-v1 output exact; run by the service device in the AppContainer sandbox'
    } catch { Add-Result 'C3 job' 'FAIL' $_.Exception.Message }

    # ---- 6. Stop while a worker runs (C4) ----
    try {
        $svcPid = Get-ServicePid
        $j = Submit-Job 'long' $LongLoop
        Wait-Until { (Sql "SELECT count(*) FROM leases l JOIN tasks t ON t.id=l.task_id WHERE t.job_id='$($j.JobId)' AND l.state='active'")[0] -ne '0' } 60 'service holding the lease'
        Wait-Until { @(Get-Workers | Where-Object { $_.ParentProcessId -eq $svcPid }).Count -gt 0 } 60 'worker running under the service'
        $sw = [Diagnostics.Stopwatch]::StartNew()
        $out = Invoke-Native -Exe $agentExe -CommandArgs @('service', 'stop')
        $sw.Stop()
        $out | ForEach-Object { Log "stop: $_" }
        $p = @()
        $m = [regex]::Match(($out -join ' '), 'stopped after (\d+) ms')
        $scmMs = if ($m.Success) { [int]$m.Groups[1].Value } else { -1 }
        if ($scmMs -lt 0 -or $scmMs -ge 1000) { $p += "SCM stop took $scmMs ms (want < 1000)" }
        $left = @(Get-Workers).Count
        if ($left -ne 0) { $p += "$left nc-exec-worker left running" }
        # The fail call goes out before the process exits; allow a moment for the DB row.
        $lease = ''
        try {
            Wait-Until { $script:leaseRow = (Sql ("SELECT l.state, coalesce(a.code,'') FROM leases l LEFT JOIN attempts a ON a.id=l.attempt_id " +
                        "JOIN tasks t ON t.id=l.task_id WHERE t.job_id='$($j.JobId)' AND l.device_id='$devId'")); $script:leaseRow -contains 'failed|STOP' } 10 'lease failed with STOP'
            $lease = 'failed|STOP'
        } catch { $lease = ($script:leaseRow -join ', '); $p += "lease rows: $lease (want failed|STOP)" }
        if (-not $j.Proc.HasExited) { Stop-Process -Id $j.Proc.Id -Force -ErrorAction SilentlyContinue }
        if ($p.Count) { Add-Result 'C4 stop' 'FAIL' ($p -join '; ') }
        else { Add-Result 'C4 stop' 'PASS' ("stopped in $scmMs ms (SCM), {0} ms wall for the CLI; lease $lease; no worker left" -f $sw.ElapsedMilliseconds) }
    } catch { Add-Result 'C4 stop' 'FAIL' $_.Exception.Message }

    # ---- 7. ACL (C5) and failure policy (C6) ----
    try {
        $acl = Get-Acl $dataDir
        $ids = @($acl.Access | ForEach-Object { $_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value } | Sort-Object -Unique)
        $svcSid = (New-Object Security.Principal.NTAccount 'NT SERVICE\nc-agent').Translate([Security.Principal.SecurityIdentifier]).Value
        $want = @('S-1-5-18', 'S-1-5-32-544', $svcSid) | Sort-Object
        $ic = (Get-Native "$env:SystemRoot\System32\icacls.exe" @($dataDir)) | Where-Object { $_ -match '\S' }
        $ic | ForEach-Object { Log "icacls: $_" }
        $p = @()
        if (($ids -join ',') -ne ($want -join ',')) { $p += "ACL principals: $($ids -join ', ')" }
        if (-not $acl.AreAccessRulesProtected) { $p += 'inheritance is not disabled' }
        if ($p.Count) { Add-Result 'C5 acl' 'FAIL' ($p -join '; ') } else { Add-Result 'C5 acl' 'PASS' 'only SYSTEM, Administrators and NT SERVICE\nc-agent; inheritance off' }
    } catch { Add-Result 'C5 acl' 'FAIL' $_.Exception.Message }
    try {
        $qf = Get-Native "$env:SystemRoot\System32\sc.exe" @('qfailure', $svcName)
        $qf | Where-Object { $_ -match '\S' } | ForEach-Object { Log "qfailure: $_" }
        $txt = $qf -join "`n"
        $p = @()
        if ($txt -notmatch 'RESET_PERIOD \(in seconds\)\s*:\s*86400') { $p += 'reset period not 86400' }
        if (@([regex]::Matches($txt, 'RESTART -- Delay = 5000')).Count -ne 1) { $p += 'no 5 s restart' }
        if (@([regex]::Matches($txt, 'RESTART -- Delay = 30000')).Count -ne 1) { $p += 'no 30 s restart' }
        $flag = (Get-Native "$env:SystemRoot\System32\sc.exe" @('qfailureflag', $svcName)) -join ' '
        if ($flag -notmatch 'FAILURE_ACTIONS_ON_NONCRASH_FAILURES\s*:\s*TRUE') { $p += 'non-crash failures flag not TRUE' }
        if ($p.Count) { Add-Result 'C6 failure-policy' 'FAIL' ($p -join '; ') } else { Add-Result 'C6 failure-policy' 'PASS' 'restart 5 s, restart 30 s, then none; reset 1 day; non-crash failures count' }
    } catch { Add-Result 'C6 failure-policy' 'FAIL' $_.Exception.Message }

    # ---- 8. Kill the service process; SCM must restart it (C7) ----
    try {
        Invoke-Native -Exe $agentExe -CommandArgs @('service', 'start') | ForEach-Object { Log "start: $_" }
        $old = Get-ServicePid
        Stop-Process -Id $old -Force
        $killed = Get-Date
        Wait-Until { (Get-ServiceState) -eq 'Running' -and (Get-ServicePid) -ne 0 -and (Get-ServicePid) -ne $old } 45 'SCM restart'
        Add-Result 'C7 scm-restart' 'PASS' ("pid $old killed; new pid $(Get-ServicePid) Running after {0:N1} s" -f ((Get-Date) - $killed).TotalSeconds)
    } catch { Add-Result 'C7 scm-restart' 'FAIL' $_.Exception.Message }
} catch {
    Log "aborted: $($_.Exception.Message)"
    if (-not $results.Count) { Add-Result 'setup' 'FAIL' $_.Exception.Message }
} finally {
    Log 'teardown'
    foreach ($j in $submits) { if (-not $j.Proc.HasExited) { Stop-Process -Id $j.Proc.Id -Force -ErrorAction SilentlyContinue } }
    if ($weCreated) {
        try {
            if (Get-ServiceExists) {
                $u = Join-Path $svcBin 'nc-agent.exe'
                if (Test-Path $u) { $null = Get-Native $u @('service', 'uninstall') }
                if (Get-ServiceExists) {
                    $null = Get-Native "$env:SystemRoot\System32\sc.exe" @('stop', $svcName)
                    $sp = Get-ServicePid
                    if ($sp) { Stop-Process -Id $sp -Force -ErrorAction SilentlyContinue }
                    $null = Get-Native "$env:SystemRoot\System32\sc.exe" @('delete', $svcName)
                }
            }
        } catch { Log "service removal: $($_.Exception.Message)" }
        Get-Workers | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
        for ($i = 0; $i -lt 20 -and (Test-Path $dataDir); $i++) {
            Remove-Item -Recurse -Force $dataDir -ErrorAction SilentlyContinue
            if (Test-Path $dataDir) { Start-Sleep -Milliseconds 500 }
        }
        if (Test-Path $dataDir) { Log "WARNING: could not delete $dataDir; delete it by hand (holds a device key and enroll.secret)" }
        if (Get-ServiceExists) { Log "WARNING: service $svcName still exists; run sc.exe delete $svcName" }
    }
    try {
        if ($dockerUp) {
            Invoke-Native -Exe docker -CommandArgs ($composeArgs + @('down', '--volumes', '--remove-orphans')) | Out-Null
            if (-not $KeepImage) { $null = Get-Native docker @('image', 'rm', $image) }
        }
    } catch { Log "docker teardown: $($_.Exception.Message)" }
    foreach ($n in $secretNames) { [Environment]::SetEnvironmentVariable($n, $previous[$n], 'Process') }
    Remove-Item -Recurse -Force $runDir -ErrorAction SilentlyContinue
    Pop-Location
}

Write-Output ''
Write-Output 'Service test summary'
$results | Format-Table -AutoSize -Wrap Check, Result, Detail | Out-String -Width 220 | Write-Output
$bad = @($results | Where-Object { $_.Result -ne 'PASS' }).Count
if ($bad -or $results.Count -lt 7) { Write-Output "FAIL: $bad check(s) failed, $($results.Count) of 7 ran"; exit 1 }
Write-Output 'PASS: all 7 checks passed'
exit 0
