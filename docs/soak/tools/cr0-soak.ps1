param(
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][string]$Root,
    [Parameter(Mandatory)][string]$Commit,
    [string]$Bind = '127.0.0.1:8484',
    [int]$WarmupMinutes = 30,
    [int]$BaselineHours = 24,
    [long]$MinFreeBytes = 50GB,
    [long]$WarnFreeBytes = 20GB,
    [long]$StopFreeBytes = 10GB,
    [long]$MaxLogBytes = 1GB,
    [string[]]$AdditionalStoragePaths = @(),
    [Parameter(Mandatory)][ValidateSet('embedded', 'stub')][string]$Ui,
    [Parameter(Mandatory)][string]$BuildRecord
)
# Aggregate-only evidence. Raw process logs and the database remain local.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'cr0-common.ps1')
[Threading.Thread]::CurrentThread.CurrentCulture = [Globalization.CultureInfo]::InvariantCulture
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$Root = [IO.Path]::GetFullPath($Root)
if (Test-Path -LiteralPath $Root) { throw 'Run already exists; use a fresh directory.' }
Test-SoakLaunchEnvironment $Bind
if ($WarmupMinutes -lt 30 -or $BaselineHours -lt 24) { throw 'Baseline requires at least 30 minutes warmup and 24 measured hours.' }
if ($StopFreeBytes -le 0 -or $WarnFreeBytes -le $StopFreeBytes -or $MinFreeBytes -lt $WarnFreeBytes -or $MaxLogBytes -le 0) { throw 'Invalid storage guardrails.' }
$BuildRecord = (Resolve-Path -LiteralPath $BuildRecord).Path
$build = Get-Content -LiteralPath $BuildRecord -Raw | ConvertFrom-Json
$originalExeHash = (Get-FileHash -LiteralPath $Exe -Algorithm SHA256).Hash
Test-SoakBuildRecord $build $originalExeHash $Commit $Ui
$storagePaths = @($Root, [IO.Path]::GetTempPath()) + $AdditionalStoragePaths
$volumes = @($storagePaths | ForEach-Object { [IO.Path]::GetPathRoot([IO.Path]::GetFullPath($_)) } | Sort-Object -Unique)
function Read-Volumes {
    Get-SoakVolumes $volumes
}
foreach ($volume in @(Read-Volumes)) {
    if ($volume.free_bytes -lt $MinFreeBytes) { throw 'Insufficient free disk space for soak launch.' }
}
New-Item -ItemType Directory -Force $Root, (Join-Path $Root 'metrics') | Out-Null
$data = Join-Path $Root 'data'
if (Test-Path $data) { throw 'A fresh data directory is required.' }
New-Item -ItemType Directory $data | Out-Null
$events = Join-Path $Root 'events.log'
$statusPath = Join-Path $Root 'status.json'
function Note([string]$message) {
    try { "$((Get-SoakUtcNow).ToString('o')) $message" | Add-Content -LiteralPath $events }
    catch { [Console]::Error.WriteLine('Soak event log write failed; cleanup remains enabled.') }
}
function Save-Json($value, [string]$path) {
    try { Save-SoakJson $value $path }
    catch { $script:reason = 'evidence_write_error'; throw }
}
function Write-Evidence([string]$Path, [string]$Content, [switch]$Append) {
    try {
        Write-SoakText $Path $Content ([bool]$Append)
    } catch { $script:reason = 'evidence_write_error'; throw }
}
# Freeze launch inputs. The launcher itself is already loaded; hashes identify
# both it and its common implementation, which must stay unchanged after launch.
$bin = New-Item -ItemType Directory (Join-Path $Root 'bin')
Copy-Item -LiteralPath $Exe -Destination (Join-Path $bin.FullName 'lodestar-cr0.exe')
$Exe = Join-Path $bin.FullName 'lodestar-cr0.exe'
if ((Get-FileHash -LiteralPath $Exe -Algorithm SHA256).Hash -ne $originalExeHash) { throw 'Executable copy hash mismatch' }
Copy-Item -LiteralPath $PSCommandPath -Destination (Join-Path $Root 'collector.ps1')
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'cr0-common.ps1') -Destination (Join-Path $Root 'cr0-common.ps1')
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'cr0-signal.ps1') -Destination (Join-Path $Root 'cr0-signal.ps1')
Copy-Item -LiteralPath $BuildRecord -Destination (Join-Path $Root 'build-record.json')
function Read-Metric([string]$text, [string]$name) {
    $match = [regex]::Match($text, '(?m)^lodestar_' + [regex]::Escape($name) + ' ([0-9.]+)\r?$')
    if (-not $match.Success) { throw "Missing metric: $name" }
    return [double]::Parse($match.Groups[1].Value, [Globalization.CultureInfo]::InvariantCulture)
}

