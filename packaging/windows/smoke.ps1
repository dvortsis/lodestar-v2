<#
.SYNOPSIS
  Smoke-test the Windows packages on a clean runner (needs an elevated shell).

  -Msi : install silently, check files, service, firewall rules and the Start-menu
         shortcut, run the installed binary and check /healthz, uninstall (data
         stays), then install with the Defender box ticked, uninstall with
         REMOVE_DATA=1 (data goes).
  -Zip : unpack the portable zip, run it with a temporary data folder, check /healthz.

  Until the service entrypoint (slice M6-win) merges, the service is installed and
  checked but not started: the binary is run directly instead.
#>
param(
  [string]$Msi,
  [string]$Zip,
  [int]$Port = 18090
)
$ErrorActionPreference = 'Stop'
if (-not $Msi -and -not $Zip) { throw 'pass -Msi <file> and/or -Zip <file>' }
# msiexec rejects paths that mix / and \ (error 1619), so use full Windows paths.
if ($Msi) { $Msi = (Resolve-Path $Msi).ProviderPath }
if ($Zip) { $Zip = (Resolve-Path $Zip).ProviderPath }

function Assert($ok, $msg) { if (-not $ok) { throw "SMOKE FAILED: $msg" } }

function Test-Serve([string]$exe, [string]$dataDir) {
  New-Item -ItemType Directory -Force $dataDir | Out-Null
  $env:LODESTAR_DATA_DIR = $dataDir
  $env:LODESTAR_HTTP_BIND = "127.0.0.1:$Port"
  $env:LODESTAR_DHT_ENABLED = 'false'
  $log = Join-Path $dataDir 'smoke.log'
  $p = Start-Process -FilePath $exe -ArgumentList 'serve' -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput $log -RedirectStandardError "$log.err"
  try {
    $ok = $false
    for ($i = 0; $i -lt 60 -and -not $ok; $i++) {
      if ($p.HasExited) { throw "lodestar exited early ($($p.ExitCode)): $(Get-Content $log, "$log.err" -Raw)" }
      try {
        $r = Invoke-WebRequest -UseBasicParsing -TimeoutSec 2 "http://127.0.0.1:$Port/healthz"
        $ok = ($r.StatusCode -eq 200)
      } catch { Start-Sleep -Seconds 1 }
    }
    Assert $ok '/healthz never answered'
    Write-Host "healthz: $((Invoke-WebRequest -UseBasicParsing "http://127.0.0.1:$Port/healthz").Content)"
    $index = (Invoke-WebRequest -UseBasicParsing "http://127.0.0.1:$Port/").Content
    Assert ($index -notmatch 'UI not built') '/ served the stub page: the web UI was not embedded'
    Assert ($index -match '_app/') '/ is not the SvelteKit index.html'
  } finally {
    # Windows has no SIGTERM; stop it hard (the graceful path is the service's job).
    if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force; $p.WaitForExit(15000) | Out-Null }
    Remove-Item Env:LODESTAR_DATA_DIR, Env:LODESTAR_HTTP_BIND, Env:LODESTAR_DHT_ENABLED -ErrorAction SilentlyContinue
  }
}

function Invoke-Msiexec([string[]]$msiArgs, [string]$what) {
  $p = Start-Process msiexec.exe -ArgumentList $msiArgs -Wait -PassThru
  Assert ($p.ExitCode -eq 0 -or $p.ExitCode -eq 3010) "$what failed (msiexec exit $($p.ExitCode)); see the log"
}

if ($Zip) {
  $work = Join-Path ([IO.Path]::GetTempPath()) ("lodestar-zip-" + [guid]::NewGuid())
  Expand-Archive -Path $Zip -DestinationPath $work
  $dir = (Get-ChildItem $work -Directory | Select-Object -First 1).FullName
  foreach ($f in 'lodestar.exe', 'LICENSE', 'README.txt', 'Start Lodestar.cmd') {
    Assert (Test-Path (Join-Path $dir $f)) "zip is missing $f"
  }
  Test-Serve (Join-Path $dir 'lodestar.exe') (Join-Path $work 'data')
  Remove-Item -Recurse -Force $work
  Write-Host "portable zip ok: $Zip"
}

