param(
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][string]$Index,
    [Parameter(Mandatory)][string]$Root,
    [int]$Port = 18485
)
# Isolated packaging/supervision fixture. Never launches the public crawler.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'cr0-common.ps1')
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$Index = (Resolve-Path -LiteralPath $Index).Path
$Root = [IO.Path]::GetFullPath($Root)
if (Test-Path -LiteralPath $Root) { throw 'Smoke directory already exists.' }
if ($Port -lt 1024 -or $Port -gt 65535) { throw 'Invalid fixture port.' }
if ([Net.NetworkInformation.IPGlobalProperties]::GetIPGlobalProperties().GetActiveTcpListeners() | Where-Object Port -eq $Port) { throw 'Fixture port is in use.' }
[void][IO.Directory]::CreateDirectory($Root)
$data = Join-Path $Root 'fixture'
$oldEnv = @{}
Get-ChildItem Env:LODESTAR_* | ForEach-Object { $oldEnv[$_.Name]=$_.Value; Remove-Item -LiteralPath "Env:$($_.Name)" }
$env:LODESTAR_HTTP_BIND = "127.0.0.1:$Port"
$env:LODESTAR_DHT_ENABLED = 'false'
$env:LODESTAR_SCRAPE_ENABLED = 'false'
$env:LODESTAR_UPDATES = 'off'
$p = $null
$identity = $null
$outcome = @{ schema_version=1; checks_passed=$false; failure=$null; shutdown=$null; exe_sha256=(Get-FileHash -LiteralPath $Exe).Hash; index_sha256=(Get-FileHash -LiteralPath $Index).Hash }
try {
    Enable-SoakChildCtrlC
    # Explicit fixture mode also disables subscriptions and other outbound services.
    $p = Start-Process -FilePath $Exe -ArgumentList @('serve', '--fixture', ('"' + $data + '"')) -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput (Join-Path $Root 'stdout.private.log') -RedirectStandardError (Join-Path $Root 'stderr.private.log')
    $identity = "$($p.Id):$($p.StartTime.ToUniversalTime().Ticks)"
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $healthy = $false
    while ($watch.Elapsed.TotalSeconds -lt 120) {
        $p.Refresh()
        if ($p.HasExited) { throw 'Fixture exited before health became ready.' }
        try { $healthy = (Invoke-WebRequest "http://127.0.0.1:$Port/healthz" -TimeoutSec 2).StatusCode -eq 200 } catch { }
        if ($healthy) { break }
        Start-Sleep -Milliseconds 250
    }
    if (-not $healthy) { throw 'Fixture health deadline exceeded.' }
    $page = Invoke-WebRequest "http://127.0.0.1:$Port/" -Headers @{'Accept-Encoding'='identity'} -TimeoutSec 3
    $bytes = if ($page.Content -is [byte[]]) { $page.Content } else { [Text.Encoding]::UTF8.GetBytes([string]$page.Content) }
    if ([Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)) -ne $outcome.index_sha256) { throw 'Served UI does not match the production index.' }
    $metrics = (Invoke-WebRequest "http://127.0.0.1:$Port/metrics" -TimeoutSec 3).Content
    if ($metrics -notmatch '(?m)^lodestar_start_time_seconds ' -or $metrics -notmatch '(?m)^lodestar_torrents ') { throw 'Required metrics absent.' }
    $udp = (& netstat -ano -p udp | Out-String)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot inspect fixture UDP ownership.' }
    if ($udp -match ('(?m)^\s*UDP\s+.*\s' + $p.Id + '\s*$')) { throw 'Offline fixture unexpectedly owns a UDP socket.' }
    [IO.File]::WriteAllText((Join-Path $Root 'metrics.prom'), $metrics)
    $outcome.checks_passed = $true
} catch {
    # Error messages from this script are fixed, not raw application logs/tokens.
    $outcome.failure = 'packaging_or_supervision_check_failed'
    [Console]::Error.WriteLine($_.Exception.Message)
} finally {
    if ($null -ne $p) { $outcome.shutdown = Stop-SoakChild $identity (New-SoakWindowsProcessAdapter $p $identity) }
    Get-ChildItem Env:LODESTAR_* | ForEach-Object { Remove-Item -LiteralPath "Env:$($_.Name)" }
    foreach ($key in $oldEnv.Keys) { Set-Item -LiteralPath "Env:$key" -Value $oldEnv[$key] }
    Save-SoakJson $outcome (Join-Path $Root 'smoke.json')
}
if (-not $outcome.checks_passed -or -not $outcome.shutdown.signal_delivered -or
    -not $outcome.shutdown.exit_confirmed -or $outcome.shutdown.stop_mode -ne 'exited_0' -or $outcome.shutdown.errors.Count) { exit 1 }
'Packaged UI, offline fixture, metrics and graceful Windows shutdown passed.'
