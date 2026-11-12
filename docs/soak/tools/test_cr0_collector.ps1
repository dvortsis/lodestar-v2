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

# Service mode (M8-WIN-1): attach skips ownership checks but never the bind rules.
Test-SoakLaunchEnvironment '127.0.0.1:8090' { @('lodestar') } { @{tcp=@(@{Port=8090});udp=@(@{Port=6881})} } $true
$rejected=$false
try { Test-SoakLaunchEnvironment '0.0.0.0:8090' { @() } $emptyPorts $true } catch { $rejected=$true }
Assert $rejected 'unsafe bind accepted in attach mode'
$rejected=$false
try { Test-SoakLaunchEnvironment '127.0.0.1:8090' { @('dockerd') } $emptyPorts $true } catch { $rejected=$true }
Assert $rejected 'docker accepted in attach mode'
$identity = Get-SoakServiceIdentity 'Lodestar' { param($n) Assert ($n -eq 'Lodestar') 'service name'; @{ pid = 7; start_ticks = 99 } }
Assert ($identity.pid -eq 7 -and $identity.start_ticks -eq 99) 'service identity'
$rejected=$false
try { Get-SoakServiceIdentity 'Lodestar' { $null } | Out-Null } catch { $rejected=$true }
Assert $rejected 'stopped service accepted'
Assert ((Get-SoakImagePath '"C:\Program Files\Lodestar\lodestar.exe" serve') -eq 'C:\Program Files\Lodestar\lodestar.exe') 'quoted image path'
Assert ((Get-SoakImagePath 'C:\Lodestar\lodestar.exe serve') -eq 'C:\Lodestar\lodestar.exe') 'bare image path'

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
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('cr0-life-' + [Guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($fixture)
$life = Join-Path $fixture 'lifecycle.json'
try {
    Save-SoakLifecycle $life 'upgrade' @{ from_version = '0.5.1'; to_version = '1.0.0' }
    Save-SoakLifecycle $life 'uninstall' @{ service_present = $false }
    $record = Get-Content -LiteralPath $life -Raw | ConvertFrom-Json
    Assert ($record.schema_version -eq 1 -and $record.upgrade.to_version -eq '1.0.0' -and $record.uninstall.service_present -eq $false) 'lifecycle steps merge'
    $exe = Join-Path $fixture 'lodestar.exe'
    [IO.File]::WriteAllText($exe, 'fixture')
    $image = Get-SoakServiceImage 'Lodestar' { '"' + $exe + '" serve' }
    Assert ($image.sha256 -eq (Get-FileHash -LiteralPath $exe).Hash) 'image hash'
    # Attach binding: real files, only the service and process calls are stubbed.
    $other = Join-Path $fixture 'other.exe'
    [IO.File]::WriteAllText($other, 'older build')
    $registered = (Get-FileHash -LiteralPath $exe).Hash
    function New-BindProbe([string]$Running, [string]$Registered = $registered) {
        @{ Identity = { param($n) @{ pid = 7; start_ticks = 99 } }
           ProcessImage = { param($procId) $Running }.GetNewClosure()
           ServiceImage = { param($n) $Registered }.GetNewClosure() }
    }
    Assert-SoakAttachBinding 'Lodestar' $exe (New-BindProbe $exe)
    $rejected = $false; try { Assert-SoakAttachBinding 'Lodestar' $exe (New-BindProbe $other) } catch { $rejected = $true }
    Assert $rejected 'a different running binary was accepted'
    $rejected = $false; try { Assert-SoakAttachBinding 'Lodestar' $other (New-BindProbe $exe) } catch { $rejected = $true }
    Assert $rejected 'a different -Exe was accepted'
    $rejected = $false; try { Assert-SoakAttachBinding 'Lodestar' $exe (New-BindProbe '') } catch { $rejected = $true }
    Assert $rejected 'an unreadable process path was accepted'
} finally {
    foreach ($f in @($life, $exe, $other)) { if ($f -and [IO.File]::Exists($f)) { [IO.File]::Delete($f) } }
    [IO.Directory]::Delete($fixture)
}
# Every changed script must parse: a slip in a branch would otherwise first show on the owner's PC.
foreach ($script in @('cr0-soak.ps1', 'cr0-common.ps1', 'test_cr0_collector.ps1')) {
    $parseErrors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot $script), [ref]$null, [ref]$parseErrors)
    Assert ($parseErrors.Count -eq 0) "parse errors in $script"
}

# Evidence producers (M8-WIN-1), with the Windows calls replaced.
Assert ((Get-SoakTorrentCount '127.0.0.1:8090' { param($u) "# HELP`nlodestar_torrents 1234`r`nlodestar_other 1`r`n" }) -eq 1234) 'torrent count'
$rejected=$false
try { Get-SoakTorrentCount '127.0.0.1:8090' { 'lodestar_other 1' } | Out-Null } catch { $rejected=$true }
Assert $rejected 'missing torrent metric accepted'
Assert ((Get-SoakInstalledVersion 'x.exe' { param($e) 'lodestar 1.0.0' }) -eq '1.0.0') 'version prefix stripped'

