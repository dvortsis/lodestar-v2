<#
.SYNOPSIS
  Smoke-test the Windows packages on a clean runner (needs an elevated shell).

  -Msi : install silently, check files, service, firewall rules and the Start-menu
         shortcut, run the installed binary and check /healthz, uninstall (data
         stays), then install with the Defender box ticked, uninstall with
         REMOVE_DATA=1 (data goes).
  -Zip : unpack the portable zip, run it with a temporary data folder, check /healthz.

  The installed service is started for real: Start-Service, /healthz on 8090, Stop-Service.
#>
param(
  [string]$Msi,
  [string]$UpgradeMsi,   # the same product at a higher version, for the upgrade check
  [string]$Zip,
  [int]$Port = 18090
)
$ErrorActionPreference = 'Stop'
if (-not $Msi -and -not $Zip) { throw 'pass -Msi <file> and/or -Zip <file>' }
# msiexec rejects paths that mix / and \ (error 1619), so use full Windows paths.
if ($Msi) { $Msi = (Resolve-Path $Msi).ProviderPath }
if ($UpgradeMsi) { $UpgradeMsi = (Resolve-Path $UpgradeMsi).ProviderPath }
if ($Zip) { $Zip = (Resolve-Path $Zip).ProviderPath }

function Assert($ok, $msg) { if (-not $ok) { throw "SMOKE FAILED: $msg" } }

# Windows can hold a killed process's files for a moment; the temp folder is only scratch
# space, so retry and then give up with a warning rather than fail the smoke test.
function Remove-Scratch([string]$path) {
  for ($i = 0; $i -lt 10; $i++) {
    try { Remove-Item -LiteralPath $path -Recurse -Force -ErrorAction Stop; return } catch { Start-Sleep -Seconds 1 }
  }
  Write-Host "::warning::could not remove $path"
}

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
  Remove-Scratch $work
  Write-Host "portable zip ok: $Zip"
}

