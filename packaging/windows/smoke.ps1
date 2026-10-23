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
  # Releases are built without the `fixture` feature (`serve --fixture` test data).
  # The flag always exists; a build without the feature refuses it ("no fixture backend").
  $fx = (& $exe serve --fixture (Join-Path ([IO.Path]::GetTempPath()) 'lodestar-fixture-check') 2>&1 | Out-String)
  Assert ($fx -match 'no fixture backend') 'this build has the fixture feature; release builds must not'
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
  Assert ($svc.StartName -eq 'NT SERVICE\Lodestar') "service runs as $($svc.StartName)"
  $web = Get-NetFirewallRule -DisplayName 'Lodestar web UI*' -ErrorAction SilentlyContinue
  Assert ($null -ne $web) 'web firewall rule missing'
  Assert ($web.Profile -eq 'Private') "web firewall rule profile is $($web.Profile), expected Private only"
  $dht = Get-NetFirewallRule -DisplayName 'Lodestar DHT*' -ErrorAction SilentlyContinue
  Assert ($null -ne $dht) 'DHT firewall rule missing'
  Assert ($dht.Profile -eq 'Private') "DHT firewall rule profile is $($dht.Profile), expected Private only"
  $acl = Get-Acl $dataDir
  Assert $acl.AreAccessRulesProtected 'data folder still inherits permissions from ProgramData'
  $broad = @($acl.Access | Where-Object { $_.IdentityReference.Value -match '(Users|Everyone|Authenticated Users)$' })
  Assert ($broad.Count -eq 0) "data folder grants access to $($broad.IdentityReference -join ', ')"
  $shortcut = Get-ChildItem "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\Lodestar\*" -Include '*.lnk', '*.url' -ErrorAction SilentlyContinue
  Assert ($null -ne $shortcut) 'Start-menu shortcut missing'
  if (Get-Command Get-MpPreference -ErrorAction SilentlyContinue) {
    $ex = @((Get-MpPreference).ExclusionPath)
    Assert (-not ($ex -like "$dataDir*")) 'Defender exclusion was added without being asked'
  }

  # --- run the installed binary directly (the service entrypoint is slice M6-win) ---
  Test-Serve "$installDir\lodestar.exe" (Join-Path ([IO.Path]::GetTempPath()) 'lodestar-msi-data')

  # --- uninstall keeps data ---
  # What the service leaves behind once it has run: its marker plus data.
  Set-Content -Path "$dataDir\.lodestar-secured" -Value 'secured'
  Set-Content -Path "$dataDir\keep-me.txt" -Value 'user data'
  Invoke-Msiexec @('/x', "`"$Msi`"", '/qn', '/norestart', '/l*v', "`"$logs\msi-uninstall.log`"") 'uninstall'
  Assert (-not (Test-Path "$installDir\lodestar.exe")) 'lodestar.exe left behind after uninstall'
  Assert ($null -eq (Get-CimInstance Win32_Service -Filter "Name='Lodestar'")) 'service left behind after uninstall'
  Assert ($null -eq (Get-NetFirewallRule -DisplayName 'Lodestar*' -ErrorAction SilentlyContinue)) 'firewall rules left behind'
  Assert (Test-Path "$dataDir\keep-me.txt") 'uninstall deleted the data folder; it must keep it'

  # --- a non-empty folder Lodestar did not secure must stop the install ---
  Rename-Item "$dataDir\.lodestar-secured" 'marker.off'
  $p = Start-Process msiexec.exe -ArgumentList @('/i', "`"$Msi`"", '/qn', '/norestart', '/l*v', "`"$logs\msi-refused.log`"") -Wait -PassThru
  Assert ($p.ExitCode -ne 0) 'install went ahead on a non-empty folder that was never secured'
  Assert (-not (Test-Path "$installDir\lodestar.exe")) 'refused install left files behind'
  Rename-Item "$dataDir\marker.off" '.lodestar-secured'

  # --- the marker alone is not trusted: inherited ACL, or a foreign-owned entry, must stop it ---
  $icacls = Join-Path $env:SystemRoot 'System32\icacls.exe'
  & $icacls $dataDir /inheritance:e | Out-Null
  Assert ($LASTEXITCODE -eq 0) 'could not re-enable inheritance for the test'
  $p = Start-Process msiexec.exe -ArgumentList @('/i', "`"$Msi`"", '/qn', '/norestart', '/l*v', "`"$logs\msi-refused2.log`"") -Wait -PassThru
  Assert ($p.ExitCode -ne 0) 'install went ahead on a folder with inherited permissions'
  & $icacls $dataDir /inheritance:r | Out-Null
  & $icacls "$dataDir\keep-me.txt" /setowner '*S-1-5-32-545' | Out-Null
  Assert ($LASTEXITCODE -eq 0) 'could not set a foreign owner for the test'
  $p = Start-Process msiexec.exe -ArgumentList @('/i', "`"$Msi`"", '/qn', '/norestart', '/l*v', "`"$logs\msi-refused3.log`"") -Wait -PassThru
  Assert ($p.ExitCode -ne 0) 'install went ahead with a file owned by Users'
  & $icacls "$dataDir\keep-me.txt" /setowner '*S-1-5-32-544' | Out-Null

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
