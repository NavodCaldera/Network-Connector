# Fleet end-to-end test (task 1.L1): coordinator + Postgres in Docker, two nc-agent helper daemons on this PC
# (own data dir, key and control port each), and the `nc` CLI as requester submitting real cpu-core-v1 jobs.
#
# Scenarios (each reported PASS / FAIL / EXPECTED-FAIL; EXPECTED-FAIL names its gap in docs/laptop-gap.md):
#   S1 baseline         jobs complete on the two helpers, outputs correct, one commit per task
#   S2 helper-crash     helper A is killed (Stop-Process -Force) while it runs a job, then restarted
#   S3 coord-disconnect coordinator is paused (connection hangs) and later restarted while jobs run
#   S4 two-helper       --replicas 2: both helpers must return the same output before commit
#   S5 tariff-boundary  helper B gets a metered profile whose window closes mid-run; after the boundary B must
#                       not lease or upload anything, and the jobs still complete on A
#   X1..X4              not possible yet (gaps); reported as EXPECTED-FAIL with a real check where one exists
#
# Windows only (the executor sandbox is AppContainer + Job Object; nothing is weakened for this test).
# Needs Docker Desktop (Linux engine), Rust/Cargo, free ports 8080, 47711, 47712.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File infra/fleet-e2e.ps1 [-SkipBuild] [-KeepImage]
#
# Secrets are generated per run in this process's environment only and never printed. Agent logs stay in the
# run folder (deleted at the end unless -KeepRunDir); they contain no secrets or job data.
param(
    [switch]$SkipBuild,
    [switch]$KeepImage,
    [switch]$KeepRunDir,
    [int]$JobsPerScenario = 4,
    # Loop iterations per job (sets job run time; ~5-10 s on a debug worker is the target).
    [long]$LoopCount = 150000000
)
$ErrorActionPreference = 'Stop'
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = 'C:\nc-target' }
$repo = Split-Path $PSScriptRoot -Parent
$runId = [Guid]::NewGuid().ToString('N').Substring(0, 8)
$runDir = Join-Path ([IO.Path]::GetTempPath()) "nc-fleet-$runId"
$projectName = "nc-fleet-$runId"
$image = "nc-server:fleet-e2e-$runId"
$override = Join-Path $runDir 'compose.override.yml'
$composeArgs = @('compose', '--project-name', $projectName, '--file', (Join-Path $repo 'infra/docker-compose.yml'), '--file', $override, '--progress', 'plain')
$secretNames = @('NC_PG_PASSWORD', 'NC_ENROLL_SECRET', 'NC_RELAY_TOKEN_KEY', 'NC_AGENT_CONFIG', 'RUST_LOG')
$previous = @{}
$dockerUp = $false
$helpers = @{}
$submits = New-Object System.Collections.ArrayList
$results = New-Object System.Collections.ArrayList
$gapDoc = 'docs/laptop-gap.md'

