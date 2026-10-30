param([string]$OutputRoot = (Join-Path ([IO.Path]::GetTempPath()) ('cr0-rehearsal-' + [guid]::NewGuid().ToString('N'))))
$ErrorActionPreference = 'Stop'
function Assert($Condition, $Message) { if (-not $Condition) { throw $Message } }
$OutputRoot = [IO.Path]::GetFullPath($OutputRoot)
if (Test-Path -LiteralPath $OutputRoot) { throw 'Use a new disposable rehearsal directory.' }
[void][IO.Directory]::CreateDirectory($OutputRoot)
# The production collector is copied unchanged. Only external effects in its
# common dependency are replaced. The fake executable is deliberately invalid.
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'cr0-soak.ps1') -Destination (Join-Path $OutputRoot 'collector.ps1')
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'cr0-signal.ps1') -Destination $OutputRoot
$common = (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'cr0-common.ps1') -Raw) + "`n" +
    (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'rehearsal-adapters.ps1') -Raw)
[IO.File]::WriteAllText((Join-Path $OutputRoot 'cr0-common.ps1'), $common)
$exe = Join-Path $OutputRoot 'never-executable.exe'
[IO.File]::WriteAllText($exe, 'TEST FIXTURE; NOT EXECUTABLE')
$build = @{
    schema_version=1; exe_sha256=(Get-FileHash -LiteralPath $exe).Hash; source_commit=('b'*40)
    ui='stub'; index_sha256=[Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes('fixture index')))
    rustc='fixture'; cargo='fixture'; target='fixture'
}
$buildPath = Join-Path $OutputRoot 'build.json'
$build | ConvertTo-Json | Set-Content -LiteralPath $buildPath
$cases = [ordered]@{
    preflight_disk='preflight'; disk_startup='disk_reserve_reached'; disk_warmup='disk_reserve_reached'
    disk_baseline='disk_reserve_reached'; disk_read_failure='disk_monitor_failed'; log_growth='redirected_logs_limit'
    evidence_write='evidence_write_error'; status_write='evidence_write_error'; event_write='operator_stop'
    result_write='operator_stop'; child_exit='process_exited_7'; signal_failure='operator_stop'
    forced_stop='operator_stop'; stale_identity='operator_stop'
    health_failure='three_consecutive_health_failures'; metrics_failure='three_consecutive_metrics_failures'; adapter_failure='operator_stop'
    exact_endpoint='baseline_window_complete'; bad_endpoint='baseline_window_complete'
    unhealthy_endpoint='baseline_window_complete'
}
$outcomes = @()
$previousScenario = $env:CR0_REHEARSAL_SCENARIO
try {
    foreach ($case in $cases.GetEnumerator()) {
        $env:CR0_REHEARSAL_SCENARIO = $case.Key
        $root = Join-Path $OutputRoot $case.Key
        & (Join-Path $PSHOME 'pwsh.exe') -NoProfile -NonInteractive -File (Join-Path $OutputRoot 'collector.ps1') `
            -Exe $exe -Root $root -Commit ('b'*40) -Ui stub -BuildRecord $buildPath *> (Join-Path $OutputRoot "$($case.Key).log")
        $code = $LASTEXITCODE
        if ($case.Key -eq 'preflight_disk') {
            Assert ($code -ne 0 -and -not (Test-Path -LiteralPath $root)) 'preflight must reject before child or run directory'
        } else {
            $resultPath = Join-Path $root 'result.json'
            if ($case.Key -eq 'result_write') {
                Assert (-not (Test-Path -LiteralPath $resultPath)) 'injected final result must be absent'
                $resultPath = Join-Path $root 'status.json'
            }
            Assert (Test-Path -LiteralPath $resultPath) "$($case.Key): missing termination evidence (see private fixture log)"
            $result = Get-Content -LiteralPath $resultPath -Raw | ConvertFrom-Json
            Assert ($result.reason -eq $case.Value) "$($case.Key): reason $($result.reason)"
            $complete = $case.Key -in @('exact_endpoint','bad_endpoint','unhealthy_endpoint')
            Assert (($code -eq 0) -eq $complete) "$($case.Key): wrong collector exit $code"
            if ($case.Key -in @('stale_identity','adapter_failure')) {
                Assert (-not $result.shutdown.exit_confirmed -and -not (Test-Path (Join-Path $root 'fake-signal')) -and -not (Test-Path (Join-Path $root 'fake-kill'))) 'stale identity must never be signalled or killed'
            } else { Assert $result.shutdown.exit_confirmed "$($case.Key): cleanup not confirmed" }
            if ($case.Key -in @('signal_failure','forced_stop')) {
                Assert ($result.stop_mode -eq 'forced_kill') "$($case.Key): expected bounded forced stop"
            }
            if ($case.Key -in @('result_write','status_write')) {
                Assert (-not $result.finalization_complete) "$($case.Key): publication failure hidden"
            }
            if ($case.Key -in @('disk_startup','disk_warmup','disk_baseline')) {
                $last = Get-Content -LiteralPath (Join-Path $root 'samples.jsonl') -Tail 1 | ConvertFrom-Json
                Assert ($last.phase -eq $case.Key.Substring(5)) "$($case.Key): wrong phase exercised"
            }
            if ($complete) {
                $rows = @(Get-Content -LiteralPath (Join-Path $root 'samples.jsonl') | ConvertFrom-Json)
                $baseline = @($rows | Where-Object phase -eq 'baseline')
                $span = $baseline[-1].elapsed_seconds - $baseline[0].elapsed_seconds
                Assert ($span -eq $(if ($case.Key -eq 'exact_endpoint') {86400} else {86460})) "$($case.Key): invalid endpoint span $span"
                Assert $baseline[-1].metrics_ok 'completion endpoint must be valid'
                if ($case.Key -eq 'bad_endpoint') { Assert (-not $baseline[-2].metrics_ok) 'failed endpoint must remain in evidence' }
                if ($case.Key -eq 'unhealthy_endpoint') { Assert ($baseline[-2].health -eq 503 -and $baseline[-2].metrics_ok) 'unhealthy endpoint with valid metrics must remain in evidence' }
            }
        }
        $outcomes += @{scenario=$case.Key; exit_code=$code; checks_passed=$true}
        Write-Output "$($case.Key): passed"
    }
    # Collector restarts must refuse existing evidence before any mutation.
    $root = Join-Path $OutputRoot 'exact_endpoint'
    $before = (Get-FileHash -LiteralPath (Join-Path $root 'result.json')).Hash
    & (Join-Path $PSHOME 'pwsh.exe') -NoProfile -NonInteractive -File (Join-Path $OutputRoot 'collector.ps1') `
        -Exe $exe -Root $root -Commit ('b'*40) -Ui stub -BuildRecord $buildPath *> (Join-Path $OutputRoot 'restart.log')
    Assert ($LASTEXITCODE -ne 0 -and (Get-FileHash -LiteralPath (Join-Path $root 'result.json')).Hash -eq $before) 'restart must preserve evidence'
    $outcomes += @{scenario='restart'; checks_passed=$true}
    $outcomes | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputRoot 'checks.json')
    Write-Output "$($outcomes.Count) integrated collector rehearsals passed; no application or network started."
} finally { $env:CR0_REHEARSAL_SCENARIO = $previousScenario }
