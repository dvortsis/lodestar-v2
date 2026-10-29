param(
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][string]$Root,
    [Parameter(Mandatory)][string]$Commit,
    [string]$Bind = '127.0.0.1:8484',
    [int]$WarmupMinutes = 30,
    [int]$BaselineHours = 24
)
# Aggregate-only evidence. Raw process logs and the database remain local.
$ErrorActionPreference = 'Stop'
[Threading.Thread]::CurrentThread.CurrentCulture = [Globalization.CultureInfo]::InvariantCulture
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$Root = [IO.Path]::GetFullPath($Root)
if (Test-Path (Join-Path $Root 'run.json')) { throw 'Run already exists; use a fresh directory.' }
if (Get-Process -Name 'lodestar*' -ErrorAction SilentlyContinue) { throw 'Another Lodestar process is running.' }
if ($WarmupMinutes -lt 0 -or $BaselineHours -lt 1) { throw 'Invalid observation duration.' }
New-Item -ItemType Directory -Force $Root, (Join-Path $Root 'metrics') | Out-Null
$data = Join-Path $Root 'data'
if (Test-Path $data) { throw 'A fresh data directory is required.' }
New-Item -ItemType Directory $data | Out-Null
$events = Join-Path $Root 'events.log'
$statusPath = Join-Path $Root 'status.json'
function Note([string]$message) {
    "$([DateTimeOffset]::Now.ToString('o')) $message" | Add-Content -LiteralPath $events
}
function Save-Json($value, [string]$path) {
    $temp = "$path.tmp"
    $value | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $temp
    Move-Item -LiteralPath $temp -Destination $path -Force
}
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
    query_timeout_seconds = 4; suppression_seconds = 21604; cooldown_floor_seconds = 60
    targets = 'random'; warmup_minutes = $WarmupMinutes; baseline_hours = $BaselineHours
    ui = 'stub (same as prior Windows soak)'; all_other_settings = 'commit defaults'
}
Save-Json $settings (Join-Path $Root 'configuration.json')
Add-Type -Namespace Cr0Soak -Name Console -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError=true)] public static extern bool AttachConsole(uint pid);
[DllImport("kernel32.dll", SetLastError=true)] public static extern bool FreeConsole();
[DllImport("kernel32.dll", SetLastError=true)] public static extern bool SetConsoleCtrlHandler(IntPtr h, bool add);
[DllImport("kernel32.dll", SetLastError=true)] public static extern bool GenerateConsoleCtrlEvent(uint ev, uint grp);
'@
$started = [DateTimeOffset]::Now
$clock = [Diagnostics.Stopwatch]::StartNew()
$run = [ordered]@{
    commit = $Commit; exe = $Exe; sha256 = (Get-FileHash -LiteralPath $Exe -Algorithm SHA256).Hash
    version = (& $Exe --version | Out-String).Trim(); started = $started.ToString('o')
    timezone = (Get-TimeZone).Id; user_timezone = 'America/Denver'
    os = [Environment]::OSVersion.VersionString; logical_cpus = [Environment]::ProcessorCount
    collector_sha256 = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash
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
try {
    $p = Start-Process -FilePath $Exe -ArgumentList 'serve' -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput (Join-Path $Root 'lodestar.out.log') `
        -RedirectStandardError (Join-Path $Root 'lodestar.err.log')
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
        if ($endAt -ne $null -and $elapsed -ge $endAt) { $reason = 'baseline_window_complete'; break }
        if ($elapsed -ge (8 + $BaselineHours) * 3600) { $reason = 'overall_deadline_without_complete_baseline'; break }
        if ($elapsed -lt $nextSample) { Start-Sleep -Seconds 1; continue }
        $nextSample = $elapsed + 60
        $stamp = [DateTimeOffset]::Now
        $health = 0
        try { $health = [int](Invoke-WebRequest "http://$Bind/healthz" -TimeoutSec 10).StatusCode } catch {}
        if ($health -eq 200) { $badHealth = 0 } else { $badHealth++ }
        $metricsOk = $false
        $values = @{}
        try {
            $metrics = (Invoke-WebRequest "http://$Bind/metrics" -TimeoutSec 10).Content
            foreach ($name in @('start_time_seconds', 'torrents', 'dht_sample_suppression_seconds',
                'dht_sample_cooling', 'dht_sample_inflight', 'dht_sample_frontier',
                'dht_peer_active', 'dht_peer_queued', 'dht_peer_admitted_total',
                'dht_peer_rejected_total', 'dht_peer_cancelled_total', 'dht_lookup_scheduled',
                'dht_lookup_deadlines_total', 'dht_lookups_completed_total', 'dht_outstanding',
                'fetch_lookups', 'fetch_active', 'fetch_pending')) {
                $values[$name] = Read-Metric $metrics $name
            }
            $metrics | Set-Content -LiteralPath (Join-Path $Root ('metrics/{0:D5}.prom' -f $sample))
            $metricsOk = $true; $badMetrics = 0
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
                $metrics | Set-Content -LiteralPath (Join-Path $Root 'baseline-start.prom')
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
        } catch { $badMetrics++; Note "metrics unavailable or missing required fields: $($_.Exception.Message)" }
        if ($p.WorkingSet64 -gt 1GB) { $badMemory++ } else { $badMemory = 0 }
        $phase = if ($null -ne $baselineStart) { 'baseline' } elseif ($null -ne $baselineAt) { 'warmup' } else { 'startup' }
        $row = [ordered]@{
            time = $stamp.ToString('o'); elapsed_seconds = [math]::Round($elapsed, 1); phase = $phase
            process_id = $p.Id; health = $health; metrics_ok = $metricsOk
            working_set_bytes = $p.WorkingSet64; private_bytes = $p.PrivateMemorySize64
            threads = $p.Threads.Count; handles = $p.HandleCount; cpu_seconds = $p.TotalProcessorTime.TotalSeconds
            values = $values
        }
        $row | ConvertTo-Json -Depth 4 -Compress | Add-Content -LiteralPath (Join-Path $Root 'samples.jsonl')
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
    }
} catch { Note "collector failure: $($_.Exception.Message)" }
finally {
    if ($null -ne $p) {
        $p.Refresh()
        if (-not $p.HasExited) {
            try {
                (Invoke-WebRequest "http://$Bind/metrics" -TimeoutSec 10).Content |
                    Set-Content -LiteralPath (Join-Path $Root 'final.prom')
            } catch {}
            Note "stopping: $reason; sending Ctrl+C"
            [void][Cr0Soak.Console]::FreeConsole()
            if ([Cr0Soak.Console]::AttachConsole([uint32]$p.Id)) {
                [void][Cr0Soak.Console]::SetConsoleCtrlHandler([IntPtr]::Zero, $true)
                [void][Cr0Soak.Console]::GenerateConsoleCtrlEvent(0, 0)
                Start-Sleep -Seconds 2
                [void][Cr0Soak.Console]::FreeConsole()
                [void][Cr0Soak.Console]::SetConsoleCtrlHandler([IntPtr]::Zero, $false)
            } else { Note 'AttachConsole failed; graceful signal was not delivered' }
            if ($p.WaitForExit(60000)) { $stopMode = "exited_$($p.ExitCode)" }
            else { $p.Kill(); $p.WaitForExit(30000) | Out-Null; $stopMode = 'forced_kill' }
        } else { $stopMode = "already_exited_$($p.ExitCode)" }
    }
    $result = @{
        state = 'stopped'; reason = $reason; stop_mode = $stopMode; samples = $sample
        ended = [DateTimeOffset]::Now.ToString('o'); suppression_ended = $suppressionEndAt
        baseline_started = $baselineStart; elapsed_seconds = $clock.Elapsed.TotalSeconds
        assessment = 'Evidence collected; review required before any acceptance or performance claim.'
    }
    Save-Json $result $statusPath
    Save-Json $result (Join-Path $Root 'result.json')
    Note "done: $reason; stop=$stopMode"
}