# Windows PowerShell 5.1 turns native stderr into terminating errors under 'Stop' once output is redirected, so
# every native call goes through these two helpers, which judge success by exit code only.
function Invoke-Native {
    param([string]$Exe, [string[]]$CommandArgs)
    $ErrorActionPreference = 'Continue'
    & $Exe @CommandArgs 2>&1 | ForEach-Object { "$_" }
    if ($LASTEXITCODE -ne 0) { throw "$(Split-Path $Exe -Leaf) exited with code $LASTEXITCODE" }
}
# Runs a native command, discards stderr, returns stdout lines; $script:lastExit holds the exit code.
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
function Add-Result([string]$Id, [string]$Result, [string]$Detail, [string]$Gap = '') {
    [void]$results.Add([pscustomobject]@{ Scenario = $Id; Result = $Result; Gap = $Gap; Detail = $Detail })
    Log "RESULT $Id : $Result $(if ($Gap) { "($gapDoc $Gap)" }) - $Detail"
}
function Sql([string]$q) {
    $ErrorActionPreference = 'Continue'
    # The query goes in on stdin: no native-argument quoting issues. psql errors carry no secrets.
    $all = $q | & docker @composeArgs exec -T postgres psql -X -q -U nc -d nc -At -F '|' -v ON_ERROR_STOP=1 2>&1
    $code = $LASTEXITCODE
    $err = @($all | Where-Object { $_ -is [Management.Automation.ErrorRecord] } | ForEach-Object { "$_" })
    $out = @($all | Where-Object { $_ -isnot [Management.Automation.ErrorRecord] } | ForEach-Object { "$_" })
    if ($code -ne 0) { throw "psql failed ($code): $($err -join ' ')" }
    return , @($out | Where-Object { $_ -ne '' })   # keep a 1-row result an array
}
function UnixNow { [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() }
function Wait-Until([scriptblock]$Cond, [int]$Seconds, [string]$What) {
    $t0 = Get-Date
    while (((Get-Date) - $t0).TotalSeconds -lt $Seconds) {
        try { if (& $Cond) { return } } catch { }
        Start-Sleep -Milliseconds 500
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
    # UTF-8 without BOM (serde_json rejects a BOM).
    [IO.File]::WriteAllText($path, ($obj | ConvertTo-Json -Depth 20), (New-Object Text.UTF8Encoding $false))
}

# cpu-core-v1 module, hand-assembled (no wat tool needed). Equivalent WAT:
# (module
#  (import "nc" "input_read" (func $read (param i64 i32 i32) (result i32)))
#  (import "nc" "output_write" (func $write (param i32 i32) (result i32)))
#  (memory (export "memory") 1)
#  (func (export "run") (result i32) (local $n i32) (local $i i64) (local $acc i64) (local $y i64)
#   (local.set $n (call $read (i64.const 0) (i32.const 0) (i32.const 4096)))
#   (local.set $i (i64.load (i32.const 0)))            ;; loop count = first 8 input bytes (LE)
#   (block (br_if 0 (i64.eqz (local.get $i)))
#    (loop (local.set $acc (i64.add (local.get $acc) (i64.const 1)))
#          ;; 4 dependent 64-bit divisions: CPU time per unit of fuel (fuel is capped at 1e10 per job)
#          (local.set $y (i64.div_u (i64.add (local.get $y) (local.get $i)) (local.get $acc)))  ;; x4
#          (br_if 0 (i64.ne (local.tee $i (i64.sub (local.get $i) (i64.const 1))) (i64.const 0)))))
#   (i64.store (i32.const 4096) (local.get $y))        ;; keeps the divisions live; not part of the output
#   (i64.store (local.get $n) (local.get $acc))       ;; output = input ++ acc (LE), acc == loop count
#   (drop (call $write (i32.const 0) (i32.add (local.get $n) (i32.const 8))))
#   (i32.const 0)))
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

function HexToBytes([string]$h) {
    $b = New-Object byte[] ($h.Length / 2)
    for ($i = 0; $i -lt $b.Length; $i++) { $b[$i] = [Convert]::ToByte($h.Substring(2 * $i, 2), 16) }
    return , $b
}

function Start-Helper([string]$name) {
    $h = $helpers[$name]
    $out = Join-Path $runDir "$name.out.log"; $err = Join-Path $runDir "$name.err.log"
    # Append across restarts.
    $p = Start-Process -FilePath $agentExe -ArgumentList @('run', '--config', $h.Config) -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput "$out.$($h.Starts)" -RedirectStandardError "$err.$($h.Starts)"
    $null = $p.Handle
    $h.Starts++
    $h.Proc = $p
    Wait-Until { Test-Path $h.Token } 30 "$name control token"
    Wait-Until { $null = Get-Native $ncExe @('--config', $h.Config, 'status'); $script:lastExit -eq 0 } 30 "$name control socket"
}
function Stop-Helper([string]$name) {
    $h = $helpers[$name]
    if ($h.Proc -and -not $h.Proc.HasExited) {
        $null = Get-Native $ncExe @('--config', $h.Config, 'shutdown')
        if (-not $h.Proc.WaitForExit(5000)) { Stop-Process -Id $h.Proc.Id -Force -ErrorAction SilentlyContinue }
    }
}
function Get-DeviceId([string]$deviceName) {
    $r = Sql "SELECT id FROM devices WHERE name='$deviceName' AND public_key IS NOT NULL"
    if ($r.Count -ne 1) { throw "device $deviceName not registered exactly once" }
    return $r[0]
}

function Submit-Jobs([string]$tag, [int]$count, [int]$replicas) {
    $batch = @()
    for ($k = 0; $k -lt $count; $k++) {
        $dir = Join-Path $runDir "jobs\$tag-$k"
        New-Item -ItemType Directory -Force $dir | Out-Null
        $text = [Text.Encoding]::UTF8.GetBytes("fleet-e2e $tag job $k $([Guid]::NewGuid())")
        $inp = New-Object byte[] (8 + $text.Length)
        [BitConverter]::GetBytes([long]$LoopCount).CopyTo($inp, 0)
        $text.CopyTo($inp, 8)
        $expected = New-Object byte[] ($inp.Length + 8)
        $inp.CopyTo($expected, 0)
        [BitConverter]::GetBytes([long]$LoopCount).CopyTo($expected, $inp.Length)
        [IO.File]::WriteAllBytes("$dir\input.bin", $inp)
        $p = Start-Process -FilePath $ncExe -PassThru -WindowStyle Hidden `
            -RedirectStandardOutput "$dir\stdout.log" -RedirectStandardError "$dir\stderr.log" `
            -ArgumentList @('--config', $reqCfg, 'submit', $modulePath, "$dir\input.bin", '--out', "$dir\output.bin",
                '--timeout', '600', '--fuel', '10000000000', '--wall-ms', '120000', '--replicas', "$replicas")
        $null = $p.Handle   # cache the handle now, or ExitCode stays empty after exit (Start-Process quirk)
        $j = [pscustomobject]@{ Tag = $tag; Dir = $dir; Proc = $p; Expected = $expected; Replicas = $replicas; JobId = $null }
        [void]$submits.Add($j)
        $batch += $j
    }
    # Wait until the coordinator has every job (submit done, CLI now polling).
    Wait-Until {
        foreach ($j in $batch) {
            if (-not $j.JobId) {
                $line = Get-Content "$($j.Dir)\stderr.log" -ErrorAction SilentlyContinue | Select-String 'submitted job ([0-9a-f-]{36})'
                if ($line) { $j.JobId = $line.Matches[0].Groups[1].Value }
            }
        }
        @($batch | Where-Object { -not $_.JobId }).Count -eq 0
    } 120 "$tag submissions"
    return , $batch
}

# Checks every job in $batch: CLI exit 0, output bytes exact, and in the database exactly one commit with
# validation_count results from distinct devices (no double commit, no extra accepted result).
function Assert-Batch($batch, [int]$timeoutSec) {
    $problems = @()
    foreach ($j in $batch) {
        if (-not $j.Proc.WaitForExit($timeoutSec * 1000)) { $problems += "$($j.Tag): CLI did not finish"; continue }
        $j.Proc.WaitForExit()
        if ($j.Proc.ExitCode -ne 0) {
            $why = (Get-Content "$($j.Dir)\stderr.log" -ErrorAction SilentlyContinue | Select-Object -Last 1)
            $problems += "job $($j.JobId): nc submit exit $($j.Proc.ExitCode) ($why)"; continue
        }
        $got = [IO.File]::ReadAllBytes("$($j.Dir)\output.bin")
        if ([Convert]::ToBase64String($got) -ne [Convert]::ToBase64String($j.Expected)) { $problems += "job $($j.JobId): wrong output" }
        $row = Sql ("SELECT t.state, (t.committed_attempt IS NOT NULL), j.validation_count, " +
            "(SELECT count(*) FROM task_results r WHERE r.task_id=t.id), " +
            "(SELECT count(DISTINCT device_id) FROM task_results r WHERE r.task_id=t.id), " +
            "(SELECT count(*) FROM leases l WHERE l.task_id=t.id AND l.state='completed'), " +
            "(SELECT count(DISTINCT sha256) FROM task_results r WHERE r.task_id=t.id), " +
            "(SELECT count(*) FROM tasks t2 WHERE t2.job_id=j.id) " +
            "FROM tasks t JOIN jobs j ON j.id=t.job_id WHERE j.id='$($j.JobId)'")
        if ($row.Count -ne 1) { $problems += "job $($j.JobId): expected 1 task row, got $($row.Count)"; continue }
        $f = $row[0].Split('|')
        if ($f[0] -ne 'committed' -or $f[1] -ne 't') { $problems += "job $($j.JobId): state $($f[0])" }
        if ([int]$f[2] -ne $j.Replicas) { $problems += "job $($j.JobId): validation_count $($f[2])" }
        if ([int]$f[3] -ne $j.Replicas -or [int]$f[4] -ne $j.Replicas -or [int]$f[5] -ne $j.Replicas) {
            $problems += "job $($j.JobId): results=$($f[3]) devices=$($f[4]) completed leases=$($f[5]) (want $($j.Replicas))"
        }
        if ([int]$f[6] -ne 1) { $problems += "job $($j.JobId): $($f[6]) different result hashes" }
    }
    return , $problems
}
function Get-Workers {
    @(Get-CimInstance Win32_Process -Filter "Name='nc-exec-worker.exe'" | Where-Object { $_.ExecutablePath -like "$runtimeDir*" })
}
function Get-Attempts($batch) {
    $ids = ($batch | ForEach-Object { "'$($_.JobId)'" }) -join ','
    return Sql ("SELECT d.name, l.state, count(*) FROM leases l JOIN tasks t ON t.id=l.task_id JOIN devices d ON d.id=l.device_id " +
        "WHERE t.job_id IN ($ids) GROUP BY d.name, l.state ORDER BY 1,2")
}

Push-Location $repo
try {
    foreach ($n in $secretNames) { $previous[$n] = [Environment]::GetEnvironmentVariable($n, 'Process') }
    $env:NC_PG_PASSWORD = New-Secret
    $env:NC_ENROLL_SECRET = New-Secret
    $env:NC_RELAY_TOKEN_KEY = New-Secret
    $env:RUST_LOG = 'info'
    New-Item -ItemType Directory -Force $runDir | Out-Null
    Log "run $runId, folder $runDir"

    # ---- Build host binaries ----
    if (-not $SkipBuild) {
        Log 'building nc-agent, nc, nc-exec-worker'
        Invoke-Native -Exe cargo -CommandArgs @('build', '-j', '2', '--manifest-path', 'core/Cargo.toml', '-p', 'nc-agent', '-p', 'nc-cli', '-p', 'nc-exec', '--bins')
    }
    $bin = Join-Path $env:CARGO_TARGET_DIR 'debug'
    $agentExe = Join-Path $bin 'nc-agent.exe'; $ncExe = Join-Path $bin 'nc.exe'
    # The worker runs inside an AppContainer: give it its own folder that ALL APPLICATION PACKAGES may read
    # (read+execute only; same grant nc-exec's sandbox tests use).
    $runtimeDir = Join-Path $runDir 'runtime'
    New-Item -ItemType Directory -Force $runtimeDir | Out-Null
    Copy-Item (Join-Path $bin 'nc-exec-worker.exe') $runtimeDir
    Invoke-Native -Exe "$env:SystemRoot\System32\icacls.exe" -CommandArgs @($runtimeDir, '/grant', '*S-1-15-2-1:(OI)(CI)(RX)', '/Q')
    $worker = Join-Path $runtimeDir 'nc-exec-worker.exe'
    $modulePath = Join-Path $runDir 'fleet.wasm'
    [IO.File]::WriteAllBytes($modulePath, (HexToBytes $moduleHex))

    # ---- Docker: Postgres + coordinator only (the relay is not used by the agent yet; see gap G2) ----
    [IO.File]::WriteAllText($override, "services:`n  coordinator:`n    image: $image`n  relay:`n    image: $image`n", (New-Object Text.UTF8Encoding $false))
    Invoke-Native -Exe docker -CommandArgs @('info', '--format', '{{.ServerVersion}}')
    $dockerUp = $true
    Invoke-Native -Exe docker -CommandArgs ($composeArgs + @('build', 'coordinator'))
    Invoke-Native -Exe docker -CommandArgs ($composeArgs + @('up', '-d', '--wait', '--wait-timeout', '120', 'postgres', 'coordinator'))
    Wait-Until { Test-Healthy } 120 'coordinator /healthz'
    Log 'coordinator healthy'

    # ---- Requester + two helpers ----
    $probe = @{ network = @{ interface = 'ethernet'; connection_id = 'fleet-lan'; bytes_per_second = 100000000 }; declared_thermal = 'nominal' }
    $reqDir = Join-Path $runDir 'requester'
    New-Item -ItemType Directory -Force $reqDir | Out-Null
    $reqCfg = Join-Path $reqDir 'agent.json'
    Write-Json @{ coordinator_url = 'http://127.0.0.1:8080'; data_dir = $reqDir; device_name = "fleet-req-$runId"; worker_path = $worker;
        network_budget = 'unmetered'; probe = $probe; control_port = 47710 } $reqCfg
    $publisherHex = B64UrlToHex ((Get-Native $ncExe @('--config', $reqCfg, 'key')) | Select-Object -First 1)
    if ($publisherHex.Length -ne 64) { throw 'nc key did not return a 32-byte key' }

    $port = 47711
    foreach ($name in @('A', 'B')) {
        $d = Join-Path $runDir "helper-$name"
        New-Item -ItemType Directory -Force $d | Out-Null
        $cfg = @{ coordinator_url = 'http://127.0.0.1:8080'; data_dir = $d; device_name = "fleet-$name-$runId"; worker_path = $worker;
            network_budget = 'unmetered'; probe = $probe; control_port = $port; poll_seconds = 1 }
        Write-Json $cfg (Join-Path $d 'agent.json')
        $helpers[$name] = [pscustomobject]@{ Name = $name; Dir = $d; Config = (Join-Path $d 'agent.json'); Cfg = $cfg; Token = (Join-Path $d 'control.token'); Proc = $null; Starts = 0; DeviceName = "fleet-$name-$runId" }
        $port++
    }
    foreach ($name in @('A', 'B')) {
        $h = $helpers[$name]
        Start-Helper $name
        # Owner policy through the real CLI: start from the daemon's own defaults, then opt in.
        $pol = (Get-Native $ncExe @('--config', $h.Config, 'policy', 'show')) -join "`n" | ConvertFrom-Json
        $pol.accept_jobs = $true
        $pol.charging_only = $false
        $pol.min_battery_percent = 0
        $pol.pause_on_user_activity = $false
        $pol.time_windows = @(
            @{ days = @('Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'); start = '00:00:00'; end = '12:00:00' },
            @{ days = @('Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'); start = '12:00:00'; end = '00:00:00' })
        $pol.trusted_peers = @($publisherHex)
        $pol.allowed_execution_profiles = @('cpu-core-v1')
        $polFile = Join-Path $runDir "policy-$name.json"
        Write-Json $pol $polFile
        Invoke-Native -Exe $ncExe -CommandArgs @('--config', $h.Config, 'policy', 'set', $polFile)
        # May answer "already running" when the policy update started the session.
        $null = Get-Native $ncExe @('--config', $h.Config, 'start')
    }
    Wait-Until { (Sql "SELECT count(*) FROM devices WHERE name LIKE 'fleet-_-$runId' AND heartbeat_at IS NOT NULL")[0] -eq '2' } 60 'both helpers registered and heartbeating'
    $devA = Get-DeviceId $helpers['A'].DeviceName
    $devB = Get-DeviceId $helpers['B'].DeviceName
    Log 'both helpers registered'

    # ---- S1 baseline ----
    try {
        $t0 = Get-Date
        $batch = Submit-Jobs 's1' $JobsPerScenario 1
        $p = Assert-Batch $batch 600
        $att = Get-Attempts $batch
        if ($p.Count) { Add-Result 'S1 baseline' 'FAIL' ($p -join '; ') }
        else { Add-Result 'S1 baseline' 'PASS' ("$($batch.Count) jobs committed once with exact outputs in {0:N0} s; attempts: {1}" -f ((Get-Date) - $t0).TotalSeconds, ($att -join ', ')) }
    } catch { Add-Result 'S1 baseline' 'FAIL' $_.Exception.Message }

    # ---- S2 helper crash + restart ----
    try {
        $batch = Submit-Jobs 's2' $JobsPerScenario 1
        $ids = ($batch | ForEach-Object { "'$($_.JobId)'" }) -join ','
        Wait-Until { (Sql "SELECT count(*) FROM leases l JOIN tasks t ON t.id=l.task_id WHERE t.job_id IN ($ids) AND l.device_id='$devA' AND l.state='active'")[0] -ne '0' } 120 'helper A holding a lease'
        # Kill while the sandboxed worker is actually running.
        Wait-Until { @(Get-Workers | Where-Object { $_.ParentProcessId -eq $helpers['A'].Proc.Id }).Count -gt 0 } 60 'helper A worker running'
        $pidA = $helpers['A'].Proc.Id
        Stop-Process -Id $pidA -Force
        $killed = Get-Date
        Log 'helper A killed mid-job'
        Start-Sleep -Seconds 2
        $orphans = @(Get-Workers | Where-Object { $_.ParentProcessId -eq $pidA })
        $orphanNote = if ($orphans.Count) { "$($orphans.Count) worker(s) survived the agent kill (Job Object kill-on-close not effective)" } else { 'worker torn down with the agent' }
        Start-Sleep -Seconds 3
        Start-Helper 'A'
        Log 'helper A restarted'
        $p = Assert-Batch $batch 600
        if ($orphans.Count) { $p += $orphanNote }
        $att = Get-Attempts $batch
        if ($p.Count) { Add-Result 'S2 helper-crash' 'FAIL' ($p -join '; ') }
        else { Add-Result 'S2 helper-crash' 'PASS' "all $($batch.Count) jobs committed once, outputs exact; $orphanNote; attempts: $($att -join ', ')" }
    } catch { Add-Result 'S2 helper-crash' 'FAIL' $_.Exception.Message }

    # ---- S3 coordinator disconnect: pause (hang) 20 s, then restart ----
    try {
        $batch = Submit-Jobs 's3' $JobsPerScenario 1
        $ids = ($batch | ForEach-Object { "'$($_.JobId)'" }) -join ','
        Wait-Until { (Sql "SELECT count(*) FROM leases l JOIN tasks t ON t.id=l.task_id WHERE t.job_id IN ($ids) AND l.state='active'")[0] -ne '0' } 120 'a lease in flight'
        Invoke-Native -Exe docker -CommandArgs ($composeArgs + @('pause', 'coordinator'))
        Log 'coordinator paused (connections hang)'
        Start-Sleep -Seconds 20
        Invoke-Native -Exe docker -CommandArgs ($composeArgs + @('unpause', 'coordinator'))
        Log 'coordinator unpaused'
        # Restart while work is still in flight (or right away if the batch already finished).
        Wait-Until { (Sql "SELECT count(*) FROM tasks t WHERE t.job_id IN ($ids) AND t.state='committed'")[0] -eq "$($batch.Count)" -or
            (Sql "SELECT count(*) FROM leases l JOIN tasks t ON t.id=l.task_id WHERE t.job_id IN ($ids) AND l.state='active'")[0] -ne '0' } 120 'a lease in flight after unpause'
        Invoke-Native -Exe docker -CommandArgs ($composeArgs + @('restart', 'coordinator'))
        Wait-Until { Test-Healthy } 120 'coordinator healthy after restart'
        Log 'coordinator restarted'
        $p = Assert-Batch $batch 600
        $att = Get-Attempts $batch
        if ($p.Count) { Add-Result 'S3 coord-disconnect' 'FAIL' ($p -join '; ') }
        else { Add-Result 'S3 coord-disconnect' 'PASS' "all $($batch.Count) jobs committed once through a 20 s hang and a restart; attempts: $($att -join ', ')" }
    } catch { Add-Result 'S3 coord-disconnect' 'FAIL' $_.Exception.Message }

    # ---- S4 two-helper validation ----
    try {
        $batch = Submit-Jobs 's4' 2 2
        $p = Assert-Batch $batch 600
        $ids = ($batch | ForEach-Object { "'$($_.JobId)'" }) -join ','
        $both = Sql "SELECT count(*) FROM task_results r JOIN tasks t ON t.id=r.task_id WHERE t.job_id IN ($ids) AND r.device_id IN ('$devA','$devB')"
        if ($both[0] -ne '4') { $p += "results from A and B: $($both[0]) (want 4)" }
        if ($p.Count) { Add-Result 'S4 two-helper' 'FAIL' ($p -join '; ') }
        else { Add-Result 'S4 two-helper' 'PASS' 'each task committed only after A and B returned the same hash' }
    } catch { Add-Result 'S4 two-helper' 'FAIL' $_.Exception.Message }

    # ---- S5 tariff boundary on helper B ----
    try {
        Stop-Helper 'B'
        $now = [DateTime]::UtcNow
        $t = $now.AddSeconds(90)
        $boundary = (New-Object DateTime $t.Year, $t.Month, $t.Day, $t.Hour, $t.Minute, 0, ([DateTimeKind]::Utc)).AddMinutes(1)   # whole minute 90-150 s away
        $start = $boundary.AddHours(-3)
        $bUnix = [DateTimeOffset]::new($boundary).ToUnixTimeSeconds()
        $profile = @{ profile_id = 'fleet-night'; version = 1; timezone = 'UTC'
            binding = @{ interface = 'ethernet'; connection_id = 'fleet-lan' }
            windows = @(@{ days = @('Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'); start = $start.ToString('HH:mm:ss'); end = $boundary.ToString('HH:mm:ss'); marginal_price_per_gb = 0.0; max_bytes = 10000000000 })
            outside_window = 'block'
            billing = @{ reset_rule = @{ day_of_month = 1 }; remaining_bytes = 10000000000; safety_reserve_percent = 0 }
            policy = @{ allow_cellular_fallback = $false; boundary_guard_seconds = 0; strict_zero_outside_window = $false } }
        $cfgB = $helpers['B'].Cfg.Clone()
        $cfgB.network_budget = @{ metered = $profile }
        Write-Json $cfgB $helpers['B'].Config
        Start-Helper 'B'
        Log ("helper B metered; window closes {0:HH:mm:ss} UTC" -f $boundary)
        # Jobs across the boundary: one batch now, one right after the boundary.
        $batch1 = Submit-Jobs 's5a' $JobsPerScenario 1
        while ([DateTime]::UtcNow -lt $boundary.AddSeconds(3)) { Start-Sleep -Milliseconds 500 }
        $batch2 = Submit-Jobs 's5b' $JobsPerScenario 1
        $p = (Assert-Batch $batch1 600) + (Assert-Batch $batch2 600)
        # Owner limit: nothing after the boundary (2 s slack for host vs Docker VM clock skew).
        $lateLeases = Sql "SELECT count(*) FROM leases WHERE device_id='$devB' AND issued_at >= $($bUnix + 2)"
        $lateBlobs = Sql "SELECT count(*) FROM blobs WHERE uploader='$devB' AND created_at >= $($bUnix + 2)"
        if ($lateLeases[0] -ne '0') { $p += "B got $($lateLeases[0]) lease(s) after the window closed" }
        if ($lateBlobs[0] -ne '0') { $p += "B uploaded $($lateBlobs[0]) blob(s) after the window closed" }
        $bAfter = Sql ("SELECT count(*) FROM task_results r JOIN tasks t ON t.id=r.task_id WHERE r.device_id='$devB' AND t.job_id IN (" +
            (($batch2 | ForEach-Object { "'$($_.JobId)'" }) -join ',') + ")")
        if ($bAfter[0] -ne '0') { $p += "B returned $($bAfter[0]) result(s) for jobs submitted after the boundary" }
        $att = (Get-Attempts $batch1) + (Get-Attempts $batch2)
        if ($p.Count) { Add-Result 'S5 tariff-boundary' 'FAIL' ($p -join '; ') }
        else { Add-Result 'S5 tariff-boundary' 'PASS' "all jobs committed; B: 0 leases and 0 uploads after the boundary; attempts: $($att -join ', ')" }
    } catch { Add-Result 'S5 tariff-boundary' 'FAIL' $_.Exception.Message }

    # ---- Global invariants over the whole run ----
    try {
        $dbl = Sql 'SELECT count(*) FROM tasks t JOIN jobs j ON j.id=t.job_id WHERE (SELECT count(*) FROM task_results r WHERE r.task_id=t.id) > j.validation_count'
        $left = @(Get-Workers).Count
        if ($dbl[0] -ne '0' -or $left -ne 0) { Add-Result 'G invariants' 'FAIL' "tasks with extra results: $($dbl[0]); live workers: $left" }
        else { Add-Result 'G invariants' 'PASS' 'no task has more results than its validation count; no sandbox worker left running' }
    } catch { Add-Result 'G invariants' 'FAIL' $_.Exception.Message }

    # ---- Expected failures (gaps). Each makes a real check so a closed gap shows up as UNEXPECTED-PASS. ----
    $tree = Get-Native cargo @('tree', '--manifest-path', 'core/Cargo.toml', '-p', 'nc-agent', '-e', 'normal', '--prefix', 'none')
    if (@($tree | Select-String '^nc-net ').Count -eq 0) {
        Add-Result 'X1 p2p-direct-relay' 'EXPECTED-FAIL' 'nc-agent does not depend on nc-net: all payloads go through coordinator blob staging; no direct QUIC, no relay fallback, no LAN-only mode' 'G1/G2'
    } else { Add-Result 'X1 p2p-direct-relay' 'UNEXPECTED-PASS' 'nc-agent now links nc-net; add a real direct/relay scenario here' 'G1/G2' }
    $disp = @(Select-String -Path 'server/crates/nc-coordinator/src/*.rs' -Pattern 'third|tie.?break|verifier_device' -ErrorAction SilentlyContinue).Count
    if ($disp -eq 0) {
        Add-Result 'X2 dispute-resolution' 'EXPECTED-FAIL' 'mismatching replicas end the task as "disputed" (terminal); no third helper or trusted verifier, no distinct-owner rule' 'G3'
    } else { Add-Result 'X2 dispute-resolution' 'UNEXPECTED-PASS' 'coordinator mentions a tie-breaker; add a scenario' 'G3' }
    $appSubmit = @(Select-String -Path 'app/lib/**/*.dart', 'app/lib/*.dart' -Pattern 'submitJob|submit_job' -ErrorAction SilentlyContinue).Count
    if ($appSubmit -eq 0) {
        Add-Result 'X3 app-requester' 'EXPECTED-FAIL' 'the Flutter app has no job submit flow (CLI only)' 'G6'
    } else { Add-Result 'X3 app-requester' 'UNEXPECTED-PASS' 'app references submit; add a UI scenario' 'G6' }
    if (-not (Test-Path 'packaging') -and @(Select-String -Path 'core/crates/nc-agent/src/*.rs' -Pattern 'windows_service|service_dispatcher|sd_notify' -ErrorAction SilentlyContinue).Count -eq 0) {
        Add-Result 'X4 service-install' 'EXPECTED-FAIL' 'no Windows service, installer or Linux systemd unit: helpers run only as console processes started by hand' 'G7/G8'
    } else { Add-Result 'X4 service-install' 'UNEXPECTED-PASS' 'service/packaging code found; add an install scenario' 'G7/G8' }
} finally {
    Log 'teardown'
    foreach ($j in $submits) { if (-not $j.Proc.HasExited) { Stop-Process -Id $j.Proc.Id -Force -ErrorAction SilentlyContinue } }
    foreach ($name in @($helpers.Keys)) { try { Stop-Helper $name } catch { } }
    try {
        if ($dockerUp) {
            $null = Get-Native docker ($composeArgs + @('unpause', 'coordinator'))
            Invoke-Native -Exe docker -CommandArgs ($composeArgs + @('down', '--volumes', '--remove-orphans'))
            if (-not $KeepImage) { $null = Get-Native docker @('image', 'rm', $image) }
        }
    } finally {
        foreach ($n in $secretNames) { [Environment]::SetEnvironmentVariable($n, $previous[$n], 'Process') }
        if (-not $KeepRunDir) { Remove-Item -Recurse -Force $runDir -ErrorAction SilentlyContinue } else { Log "run folder kept: $runDir" }
        Pop-Location
    }
}

Write-Output ''
Write-Output 'Fleet e2e summary'
$results | Format-Table -AutoSize -Wrap Scenario, Result, Gap, Detail | Out-String -Width 220 | Write-Output
$bad = @($results | Where-Object { $_.Result -in @('FAIL', 'UNEXPECTED-PASS') }).Count
if ($bad) { Write-Output "$bad scenario(s) failed or need the test updated"; exit 1 }
Write-Output 'all scenarios passed or failed as expected'
exit 0
