$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'cr0-common.ps1')
function Assert($Condition, $Message) { if (-not $Condition) { throw $Message } }

$emptyPorts = { @{tcp=@(); udp=@()} }
Test-SoakLaunchEnvironment '127.0.0.1:8484' { @('pwsh') } $emptyPorts
foreach ($bind in @('0.0.0.0:8484','127.0.0.1:80','127.0.0.1:8484/path','user@127.0.0.1:8484')) {
    $rejected=$false
    try { Test-SoakLaunchEnvironment $bind { @() } $emptyPorts } catch { $rejected=$true }
    Assert $rejected "unsafe bind accepted: $bind"
}
foreach ($process in @('lodestar-cr0','Docker Desktop','com.docker.backend','dockerd','docker-proxy')) {
    $rejected=$false
    try { Test-SoakLaunchEnvironment '127.0.0.1:8484' { @($process) } $emptyPorts } catch { $rejected=$true }
    Assert $rejected "conflicting process accepted: $process"
}
foreach ($ports in @(@{tcp=@(@{Port=8484});udp=@()}, @{tcp=@();udp=@(@{Port=6881})})) {
    $rejected=$false
    try { Test-SoakLaunchEnvironment '127.0.0.1:8484' { @() } { $ports } } catch { $rejected=$true }
    Assert $rejected 'owned port accepted'
}
$rejected=$false
try { Test-SoakLaunchEnvironment '127.0.0.1:8484' { throw 'enumeration denied' } $emptyPorts } catch { $rejected=$true }
Assert $rejected 'unknown launch environment accepted'

$build = @{ schema_version=1; exe_sha256=('a'*64); source_commit=('b'*40); ui='embedded'; index_sha256=('c'*64); rustc='rustc test'; cargo='cargo test'; target='test' }
Test-SoakBuildRecord $build ('a'*64) ('b'*40) 'embedded'
foreach ($field in @('exe_sha256', 'source_commit', 'ui', 'index_sha256', 'rustc', 'cargo', 'target')) {
    $bad = $build.Clone(); $bad[$field] = ''
    $rejected = $false
    try { Test-SoakBuildRecord $bad ('a'*64) ('b'*40) 'embedded' } catch { $rejected = $true }
    Assert $rejected "invalid build provenance: $field"
}

$volumes = @(@{ free_bytes = 9GB })
Assert ((Get-SoakGuardReason $volumes 10GB 0 1GB 0) -eq 'disk_reserve_reached') 'disk reserve'
Assert ((Get-SoakGuardReason @(@{free_bytes=11GB}) 10GB 0 1GB 0) -eq $null) 'healthy disk'
Assert ((Get-SoakGuardReason @() 10GB 0 1GB 3) -eq 'disk_monitor_failed') 'failed monitor'
Assert ((Get-SoakGuardReason @() 10GB (1GB+1) 1GB 0) -eq 'redirected_logs_limit') 'log cap'
Assert (-not (Test-SoakEndpoint 86399 86400 $true 'start')) 'early endpoint'
Assert (-not (Test-SoakEndpoint 86400 86400 $false 'start')) 'invalid endpoint'
Assert (Test-SoakEndpoint 86400 86400 $true 'start') 'exact endpoint'
Assert (Test-SoakEndpoint 86401 86400 $true 'start') 'late endpoint'
Assert (-not (Test-SoakEndpoint 86401 86400 $true $null)) 'no baseline'

$script:exited = $false
$script:killed = $false
$script:waits = @()
$adapter = @{
    State = { @{identity='1:start'; exited=$script:exited; exit_code=0} }
    Signal = { $script:exited=$true; return $true }
    Wait = { param($ms) $script:waits += $ms; return $script:exited }
    Kill = { $script:killed=$true; $script:exited=$true }
}
$result = Stop-SoakChild '1:start' $adapter
Assert ($result.stop_mode -eq 'exited_0' -and $result.signal_delivered -and -not $script:killed) 'clean stop'
$script:exited=$false
$adapter.Signal = { throw 'injected signal failure' }
$result = Stop-SoakChild '1:start' $adapter
Assert ($result.stop_mode -eq 'forced_kill' -and $script:killed -and $result.errors.Count -eq 1) 'signal failure cleanup'
$script:killed=$false
$result = Stop-SoakChild '2:reused' $adapter
Assert ($result.stop_mode -eq 'unknown' -and -not $result.exit_confirmed -and -not $script:killed) 'never kill a reused PID'
$script:exited=$false
$script:killed=$false
$adapter.Wait = { param($ms) if ($ms -eq 60000) { throw 'wait failed' }; return $script:exited }
$result = Stop-SoakChild '1:start' $adapter
Assert ($result.exit_confirmed -and $script:killed -and $result.errors -contains 'graceful_wait_failed') 'wait failure fallback'
$script:saved=@()
$failed = Complete-SoakEvidence @{} 'status' 'result' { param($v,$p) $script:saved += $p; $script:published = $v | ConvertTo-Json -Depth 5; if ($p -eq 'status') { throw 'disk full' } }
Assert ($failed -eq 1 -and $script:saved.Count -eq 2) 'independent finalization'
$published = $script:published | ConvertFrom-Json
Assert (-not $published.finalization_complete -and $published.publication_failures -contains 'status') 'persist publication failure'

$fixture = Join-Path ([IO.Path]::GetTempPath()) ('cr0-test-' + [Guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($fixture)
$path = Join-Path $fixture 'status.json'
try {
    Save-SoakJson @{state='old'} $path
    $script:replaces=0
    try { Save-SoakJson @{state='new'} $path { param($a,$b) $script:replaces++; throw 'locked' }; throw 'unexpected success' }
    catch { Assert ($_.Exception.Message -ne 'unexpected success') 'replacement must fail' }
    Assert ($script:replaces -eq 3) 'bounded retry'
    Assert ((Get-Content -LiteralPath $path -Raw | ConvertFrom-Json).state -eq 'old') 'preserve last status'
    Assert (@(Get-ChildItem -LiteralPath $fixture -Filter '*.tmp').Count -eq 0) 'temporary cleanup'
    Save-SoakJson @{state='new'} $path
    Assert ((Get-Content -LiteralPath $path -Raw | ConvertFrom-Json).state -eq 'new') 'replacement success'
} finally {
    # Only this test's known file and then its empty directory; no recursive deletion.
    [IO.File]::Delete($path)
    [IO.Directory]::Delete($fixture)
}
'Collector fault-injection assertions passed.'