if ($Msi) {
  $installDir = Join-Path $env:ProgramFiles 'Lodestar'
  $dataDir = Join-Path $env:ProgramData 'Lodestar'
  $logs = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
  Assert (-not (Test-Path $installDir)) 'Lodestar is already installed on this machine'

  # --- install (Defender box left off) ---
  Invoke-Msiexec @('/i', "`"$Msi`"", '/qn', '/norestart', '/l*v', "`"$logs\msi-install.log`"") 'install'
  Assert (Test-Path "$installDir\lodestar.exe") 'lodestar.exe not in Program Files'
  Assert (Test-Path $dataDir) 'data folder not created in ProgramData'
  $svc = Get-CimInstance Win32_Service -Filter "Name='Lodestar'"
  Assert ($null -ne $svc) 'service Lodestar is not registered'
  Assert ($svc.PathName -match 'lodestar\.exe') "service points at $($svc.PathName)"
  Assert ($svc.StartName -eq 'NT AUTHORITY\LocalService') "service runs as $($svc.StartName)"
  $web = Get-NetFirewallRule -DisplayName 'Lodestar web UI*' -ErrorAction SilentlyContinue
  Assert ($null -ne $web) 'web firewall rule missing'
  Assert ($web.Profile -eq 'Private') "web firewall rule profile is $($web.Profile), expected Private only"
  $dht = Get-NetFirewallRule -DisplayName 'Lodestar DHT*' -ErrorAction SilentlyContinue
  Assert ($null -ne $dht) 'DHT firewall rule missing'
  $shortcut = Get-ChildItem "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\Lodestar" -Filter '*.url' -ErrorAction SilentlyContinue
  Assert ($null -ne $shortcut) 'Start-menu shortcut missing'
  Assert ((Get-Content $shortcut.FullName -Raw) -match 'http://localhost:8090/') 'shortcut does not open the web UI'
  if (Get-Command Get-MpPreference -ErrorAction SilentlyContinue) {
    $ex = @((Get-MpPreference).ExclusionPath)
    Assert (-not ($ex -like "$dataDir*")) 'Defender exclusion was added without being asked'
  }

  # --- run the installed binary directly (the service entrypoint is slice M6-win) ---
  Test-Serve "$installDir\lodestar.exe" (Join-Path ([IO.Path]::GetTempPath()) 'lodestar-msi-data')

  # --- uninstall keeps data ---
  Set-Content -Path "$dataDir\keep-me.txt" -Value 'user data'
  Invoke-Msiexec @('/x', "`"$Msi`"", '/qn', '/norestart', '/l*v', "`"$logs\msi-uninstall.log`"") 'uninstall'
  Assert (-not (Test-Path "$installDir\lodestar.exe")) 'lodestar.exe left behind after uninstall'
  Assert ($null -eq (Get-CimInstance Win32_Service -Filter "Name='Lodestar'")) 'service left behind after uninstall'
  Assert ($null -eq (Get-NetFirewallRule -DisplayName 'Lodestar*' -ErrorAction SilentlyContinue)) 'firewall rules left behind'
  Assert (Test-Path "$dataDir\keep-me.txt") 'uninstall deleted the data folder; it must keep it'

  # --- reinstall with the Defender box ticked, uninstall with REMOVE_DATA=1 ---
  Invoke-Msiexec @('/i', "`"$Msi`"", '/qn', '/norestart', 'DEFENDER_EXCLUSION=1', '/l*v', "`"$logs\msi-install2.log`"") 'reinstall'
  if (Get-Command Get-MpPreference -ErrorAction SilentlyContinue) {
    try {
      $ex = @((Get-MpPreference).ExclusionPath)
      if ($ex -like "$dataDir*") { Write-Host 'defender exclusion added when asked: ok' }
      else { Write-Host '::warning::Defender exclusion not visible (Defender may be off or managed on this runner)' }
    } catch { Write-Host "::warning::could not read Defender settings: $_" }
  }
  Invoke-Msiexec @('/x', "`"$Msi`"", '/qn', '/norestart', 'REMOVE_DATA=1', '/l*v', "`"$logs\msi-uninstall2.log`"") 'uninstall with REMOVE_DATA'
  Assert (-not (Test-Path $dataDir)) 'REMOVE_DATA=1 left the data folder behind'
  if (Get-Command Get-MpPreference -ErrorAction SilentlyContinue) {
    try { Assert (-not (@((Get-MpPreference).ExclusionPath) -like "$dataDir*")) 'Defender exclusion left behind after uninstall' } catch [System.Management.Automation.RuntimeException] { throw } catch { }
  }
  Write-Host "msi ok: $Msi"
}