$lifeDir = Join-Path ([IO.Path]::GetTempPath()) ('cr0-prod-' + [Guid]::NewGuid().ToString('N'))
$dataDir = Join-Path $lifeDir 'data'
[void][IO.Directory]::CreateDirectory($dataDir)
$lifePath = Join-Path $lifeDir 'lifecycle.json'
$exePath = Join-Path $lifeDir 'installed.exe'
$sameImage = Join-Path $lifeDir 'running-same.exe'
$otherImage = Join-Path $lifeDir 'running-other.exe'
$dataFile = Join-Path $dataDir 'db'
$h0 = 'A' * 64
foreach ($f in @($exePath, $sameImage)) { [IO.File]::WriteAllText($f, 'new build') }
[IO.File]::WriteAllText($otherImage, 'old build')
# Real files and folders: only the Windows service and process calls are replaced.
function New-UpgradeProbe([string]$Image = $sameImage, [hashtable]$Over = @{}) {
    $probe = @{
        Identity = { param($n) @{ pid = 7; start_ticks = 500 } }
        Status = { param($n) 'Running' }
        ProcessImage = { param($procId) $Image }.GetNewClosure()
        Torrents = { param($b) 520 }
        Version = { param($e) '1.0.0' }
        ServiceImage = { param($n) 'B' * 64 }
    }
    foreach ($key in $Over.Keys) { $probe[$key] = $Over[$key] }
    $probe
}
function Read-Step([string]$Step) { (Get-Content -LiteralPath $lifePath -Raw | ConvertFrom-Json).$Step }
function Save-Up([hashtable]$Probe, [string]$Data = $dataDir, [long]$Previous = 100) {
    Save-SoakUpgrade $lifePath '0.5.1' $h0 500 $Previous -DataDir $Data -Exe $exePath -Probe $Probe
}
try {
    [IO.File]::WriteAllText($dataFile, 'x')
    Save-Up (New-UpgradeProbe)
    $up = Read-Step 'upgrade'
    Assert ($up.service_running -and $up.service_restarted -and $up.image_matches_installed -and $up.data_present) 'upgrade pass booleans'
    Assert ($up.from_image_sha256 -eq $h0 -and $up.to_image_sha256 -eq ('B' * 64) -and $up.torrents_before -eq 500 -and $up.torrents_after -eq 520) 'upgrade values'
    Save-Up (New-UpgradeProbe) -Previous 500
    Assert (-not (Read-Step 'upgrade').service_restarted) 'same start time is not a restart'
    Save-Up (New-UpgradeProbe $otherImage)
    Assert (-not (Read-Step 'upgrade').image_matches_installed) 'a different running binary is not the installed one'
    Save-Up (New-UpgradeProbe $sameImage @{ Status = { param($n) 'Stopped' } })
    Assert (-not (Read-Step 'upgrade').service_running) 'stopped service'
    [IO.File]::Delete($dataFile)
    Save-Up (New-UpgradeProbe)
    Assert (-not (Read-Step 'upgrade').data_present) 'empty data folder'
    Save-Up (New-UpgradeProbe) -Data (Join-Path $lifeDir 'missing')
    Assert (-not (Read-Step 'upgrade').data_present) 'missing data folder'
    $rejected=$false
    try { Save-Up (New-UpgradeProbe $sameImage @{ Identity = { param($n) throw 'Service is not running.' } }) } catch { $rejected=$true }
    Assert $rejected 'stopped service accepted'
    foreach ($bad in @(@('not-a-hash', 500, 100), @($h0, 0, 100), @($h0, 500, 0))) {
        $rejected=$false
        try { Save-SoakUpgrade $lifePath '0.5.1' $bad[0] $bad[1] $bad[2] -DataDir $dataDir -Exe $exePath -Probe (New-UpgradeProbe) } catch { $rejected=$true }
        Assert $rejected "invalid upgrade baseline accepted: $($bad -join ',')"
    }
    Save-SoakUninstall $lifePath 'kept' -DataDir $dataDir -ProgramDir (Join-Path $lifeDir 'no-program') -Probe @{ ServicePresent = { param($n) $false } }
    $un = Read-Step 'uninstall'
    Assert (-not $un.service_present -and -not $un.program_files_present -and $un.data_present -and $un.data_policy -eq 'kept') 'uninstall record'
    Assert ((Read-Step 'upgrade') -ne $null) 'uninstall keeps the upgrade step'
    $rejected=$false
    try { Save-SoakUninstall $lifePath 'maybe' } catch { $rejected=$true }
    Assert $rejected 'unknown data policy accepted'
} finally {
    foreach ($f in @($lifePath, $exePath, $sameImage, $otherImage, $dataFile)) { if ([IO.File]::Exists($f)) { [IO.File]::Delete($f) } }
    [IO.Directory]::Delete($dataDir)
    [IO.Directory]::Delete($lifeDir)
}
'Collector fault-injection assertions passed.'
