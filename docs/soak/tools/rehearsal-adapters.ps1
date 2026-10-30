# TEST ONLY. Appended to a private copy of cr0-common.ps1 by the rehearsal runner.
# No executable is launched and no HTTP request leaves this process.
$script:scenario = $env:CR0_REHEARSAL_SCENARIO
$script:seconds = 0.0
$script:epoch = [DateTimeOffset]::Parse('2026-01-01T00:00:00Z')
$script:realSave = ${function:Save-SoakJson}
$script:realWrite = ${function:Write-SoakText}
$script:signalCalls = 0
$script:killCalls = 0

function New-SoakClock {
    $fake = [pscustomobject]@{}
    $fake | Add-Member ScriptProperty Elapsed { [TimeSpan]::FromSeconds($script:seconds) }
    return $fake
}
function Get-SoakUtcNow { $script:epoch.AddSeconds($script:seconds) }
function Wait-SoakTick { $script:seconds += 10 }
function Get-SoakExecutableVersion { 'rehearsal: not an application executable' }
function Enable-SoakChildCtrlC { }
function Get-Process { param($Name, $ErrorAction) }
function Test-SoakLaunchEnvironment { param($Bind) }
function Get-SoakVolumes {
    param($Volumes)
    if ($script:scenario -eq 'disk_read_failure' -and $script:seconds -ge 20) { throw 'injected capacity failure' }
    $free = 60GB
    if ($script:scenario -eq 'preflight_disk') { $free = 49GB }
    # Suppression ends at sample 70; warmup ends at sample 1870.
    if (($script:scenario -eq 'disk_startup' -and $script:seconds -ge 20) -or
        ($script:scenario -eq 'disk_warmup' -and $script:seconds -ge 80) -or
        ($script:scenario -eq 'disk_baseline' -and $script:seconds -ge 1880)) { $free = 9GB }
    foreach ($volume in $Volumes) { @{ volume=$volume; free_bytes=$free; total_bytes=100GB } }
}
function Start-Process {
    param($FilePath, $ArgumentList, $WindowStyle, [switch]$PassThru, $RedirectStandardOutput, $RedirectStandardError)
    [IO.File]::WriteAllText($RedirectStandardOutput, '')
    [IO.File]::WriteAllText($RedirectStandardError, '')
    [IO.File]::WriteAllText((Join-Path $Root 'fake-child-started'), 'fixture only')
    $script:child = [pscustomobject]@{
        Id=424242; StartTime=$script:epoch.UtcDateTime; HasExited=$false; ExitCode=0
        WorkingSet64=1MB; PrivateMemorySize64=1MB; Threads=@(1); HandleCount=10
        TotalProcessorTime=[TimeSpan]::Zero
    }
    $script:child | Add-Member ScriptMethod Refresh {
        if ($script:scenario -eq 'child_exit' -and $script:seconds -ge 20) {
            $this.HasExited=$true; $this.ExitCode=7
        }
    }
    return $script:child
}
function Invoke-WebRequest {
    param($Uri, $TimeoutSec, $Headers)
    if ($Uri.EndsWith('/healthz')) {
        return @{StatusCode=$(if ($script:scenario -eq 'health_failure' -or
            ($script:scenario -eq 'unhealthy_endpoint' -and $script:seconds -eq 88270)) {503} else {200})}
    }
    if ($Uri.EndsWith('/metrics')) {
        if ($script:scenario -eq 'metrics_failure') { throw 'metrics unavailable' }
        if ($script:scenario -eq 'bad_endpoint' -and $script:seconds -eq 88270) { throw 'endpoint unavailable' }
        $names = @('start_time_seconds', 'torrents', 'dht_sample_suppression_seconds',
            'dht_sample_cooling', 'dht_sample_inflight', 'dht_sample_frontier',
            'dht_peer_active', 'dht_peer_queued', 'dht_peer_admitted_total',
            'dht_peer_rejected_total', 'dht_peer_cancelled_total', 'dht_lookup_scheduled',
            'dht_lookup_deadlines_total', 'dht_lookups_completed_total', 'dht_outstanding',
            'fetch_lookups', 'fetch_active', 'fetch_pending')
        $lines = foreach ($name in $names) {
            $value = if ($name -eq 'dht_sample_suppression_seconds' -and $script:seconds -lt 70) { 1 } else { 0 }
            "lodestar_$name $value"
        }
        return @{Content=($lines -join "`n")}
    }
    if ($Uri -eq "http://$Bind/") { return @{Content='fixture index'} }
    throw 'Unexpected HTTP destination in rehearsal'
}
function Write-SoakText {
    param($Path, $Content, $Append)
    if ($script:scenario -eq 'evidence_write' -and $Path.EndsWith('.prom')) { throw 'injected disk write failure' }
    & $script:realWrite $Path $Content $Append
}
function Save-SoakJson {
    param($Value, $Path)
    if ($script:scenario -eq 'result_write' -and $Path.EndsWith('result.json')) { throw 'injected result write failure' }
    if ($script:scenario -eq 'status_write' -and $Path.EndsWith('status.json')) { throw 'injected status write failure' }
    & $script:realSave $Value $Path
}
function Add-Content {
    param($LiteralPath, [Parameter(ValueFromPipeline)]$Value)
    process {
        if ($script:scenario -eq 'event_write') { throw 'injected event logging failure' }
        Microsoft.PowerShell.Management\Add-Content -LiteralPath $LiteralPath -Value $Value
    }
}
function Get-Item {
    param($LiteralPath, $ErrorAction)
    if ($script:scenario -eq 'log_growth' -and $script:seconds -ge 20 -and $LiteralPath.EndsWith('.log')) {
        return @{Length=1GB}
    }
    Microsoft.PowerShell.Management\Get-Item -LiteralPath $LiteralPath -ErrorAction Stop
}
function Test-Path {
    param($LiteralPath)
    if ($LiteralPath.EndsWith('stop.request') -and $script:seconds -ge 20 -and
        $script:scenario -in @('event_write','result_write','signal_failure','forced_stop','stale_identity','adapter_failure')) { return $true }
    Microsoft.PowerShell.Management\Test-Path -LiteralPath $LiteralPath
}
function New-SoakWindowsProcessAdapter {
    param($Process, $Identity, $SignalScript)
    if ($script:scenario -eq 'adapter_failure') { throw 'injected cleanup adapter failure' }
    $scenario = $script:scenario
    return @{
        State = {
            @{ identity=if ($scenario -eq 'stale_identity') { '424242:reused' } else { $Identity }
               exited=$Process.HasExited; exit_code=$Process.ExitCode }
        }.GetNewClosure()
        Signal = {
            $script:signalCalls++
            [IO.File]::WriteAllText((Join-Path $Root 'fake-signal'), 'fixture only')
            if ($scenario -eq 'signal_failure') { throw 'signal failed' }
            if ($scenario -ne 'forced_stop') { $Process.HasExited=$true }
            return $true
        }.GetNewClosure()
        Wait = { param($ms) return $Process.HasExited }.GetNewClosure()
        Kill = {
            $script:killCalls++
            [IO.File]::WriteAllText((Join-Path $Root 'fake-kill'), 'fixture only')
            $Process.HasExited=$true; $Process.ExitCode=9
        }.GetNewClosure()
    }
}