# Avoid inheriting unrelated overrides. Only the explicit settings below apply.
Get-ChildItem Env:LODESTAR_* | ForEach-Object { Remove-Item -LiteralPath "Env:$($_.Name)" }
$env:LODESTAR_DATA_DIR = $data
$env:LODESTAR_HTTP_BIND = $Bind
$env:LODESTAR_DHT_PORT = '6881'
$env:LODESTAR_INTENSITY = 'balanced'
$env:LODESTAR_LOG = 'info'
$env:LODESTAR_UPDATES = 'off'
$settings = @{
    data = $data; http_bind = $Bind; dht_port = 6881; intensity = 'balanced'
    transport = 'TCP'; encryption = 'prefer'; updates = 'off'; fresh_data = $true
    storage_guardrails = @{ start_free_bytes = $MinFreeBytes; warn_free_bytes = $WarnFreeBytes; stop_free_bytes = $StopFreeBytes; max_redirected_log_bytes = $MaxLogBytes; check_seconds = 10; volumes = $volumes }; query_timeout_seconds = 4; suppression_seconds = 21604; cooldown_floor_seconds = 60
    targets = 'random'; warmup_minutes = $WarmupMinutes; baseline_hours = $BaselineHours
    ui = $Ui; all_other_settings = 'commit defaults'
}
Save-Json $settings (Join-Path $Root 'configuration.json')
$started = Get-SoakUtcNow
$clock = New-SoakClock
$run = [ordered]@{
    schema_version = 2; commit = $Commit; exe = $Exe; sha256 = (Get-FileHash -LiteralPath $Exe -Algorithm SHA256).Hash
    version = Get-SoakExecutableVersion $Exe; started = $started.ToString('o')
    timezone = (Get-TimeZone).Id; user_timezone = 'America/Denver'
    os = [Environment]::OSVersion.VersionString; logical_cpus = [Environment]::ProcessorCount
    collector_sha256 = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash
    common_sha256 = (Get-FileHash -LiteralPath (Join-Path $Root 'cr0-common.ps1') -Algorithm SHA256).Hash
    signal_sha256 = (Get-FileHash -LiteralPath (Join-Path $Root 'cr0-signal.ps1') -Algorithm SHA256).Hash
    build_record_sha256 = (Get-FileHash -LiteralPath $BuildRecord -Algorithm SHA256).Hash
    toolchain = @{ rustc = $build.rustc; cargo = $build.cargo; target = $build.target }
    network = @{ http_bind = $Bind; dht_port = 6881; proxy_configured = [bool]($env:HTTP_PROXY -or $env:HTTPS_PROXY -or $env:ALL_PROXY) }
    ui_verified = $false
    settings = $settings
}
$p = $null
$reason = 'collector_error'
$phase = 'startup'
$baselineStart = $null
$baselineAt = $null
$endAt = $null
$suppressionSeen = $false
$suppressionEndAt = $null
$badHealth = 0; $badMetrics = 0; $badBounds = 0; $badMemory = 0
$lastCompleted = 0; $lastProgressAt = 0.0
$sample = 0
$stopMode = 'not_started'
$identity = $null
$shutdown = $null
$finalizationErrors = 0
$diskFailures = 0
$diskHistory = @{}
$nextDisk = 0.0
$latestVolumes = @()
$lastLogs = 0L
try {
    Enable-SoakChildCtrlC
    $p = Start-Process -FilePath $Exe -ArgumentList 'serve' -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput (Join-Path $Root 'lodestar.out.log') `
        -RedirectStandardError (Join-Path $Root 'lodestar.err.log')
    $identity = "$($p.Id):$($p.StartTime.ToUniversalTime().Ticks)"
    $run.process_identity = $identity
    $run.process_id = $p.Id
    $run.collector_process_id = $PID
    Save-Json $run (Join-Path $Root 'run.json')
    Note "started pid=$($p.Id) commit=$Commit; waiting for actual sampling suppression to end"
    $nextSample = 5.0
    while ($true) {
        if (Test-Path (Join-Path $Root 'stop.request')) { $reason = 'operator_stop'; break }
        $p.Refresh()
        if ($p.HasExited) { $reason = "process_exited_$($p.ExitCode)"; break }
        $elapsed = $clock.Elapsed.TotalSeconds
        if ($elapsed -ge (8 + $BaselineHours) * 3600) { $reason = 'overall_deadline_without_complete_baseline'; break }
        if ($elapsed -ge $nextDisk) {
            $nextDisk = $elapsed + 10
            try {
                $latestVolumes = @(Read-Volumes)
                $lastLogs = 0L
                foreach ($name in @('lodestar.out.log', 'lodestar.err.log')) {
                    $file = Get-Item -LiteralPath (Join-Path $Root $name) -ErrorAction Stop
                    if ($null -ne $file) { $lastLogs += $file.Length }
                }
                $diskFailures = 0
            } catch { $diskFailures++; $latestVolumes = @() }
            foreach ($volume in $latestVolumes) {
                if ($volume.free_bytes -lt $WarnFreeBytes) { Note "disk reserve warning volume=$($volume.volume) free=$($volume.free_bytes)" }
                $history = @($diskHistory[$volume.volume]) | Where-Object { $null -ne $_ -and $_.elapsed -ge $elapsed - 600 }
                if ($history.Count -gt 0 -and $elapsed - $history[0].elapsed -ge 60) {
                    $rate = ($history[0].free - $volume.free_bytes) / ($elapsed - $history[0].elapsed)
                    if ($rate -gt 0 -and ($volume.free_bytes - $StopFreeBytes) / $rate -lt 3600) { Note "projected disk reserve within one hour volume=$($volume.volume)" }
                }
                $diskHistory[$volume.volume] = @($history) + @{ elapsed = $elapsed; free = $volume.free_bytes }
            }
            $diskRow = @{ time = (Get-SoakUtcNow).ToString('o'); elapsed_seconds = $elapsed; volumes = $latestVolumes; read_failures = $diskFailures; redirected_log_bytes = $lastLogs }
            Write-Evidence (Join-Path $Root 'disk.jsonl') ($diskRow | ConvertTo-Json -Depth 5 -Compress) -Append
            $guard = Get-SoakGuardReason $latestVolumes $StopFreeBytes $lastLogs $MaxLogBytes $diskFailures
            if ($null -ne $guard) { $reason = $guard; break }
        }
        if ($elapsed -lt $nextSample) { Wait-SoakTick; continue }
        $nextSample = $elapsed + 60
        $stamp = Get-SoakUtcNow
        $health = 0
        try { $health = [int](Invoke-WebRequest "http://$Bind/healthz" -TimeoutSec 3).StatusCode } catch {}
        if ($health -eq 200) { $badHealth = 0 } else { $badHealth++ }
        if ($health -eq 200 -and -not $run.ui_verified) {
            # Verify the bytes actually served by this process, not a caller's UI label.
            $page = Invoke-WebRequest "http://$Bind/" -Headers @{ 'Accept-Encoding' = 'identity' } -TimeoutSec 3
            $pageBytes = if ($page.Content -is [byte[]]) { $page.Content } else { [Text.Encoding]::UTF8.GetBytes([string]$page.Content) }
            $pageHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($pageBytes))
            if ($pageHash -ne $build.index_sha256) { $reason = 'embedded_ui_mismatch'; break }
            $run.ui_verified = $true
            $run.index_sha256 = $pageHash
            Save-Json $run (Join-Path $Root 'run.json')
        }
        $metricsOk = $false
        $values = @{}
        try {
            $metrics = (Invoke-WebRequest "http://$Bind/metrics" -TimeoutSec 3).Content
            foreach ($name in @('start_time_seconds', 'torrents', 'dht_sample_suppression_seconds',
                'dht_sample_cooling', 'dht_sample_inflight', 'dht_sample_frontier',
                'dht_peer_active', 'dht_peer_queued', 'dht_peer_admitted_total',
                'dht_peer_rejected_total', 'dht_peer_cancelled_total', 'dht_lookup_scheduled',
                'dht_lookup_deadlines_total', 'dht_lookups_completed_total', 'dht_outstanding',
                'fetch_lookups', 'fetch_active', 'fetch_pending')) {
                $values[$name] = Read-Metric $metrics $name
            }
            $metricsOk = $true; $badMetrics = 0
        } catch { $badMetrics++; Note 'metrics unavailable or missing required fields' }
        if ($metricsOk) {
            Write-Evidence (Join-Path $Root ('metrics/{0:D5}.prom' -f $sample)) $metrics
            if ($values.dht_sample_suppression_seconds -gt 0) { $suppressionSeen = $true }
            if ($suppressionSeen -and $values.dht_sample_suppression_seconds -eq 0 -and $null -eq $baselineAt) {
                $suppressionEndAt = $stamp.ToString('o')
                $baselineAt = $elapsed + $WarmupMinutes * 60
                $endAt = $baselineAt + $BaselineHours * 3600
                Note "sampling suppression ended; warming up for $WarmupMinutes minutes"
            }
            if ($null -ne $baselineAt -and $elapsed -ge $baselineAt -and $null -eq $baselineStart) {
                # Start at an observed sample so the saved baseline spans a full 24 hours.
                $baselineStart = $stamp.ToString('o')
                $endAt = $elapsed + $BaselineHours * 3600
                Write-Evidence (Join-Path $Root 'baseline-start.prom') $metrics
                Note "baseline started; collecting $BaselineHours hours"
            }
            $over = ($values.dht_peer_active + $values.dht_peer_queued -gt 32) -or
                ($values.dht_sample_cooling + $values.dht_sample_inflight -gt 200000) -or
                ($values.dht_sample_frontier -gt 20000) -or ($values.dht_outstanding -gt 1024) -or
                ($values.fetch_lookups -gt 256)
            if ($over) { $badBounds++ } else { $badBounds = 0 }
            if ($values.dht_lookups_completed_total -ne $lastCompleted -or $values.dht_peer_active -eq 0) {
                $lastCompleted = $values.dht_lookups_completed_total; $lastProgressAt = $elapsed
            }
        }
        if ($p.WorkingSet64 -gt 1GB) { $badMemory++ } else { $badMemory = 0 }
        $phase = if ($null -ne $baselineStart) { 'baseline' } elseif ($null -ne $baselineAt) { 'warmup' } else { 'startup' }
        $row = [ordered]@{
            schema_version = 2; sequence = $sample; process_identity = $identity; time = $stamp.ToString('o'); elapsed_seconds = [math]::Round($elapsed, 1); phase = $phase
            process_id = $p.Id; health = $health; metrics_ok = $metricsOk
            working_set_bytes = $p.WorkingSet64; private_bytes = $p.PrivateMemorySize64
            threads = $p.Threads.Count; handles = $p.HandleCount; cpu_seconds = $p.TotalProcessorTime.TotalSeconds
            volumes = $latestVolumes; redirected_log_bytes = $lastLogs; values = $values
        }
        Write-Evidence (Join-Path $Root 'samples.jsonl') ($row | ConvertTo-Json -Depth 6 -Compress) -Append
        Save-Json @{
            state = 'running'; phase = $phase; sample = $sample; latest = $row
            suppression_ended = $suppressionEndAt; baseline_started = $baselineStart
            expected_end = if ($null -ne $endAt) { $stamp.AddSeconds($endAt - $elapsed).ToString('o') } else { $null }
        } $statusPath
        $sample++
        if ($badHealth -ge 3) { $reason = 'three_consecutive_health_failures'; break }
        if ($badMetrics -ge 3) { $reason = 'three_consecutive_metrics_failures'; break }
        if ($badBounds -ge 3) { $reason = 'three_consecutive_capacity_violations'; break }
        if ($badMemory -ge 5) { $reason = 'rss_over_1GiB_for_five_samples'; break }
        if ($metricsOk -and $values.dht_peer_active -gt 0 -and $elapsed - $lastProgressAt -ge 300) {
            $reason = 'active_peer_requests_without_completion_for_five_minutes'; break
        }
        if (Test-SoakEndpoint $elapsed $endAt ($metricsOk -and $health -eq 200) $baselineStart) { $reason = 'baseline_window_complete'; break }
    }
} catch {
    if ($reason -ne 'evidence_write_error') { $reason = 'collector_error' }
    Note 'collector failure; evidence may be incomplete'
    [Console]::Error.WriteLine('Collector failed; starting protected finalization.')
} finally {
    if ($null -ne $p) {
        # No HTTP or evidence writes precede process cleanup.
        try {
            $adapter = New-SoakWindowsProcessAdapter $p $identity (Join-Path $Root 'cr0-signal.ps1')
            $shutdown = Stop-SoakChild $identity $adapter
            $stopMode = $shutdown.stop_mode
        } catch {
            $stopMode = 'unknown'
            $shutdown = @{ stop_mode='unknown'; exit_confirmed=$false; signal_delivered=$false; exit_code=$null; errors=@('cleanup_adapter_failed') }
            [Console]::Error.WriteLine('Process cleanup failed; process state remains uncertain.')
        }
    }
    $result = @{
        schema_version = 2; state = if ($null -ne $shutdown -and $shutdown.exit_confirmed) { 'stopped' } else { 'unknown' }; reason = $reason; stop_mode = $stopMode; samples = $sample
        shutdown = $shutdown; process_identity = $identity
        ended = (Get-SoakUtcNow).ToString('o'); suppression_ended = $suppressionEndAt
        baseline_started = $baselineStart; elapsed_seconds = $clock.Elapsed.TotalSeconds
        assessment = 'Evidence collected; review required before any acceptance or performance claim.'
    }
    $finalizationErrors = Complete-SoakEvidence $result $statusPath (Join-Path $Root 'result.json')
    if ($finalizationErrors -gt 0) { [Console]::Error.WriteLine('Final evidence publication failed; completion remains uncertain.') }
    Note "done: $reason; stop=$stopMode"
}
if ($reason -ne 'baseline_window_complete' -or $stopMode -ne 'exited_0' -or $finalizationErrors -gt 0 -or -not $shutdown.signal_delivered -or $shutdown.errors.Count -gt 0) { exit 1 }
exit 0