if ($Msi) {
  $installDir = Join-Path $env:ProgramFiles 'Lodestar'
  $dataDir = Join-Path $env:ProgramData 'Lodestar'
  $logs = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
  Assert (-not (Test-Path $installDir)) 'Lodestar is already installed on this machine'

  # --- a different install folder is refused ---
  $other = Start-Process msiexec.exe -ArgumentList @('/i', "`"$Msi`"", '/qn', '/norestart', 'INSTALLFOLDER=C:\LodestarElsewhere\', '/l*v', "`"$logs\msi-refused-folder.log`"") -Wait -PassThru
  Assert ($other.ExitCode -ne 0) 'an install into another folder went ahead'
  Assert (-not (Test-Path 'C:\LodestarElsewhere')) 'an install into another folder left files behind'

  # --- install (Defender box left off) ---
  Invoke-Msiexec @('/i', "`"$Msi`"", '/qn', '/norestart', '/l*v', "`"$logs\msi-install.log`"") 'install'
  Assert (Test-Path "$installDir\lodestar.exe") 'lodestar.exe not in Program Files'
  Assert (Test-Path $dataDir) 'data folder not created in ProgramData'
  $svc = Get-CimInstance Win32_Service -Filter "Name='Lodestar'"
  if ($null -eq $svc) {
    Write-Host '--- sc query Lodestar'; sc.exe query Lodestar
    Write-Host '--- sc qc Lodestar'; sc.exe qc Lodestar
    Write-Host '--- services named like Lodestar'; Get-CimInstance Win32_Service | Where-Object { $_.Name -like '*odestar*' -or $_.PathName -like '*odestar*' } | Format-List Name, PathName, StartName
  }
  Assert ($null -ne $svc) 'service Lodestar is not registered'
  Assert ($svc.PathName -match 'lodestar\.exe') "service points at $($svc.PathName)"
  Assert ($svc.StartName -eq 'NT SERVICE\Lodestar') "service runs as $($svc.StartName)"
  Assert ($null -eq (Get-NetFirewallRule -DisplayName 'Lodestar web UI*' -ErrorAction SilentlyContinue)) 'the web firewall rule was added without being asked'
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

  # --- the service itself: delayed automatic start, restart on failure, then really start it ---
  $svcKey = Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Services\Lodestar'
  Assert ($svcKey.Start -eq 2) "service start type is $($svcKey.Start), expected 2 (automatic)"
  Assert ($svcKey.DelayedAutostart -eq 1) 'service is not set to delayed start'
  Assert ($null -ne $svcKey.FailureActions) 'service has no failure (recovery) actions'
  Assert ($svcKey.PreshutdownTimeout -eq 60000) "pre-shutdown timeout is $($svcKey.PreshutdownTimeout), expected 60000"
  $failureFlag = (sc.exe qfailure Lodestar | Out-String)
  Assert ($failureFlag -match 'RESTART') 'recovery actions are not restart'
  $privs = (sc.exe qprivs Lodestar | Out-String)
  Assert ($privs -match 'SeChangeNotifyPrivilege') 'service lost SeChangeNotifyPrivilege'
  Assert ($privs -notmatch 'SeImpersonatePrivilege|SeCreateGlobalPrivilege') 'service still holds SeImpersonate or SeCreateGlobal'
  # The installer starts the service (Start="install"): it must be up without anyone starting it.
  $up = $false
  for ($i = 0; $i -lt 90 -and -not $up; $i++) {
    try { $up = ((Invoke-WebRequest -UseBasicParsing -TimeoutSec 2 'http://127.0.0.1:8090/healthz').StatusCode -eq 200) } catch { Start-Sleep -Seconds 1 }
  }
  Assert $up 'the installer did not leave the service running (no /healthz on 8090)'
  Stop-Service Lodestar
  # DHT off so the runner's network is left alone; the service reads this at start.
  Set-Content -LiteralPath "$dataDir\config.toml" -Value "[dht]`nenabled = false`n" -Encoding ascii
  Start-Service Lodestar
  $up = $false
  for ($i = 0; $i -lt 90 -and -not $up; $i++) {
    try { $up = ((Invoke-WebRequest -UseBasicParsing -TimeoutSec 2 'http://127.0.0.1:8090/healthz').StatusCode -eq 200) } catch { Start-Sleep -Seconds 1 }
  }
  if (-not $up) {
    Write-Host '--- service log'
    Get-ChildItem "$dataDir\logs" -ErrorAction SilentlyContinue | ForEach-Object { Write-Host $_.FullName; Get-Content $_.FullName -Tail 60 }
    Write-Host '--- sc query'; sc.exe query Lodestar
    Write-Host '--- event log'; Get-WinEvent -FilterHashtable @{ LogName = 'System'; ProviderName = 'Service Control Manager' } -MaxEvents 8 -ErrorAction SilentlyContinue | Format-List TimeCreated, Message
  }
  Assert $up 'the service never answered /healthz on 8090'
  Write-Host "service healthz: $((Invoke-WebRequest -UseBasicParsing 'http://127.0.0.1:8090/healthz').Content)"
  $index = (Invoke-WebRequest -UseBasicParsing 'http://127.0.0.1:8090/').Content
  Assert ($index -notmatch 'UI not built') '/ served the stub page: the web UI was not embedded'
  Assert (Test-Path "$dataDir\.lodestar-secured") 'the service did not write its .lodestar-secured marker'
  Assert (Test-Path "$dataDir\logs\lodestar.log") 'the service did not write logs\lodestar.log'
  Stop-Service Lodestar
  $svc = Get-CimInstance Win32_Service -Filter "Name='Lodestar'"
  Assert ($svc.State -eq 'Stopped') "service is $($svc.State) after Stop-Service"
  Assert ($svc.ExitCode -eq 0) "service stopped with exit code $($svc.ExitCode)"

  # --- upgrade: the next version installed over this one keeps the service, data and ACL ---
  if ($UpgradeMsi) {
    # Start from an install that chose the web rule, so the upgrade (without the option) must keep it.
    Invoke-Msiexec @('/x', "`"$Msi`"", '/qn', '/norestart', '/l*v', "`"$logs\msi-upgrade-prep-uninstall.log`"") 'uninstall before the upgrade check'
    Invoke-Msiexec @('/i', "`"$Msi`"", '/qn', '/norestart', 'WEB_FIREWALL=1', '/l*v', "`"$logs\msi-upgrade-prep.log`"") 'install with the web rule'
    Assert ($null -ne (Get-NetFirewallRule -DisplayName 'Lodestar web UI*' -ErrorAction SilentlyContinue)) 'the web rule is missing right after installing with WEB_FIREWALL=1'
    Set-Content -Path "$dataDir\upgrade-check.txt" -Value 'kept across the upgrade'
    Invoke-Msiexec @('/i', "`"$UpgradeMsi`"", '/qn', '/norestart', '/l*v', "`"$logs\msi-upgrade.log`"") 'upgrade'
    $svc = Get-CimInstance Win32_Service -Filter "Name='Lodestar'"
    Assert ($null -ne $svc) 'the upgrade left no Lodestar service'
    Assert ($svc.StartName -eq 'NT SERVICE\Lodestar') "after the upgrade the service runs as $($svc.StartName)"
    Assert (Test-Path "$installDir\lodestar.exe") 'the upgrade left no lodestar.exe'
    Assert (Test-Path "$dataDir\upgrade-check.txt") 'the upgrade lost data'
    Assert ($null -ne (Get-NetFirewallRule -DisplayName 'Lodestar web UI*' -ErrorAction SilentlyContinue)) 'the upgrade dropped the web firewall rule chosen before'
    $acl = Get-Acl $dataDir
    Assert $acl.AreAccessRulesProtected 'after the upgrade the data folder inherits permissions'
    $broad = @($acl.Access | Where-Object { $_.IdentityReference.Value -match '(Users|Everyone|Authenticated Users)$' })
    Assert ($broad.Count -eq 0) "after the upgrade the data folder grants access to $($broad.IdentityReference -join ', ')"
    Assert ((Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Services\Lodestar').DelayedAutostart -eq 1) 'after the upgrade the service is not delayed-start'
    # The upgrade starts the service itself.
    $up = $false
    for ($i = 0; $i -lt 90 -and -not $up; $i++) {
      try { $up = ((Invoke-WebRequest -UseBasicParsing -TimeoutSec 2 'http://127.0.0.1:8090/healthz').StatusCode -eq 200) } catch { Start-Sleep -Seconds 1 }
    }
    Assert $up 'after the upgrade the service was not running (no /healthz on 8090)'
    Stop-Service Lodestar
    Remove-Item "$dataDir\upgrade-check.txt"
    # An explicit WEB_FIREWALL=0 on an upgrade beats the remembered choice.
    Invoke-Msiexec @('/x', "`"$UpgradeMsi`"", '/qn', '/norestart', '/l*v', "`"$logs\msi-untick-uninstall.log`"") 'uninstall before the untick check'
    Invoke-Msiexec @('/i', "`"$Msi`"", '/qn', '/norestart', 'WEB_FIREWALL=1', '/l*v', "`"$logs\msi-untick-install.log`"") 'install with the web rule (untick check)'
    Assert ($null -ne (Get-NetFirewallRule -DisplayName 'Lodestar web UI*' -ErrorAction SilentlyContinue)) 'the web rule is missing before the untick check'
    Invoke-Msiexec @('/i', "`"$UpgradeMsi`"", '/qn', '/norestart', 'WEB_FIREWALL=0', '/l*v', "`"$logs\msi-untick-upgrade.log`"") 'upgrade with WEB_FIREWALL=0'
    Assert ($null -eq (Get-NetFirewallRule -DisplayName 'Lodestar web UI*' -ErrorAction SilentlyContinue)) 'the remembered web rule survived an upgrade with WEB_FIREWALL=0'
    # Back to the first version for the rest of the checks.
    Invoke-Msiexec @('/x', "`"$UpgradeMsi`"", '/qn', '/norestart', '/l*v', "`"$logs\msi-upgrade-uninstall.log`"") 'uninstall of the upgraded product'
    Invoke-Msiexec @('/i', "`"$Msi`"", '/qn', '/norestart', '/l*v', "`"$logs\msi-install-again.log`"") 'install again after the upgrade check'
    Write-Host 'upgrade ok'
  }

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

  # --- an empty folder that inherits ProgramData's permissions is replaced, not trusted ---
  Get-ChildItem -LiteralPath $dataDir -Force | Remove-Item -Recurse -Force
  & $icacls $dataDir /inheritance:e | Out-Null
  Assert ($LASTEXITCODE -eq 0) 'could not re-enable inheritance for the test'
  Assert (-not (Get-Acl $dataDir).AreAccessRulesProtected) 'test setup: the folder should inherit permissions now'

  # --- reinstall with the Defender box ticked, uninstall with REMOVE_DATA=1 ---
  Invoke-Msiexec @('/i', "`"$Msi`"", '/qn', '/norestart', 'DEFENDER_EXCLUSION=1', 'WEB_FIREWALL=1', '/l*v', "`"$logs\msi-install2.log`"") 'reinstall'
  $web = Get-NetFirewallRule -DisplayName 'Lodestar web UI*' -ErrorAction SilentlyContinue
  Assert ($null -ne $web) 'web firewall rule missing when asked for'
  Assert ($web.Profile -eq 'Private') "web firewall rule profile is $($web.Profile), expected Private only"
  $acl = Get-Acl $dataDir
  Assert $acl.AreAccessRulesProtected 'the empty inherited folder was not replaced by a private one'
  $broad = @($acl.Access | Where-Object { $_.IdentityReference.Value -match '(Users|Everyone|Authenticated Users)$' })
  Assert ($broad.Count -eq 0) "the replaced folder grants access to $($broad.IdentityReference -join ', ')"
  if (Get-Command Get-MpPreference -ErrorAction SilentlyContinue) {
    try {
      $ex = @((Get-MpPreference).ExclusionPath)
      if ($ex -like "$dataDir*") { Write-Host 'defender exclusion added when asked: ok' }
      else { Write-Host '::warning::Defender exclusion not visible (Defender may be off or managed on this runner)' }
    } catch { Write-Host "::warning::could not read Defender settings: $_" }
  }
  Invoke-Msiexec @('/x', "`"$Msi`"", '/qn', '/norestart', 'REMOVE_DATA=1', '/l*v', "`"$logs\msi-uninstall2.log`"") 'uninstall with REMOVE_DATA'
  $left = Test-Path $dataDir
  if ($left) {
    # Show why (the diagnostics must not decide the result): run the repository copy of the script here.
    Write-Host '--- data folder contents'
    Get-ChildItem -LiteralPath $dataDir -Force -Recurse -ErrorAction SilentlyContinue | Select-Object -First 40 | ForEach-Object { Write-Host ("{0} {1}" -f $_.Attributes, $_.FullName) }
    Write-Host '--- remove-datadir.ps1 run by hand'
    & (Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe') -NoProfile -NonInteractive -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'remove-datadir.ps1') -Path "$dataDir\."
    Write-Host "exit code: $LASTEXITCODE"
  }
  Assert (-not $left) 'REMOVE_DATA=1 left the data folder behind'
  if (Get-Command Get-MpPreference -ErrorAction SilentlyContinue) {
    try { Assert (-not (@((Get-MpPreference).ExclusionPath) -like "$dataDir*")) 'Defender exclusion left behind after uninstall' } catch [System.Management.Automation.RuntimeException] { throw } catch { }
  }
  Write-Host "msi ok: $Msi"
}
