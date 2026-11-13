<#
.SYNOPSIS
  Checks the "Join an existing Lodestar" custom action end to end, with a stub lodestar.exe:
  join-ca\JoinRunner.cs (the custom action's core) starts join-worker.ps1, which writes the
  token file and runs the stub. Checks that:
    - the token file's folder is private (SYSTEM and Administrators, protected) before the
      token is written, and a folder that isn't is refused with nothing written;
    - the token file and its folder are deleted after a join that succeeds, fails or runs over
      its deadline, and a folder that can't be deleted is reported;
    - a sentinel token never appears in the output, the MSI-log lines or join-worker.log, even
      when lodestar echoes it, and never on lodestar's command line or in its environment;
    - lodestar is run with --join-only, and a bad token or address stops before anything runs.
  Needs an elevated Windows PowerShell. Run in CI before the MSI is built:
    powershell -File packaging/windows/test-join-worker.ps1
#>
$ErrorActionPreference = 'Stop'

function Assert($ok, $msg) { if (-not $ok) { throw "JOIN TEST FAILED: $msg" } }

# A token-shaped sentinel (lsj1 + 122 base32 characters). Mixed case: the token is
# case-insensitive, so the scrub must be too.
$sentinel = 'lsj1' + ('sentinel2345abcdefghijk' * 6).Substring(0, 122)
Assert ($sentinel.Length -eq 126) 'the sentinel is not token-shaped'
$env:STUB_EXPECT = $sentinel
# A second, different token-shaped value, for the address field: the pasted-token cases check
# that neither reaches any output, so an address scrubbed as "[join token]" instead of left
# out would still be caught.
$sentinel2 = 'lsj1' + ('pastedaddress67qrstuvwxyz' * 6).Substring(0, 122)
Assert ($sentinel2.Length -eq 126 -and $sentinel2 -ne $sentinel) 'the second sentinel is not token-shaped'

$root = Join-Path ([IO.Path]::GetTempPath()) ('lodestar-jw-' + [guid]::NewGuid().ToString('N'))
$install = Join-Path $root 'install'
$tmp = Join-Path $root 'tmp'
New-Item -ItemType Directory -Path $install, $tmp | Out-Null
foreach ($f in 'join-worker.ps1', 'join-token-file.ps1', 'join-finish.ps1', 'dacl-check.ps1', 'service-sid.ps1') {
  Copy-Item -LiteralPath (Join-Path $PSScriptRoot $f) -Destination $install
}

# The stub: `lodestar.exe <mode> worker --join A --token-file F --join-only --data D`. It
# records what it saw in D\stub-seen.txt, then acts out <mode>.
Add-Type -OutputType ConsoleApplication -OutputAssembly (Join-Path $install 'lodestar.exe') -TypeDefinition @'
using System;
using System.IO;
using System.Security.AccessControl;
using System.Text;
using System.Threading;
public static class Stub {
  public static int Main(string[] a) {
    string mode = a[0], file = null, data = null, join = null;
    bool joinOnly = false;
    for (int i = 1; i < a.Length; i++) {
      if (a[i] == "--token-file") file = a[++i];
      else if (a[i] == "--data") data = a[++i];
      else if (a[i] == "--join") join = a[++i];
      else if (a[i] == "--join-only") joinOnly = true;
    }
    Directory.CreateDirectory(data);
    string tok = File.ReadAllText(file).Trim();
    AccessControlSections s = AccessControlSections.Owner | AccessControlSections.Access;
    StringBuilder sb = new StringBuilder();
    sb.AppendLine("pid=" + System.Diagnostics.Process.GetCurrentProcess().Id);
    sb.AppendLine("file=" + file);
    sb.AppendLine("join=" + join);
    sb.AppendLine("joinonly=" + joinOnly);
    sb.AppendLine("filesddl=" + File.GetAccessControl(file).GetSecurityDescriptorSddlForm(s));
    sb.AppendLine("dirsddl=" + Directory.GetAccessControl(Path.GetDirectoryName(file)).GetSecurityDescriptorSddlForm(s));
    sb.AppendLine("token=" + (tok == Environment.GetEnvironmentVariable("STUB_EXPECT") ? "match" : "differ"));
    sb.AppendLine("envtoken=" + (Environment.GetEnvironmentVariable("LODESTAR_TOKEN") == null ? "unset" : "set"));
    sb.AppendLine("onargv=" + (Environment.CommandLine.IndexOf(tok, StringComparison.OrdinalIgnoreCase) >= 0));
    File.WriteAllText(Path.Combine(data, "stub-seen.txt"), sb.ToString());
    switch (mode) {
      case "ok":
        Directory.CreateDirectory(Path.Combine(data, "cluster"));
        File.WriteAllText(Path.Combine(data, "cluster", "credentials"), "stub");
        File.WriteAllText(Path.Combine(data, "cluster", "coordinator"), "stub");
        File.WriteAllText(Path.Combine(data, "secrets.key"), "stub");
        Console.Error.WriteLine("lodestar worker: joined " + join + " as worker stub-1");
        return 0;
      case "fail":
        Console.Error.WriteLine("lodestar worker: the coordinator refused the join token: it was already used or has expired");
        return 1;
      case "leak":
        Console.WriteLine("the token is " + tok);
        Console.Error.WriteLine("lodestar worker: bad token " + tok.ToUpperInvariant());
        return 1;
      case "hang":
        Thread.Sleep(Timeout.Infinite);
        return 0;
      default:  // "nocreds": says it worked, saves nothing
        return 0;
    }
  }
}
'@

Add-Type -TypeDefinition (Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'join-ca\JoinRunner.cs'))

# The helpers: data folders are made private first, as secure-datadir.ps1 makes the real one.
. (Join-Path $PSScriptRoot 'dacl-check.ps1')
. (Join-Path $PSScriptRoot 'join-token-file.ps1')
. (Join-Path $PSScriptRoot 'service-sid.ps1')
Initialize-RawReader $tmp

$case = 0
# Runs one join through JoinRunner and returns what came back, what it logged, what the stub
# saw and the folders the script made.
# -runnerMs is the custom action's own deadline (JoinRunner), -timeout the script's.
# -service: EventLog by default, a service every Windows has, so icacls can name its SID.
# -Joined: the data folder already holds credentials, as after a join that was undone.
function Invoke-Join([string]$mode, [string]$token = $sentinel, [string]$address = 'https://192.168.1.10:8091', [int]$timeout = 30, [switch]$OpenData, [int]$runnerMs = 120000, [string]$service = 'EventLog', [switch]$Joined, [switch]$WithDb) {
  $script:case++
  $data = Join-Path $root "data-$script:case"
  if ($OpenData) { New-Item -ItemType Directory -Path $data | Out-Null } else { New-PrivateFolder $data }
  # A coordinator's database, left by a Lodestar that was uninstalled with its data kept.
  if ($WithDb) { [IO.File]::WriteAllText((Join-Path $data 'lodestar.db'), 'db') }
  if ($Joined) {
    New-Item -ItemType Directory -Path (Join-Path $data 'cluster') | Out-Null
    [IO.File]::WriteAllText((Join-Path $data 'cluster\credentials'), 'earlier')
  }
  $log = New-Object 'Collections.Generic.List[string]'
  $extra = "-StubArgs $mode -TimeoutSeconds $timeout -ServiceName $service"
  $before = @(Get-ChildItem -LiteralPath $tmp -Directory | ForEach-Object FullName)
  $result = [Lodestar.Setup.JoinRunner]::Run($install, $data, $address, $token,
    [Action[string]] ({ param($l) $log.Add($l) }.GetNewClosure()), $tmp, $extra, $runnerMs)
  $work = @(Get-ChildItem -LiteralPath $tmp -Directory | Where-Object { $before -notcontains $_.FullName })
  $seen = @{}
  $seenFile = Join-Path $data 'stub-seen.txt'
  if (Test-Path -LiteralPath $seenFile) {
    foreach ($l in Get-Content -LiteralPath $seenFile) { $k, $v = $l -split '=', 2; $seen[$k] = $v }
  }
  $logText = ($work | ForEach-Object { Get-ChildItem -LiteralPath $_.FullName -Filter *.log -Recurse | Get-Content -Raw }) -join "`n"
  [pscustomobject]@{ Result = $result; Log = $log; Seen = $seen; Work = $work; LogFile = $logText; Data = $data }
}

# The sentinel, in any case, nowhere the user, the MSI log or join-worker.log can see.
function Assert-NoToken($r, [string]$what) {
  $all = @($r.Result) + @($r.Log) + @($r.LogFile) -join "`n"
  Assert ($all.IndexOf($sentinel, [StringComparison]::OrdinalIgnoreCase) -lt 0) "$what`: the token appeared in the output or log"
}

# Nothing left but the work folder's log and the raw reader: the token folder is gone.
function Assert-TokenGone($r, [string]$what) {
  if ($r.Seen.file) {
    Assert (-not (Test-Path -LiteralPath $r.Seen.file)) "$what`: the token file is still there"
    Assert (-not (Test-Path -LiteralPath (Split-Path -Parent $r.Seen.file))) "$what`: the token folder is still there"
  }
  foreach ($w in $r.Work) {
    $left = @(Get-ChildItem -LiteralPath $w.FullName -Directory -Filter 't-*')
    Assert ($left.Count -eq 0) "$what`: a token folder was left in $($w.FullName)"
  }
}

# Every entry for SYSTEM (SY) or Administrators (BA), owner one of those or this account.
function Assert-PrivateSddl([string]$sddl, [string]$what) {
  Assert ($sddl) "$what`: no descriptor recorded"
  $me = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
  $sd = New-Object Security.AccessControl.RawSecurityDescriptor $sddl
  Assert (@('S-1-5-18', 'S-1-5-32-544', $me) -contains $sd.Owner.Value) "$what`: owned by $($sd.Owner)"
  foreach ($ace in $sd.DiscretionaryAcl) {
    Assert ($ace.AceType -eq 'AccessAllowed') "$what`: an entry of type $($ace.AceType)"
    Assert (@('S-1-5-18', 'S-1-5-32-544') -contains $ace.SecurityIdentifier.Value) "$what`: lets $($ace.SecurityIdentifier) in"
  }
}

# ---- a join that works ----
$r = Invoke-Join 'ok'
Assert ($null -eq $r.Result) "ok: the join was reported as failed: $($r.Result)`n$(@($r.Log) -join "`n")"
Assert ($r.Seen.token -eq 'match') 'ok: lodestar did not read the token from the file'
Assert ($r.Seen.joinonly -eq 'True') 'ok: lodestar was not run with --join-only'
Assert ($r.Seen.join -eq 'https://192.168.1.10:8091') "ok: lodestar got the address $($r.Seen.join)"
Assert ($r.Seen.onargv -eq 'False') "ok: the token was on lodestar's command line"
Assert ($r.Seen.envtoken -eq 'unset') "ok: LODESTAR_TOKEN was set in lodestar's environment"
Assert-PrivateSddl $r.Seen.filesddl 'ok: the token file'
Assert-PrivateSddl $r.Seen.dirsddl 'ok: the token folder'
Assert ($r.Seen.dirsddl -match 'D:P') 'ok: the token folder inherits permissions'
$creds = Join-Path $r.Data 'cluster\credentials'
Assert (Test-Path -LiteralPath $creds) 'ok: no credentials'
$credsOwner = (Get-Acl -LiteralPath $creds).GetOwner([Security.Principal.SecurityIdentifier]).Value
Assert ($credsOwner -eq (Get-ServiceSid 'EventLog').Value) "ok: the credentials are owned by $credsOwner, not the service"
$keyOwner = (Get-Acl -LiteralPath (Join-Path $r.Data 'secrets.key')).GetOwner([Security.Principal.SecurityIdentifier]).Value
Assert ($keyOwner -eq (Get-ServiceSid 'EventLog').Value) "ok: secrets.key is owned by $keyOwner, not the service"
Assert-TokenGone $r 'ok'
Assert-NoToken $r 'ok'
Assert ($r.Work.Count -eq 1) "ok: expected one work folder, got $($r.Work.Count)"

# ---- the coordinator refuses ----
$r = Invoke-Join 'fail'
Assert ($r.Result -like "$([Lodestar.Setup.JoinRunner]::FailedPrefix)*refused*") "fail: unexpected result: $($r.Result)"
Assert ($r.Result -like '*192.168.1.10:8091*') 'fail: the message does not name the address'
Assert-TokenGone $r 'fail'
Assert-NoToken $r 'fail'

# ---- lodestar echoes the token (it never does; the scrub is the backstop) ----
$r = Invoke-Join 'leak'
Assert ($r.Result) 'leak: the join was reported as working'
Assert ((@($r.Log) -join "`n") -match '\[join token\]') 'leak: the echoed token was not replaced'
Assert-TokenGone $r 'leak'
Assert-NoToken $r 'leak'

# ---- lodestar runs over its deadline ----
$r = Invoke-Join 'hang' -timeout 10
Assert ($r.Result -like '*longer than 10 seconds*') "hang: unexpected result: $($r.Result)"
Assert ($r.Seen.pid) 'hang: the stub did not start'
Assert ($null -eq (Get-Process -Id ([int]$r.Seen.pid) -ErrorAction SilentlyContinue)) 'hang: lodestar is still running'
Assert-TokenGone $r 'hang'
Assert-NoToken $r 'hang'

# ---- exits 0 but saves nothing ----
$r = Invoke-Join 'nocreds'
Assert ($r.Result -like '*holds no worker credentials*') "nocreds: unexpected result: $($r.Result)"
Assert-TokenGone $r 'nocreds'

# ---- a mistyped token: refused before any file or process ----
$typo = $sentinel.Substring(0, 125)
$r = Invoke-Join 'ok' -token $typo
Assert ($r.Result -like '*not a join token*') "typo: unexpected result: $($r.Result)"
Assert ($r.Seen.Count -eq 0) 'typo: lodestar ran'
Assert ($r.Work.Count -eq 0) 'typo: a work folder was made'
$all = @($r.Result) + @($r.Log) -join "`n"
Assert ($all.IndexOf($typo, [StringComparison]::OrdinalIgnoreCase) -lt 0) 'typo: the mistyped token appeared in the output'

# ---- an address that could add parameters: refused before PowerShell starts ----
$r = Invoke-Join 'ok' -address 'https://c:8091" -StubArgs x'
Assert ($r.Result -like '*should look like*') "quote: unexpected result: $($r.Result)"
Assert ($r.Log.Count -eq 0) 'quote: join-worker.ps1 ran'
# This one passes the custom action's character check; join-worker.ps1 refuses the path.
$r = Invoke-Join 'ok' -address 'https://c:8091/path'
Assert ($r.Result -like '*should look like*') "path: unexpected result: $($r.Result)"
Assert ($r.Seen.Count -eq 0) 'path: lodestar ran'

# ---- the custom action's own deadline: it stops the script and deletes the token folder ----
# The script's deadline (90 s) is well past the action's (30 s), so the action's kill path runs
# and the script's finally never does, even on a slow runner.
$r = Invoke-Join 'hang' -timeout 90 -runnerMs 30000
Assert ($r.Result -like '*did not finish in time*') "runner deadline: unexpected result: $($r.Result)"
Assert ($r.Seen.pid) 'runner deadline: the stub did not start'
Assert ($null -eq (Get-Process -Id ([int]$r.Seen.pid) -ErrorAction SilentlyContinue)) 'runner deadline: lodestar is still running'
Assert-TokenGone $r 'runner deadline'
Assert-NoToken $r 'runner deadline'

# ---- already joined (an earlier setup joined and was undone): how to start over ----
$r = Invoke-Join 'fail' -Joined
Assert ($r.Result -like '*already joined a Lodestar*remove this worker*') "joined: unexpected result: $($r.Result)"
Assert-TokenGone $r 'joined'

# ---- the files can't be handed to the service: the install fails ----
$r = Invoke-Join 'ok' -service 'NoSuchLodestarTestService'
Assert ($r.Result -like '*could not be handed to the Lodestar service*remove this worker*run setup again*') "hand-off: unexpected result: $($r.Result)"
# Marked as this setup's join before the hand-off, so the rollback removes its files.
Assert (Test-Path -LiteralPath (Join-Path $r.Data 'cluster\joined-by-setup')) 'hand-off: the join was not marked for the rollback' 
Assert-TokenGone $r 'hand-off'

# ---- the token pasted into the address field: refused, and never repeated ----
# Neither value may appear anywhere, not even scrubbed (Assert-NoToken checks the token).
function Assert-NoAddressToken($r, [string]$what) {
  $all = @($r.Result) + @($r.Log) + @($r.LogFile) -join "`n"
  Assert ($all.IndexOf($sentinel2, [StringComparison]::OrdinalIgnoreCase) -lt 0) "$what`: the pasted token appeared"
  Assert ($all -notmatch '\[join token\]') "$what`: the pasted token was scrubbed instead of left out"
}
foreach ($pasted in $sentinel2, "https://$sentinel2") {
  $r = Invoke-Join 'ok' -address $pasted
  Assert ($r.Result -like '*looks like the join token*') "token as address ($($pasted.Length)): unexpected result: $($r.Result)"
  Assert ($r.Log.Count -eq 0) 'token as address: join-worker.ps1 ran'
  Assert-NoToken $r 'token as address'
  Assert-NoAddressToken $r 'token as address'
}

# ---- the script's own check, without the custom action in front of it ----
# Runs join-worker.ps1 directly, the token on stdin, and returns its exit code and output.
function Invoke-Script([string]$coordinator) {
  $script:case++
  $data = Join-Path $root "data-$script:case"
  New-PrivateFolder $data
  $w = $null; $t = $null
  [Lodestar.Setup.JoinRunner]::WorkFolders($tmp, [guid]::NewGuid().ToString('N'), [ref]$w, [ref]$t)
  $psi = New-Object Diagnostics.ProcessStartInfo (Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe')
  $psi.Arguments = "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$install\join-worker.ps1`" -Exe `"$install\lodestar.exe`" -Coordinator `"$coordinator`" -DataDir `"$data`" -WorkDir `"$w`" -TokenDir `"$t`" -TestTempRoot `"$tmp`" -StubArgs ok -ServiceName EventLog"
  $psi.UseShellExecute = $false
  $psi.RedirectStandardInput = $true
  $psi.RedirectStandardOutput = $true
  $psi.RedirectStandardError = $true
  $p = [Diagnostics.Process]::Start($psi)
  $bytes = [Text.Encoding]::ASCII.GetBytes($sentinel + "`n")
  $p.StandardInput.BaseStream.Write($bytes, 0, $bytes.Length)
  $p.StandardInput.Close()
  $out = $p.StandardOutput.ReadToEndAsync(); $err = $p.StandardError.ReadToEndAsync()
  Assert ($p.WaitForExit(60000)) 'the script did not finish within 60 s'
  [void]$out.Wait(10000); [void]$err.Wait(10000)
  [pscustomobject]@{ Code = $p.ExitCode; Text = $out.Result + $err.Result; Data = $data }
}
$s = Invoke-Script "https://$sentinel2"
Assert ($s.Code -ne 0 -and $s.Text -like '*looks like the join token*') "script's own check: exit $($s.Code): $($s.Text)"
Assert ($s.Text.IndexOf($sentinel2, [StringComparison]::OrdinalIgnoreCase) -lt 0) "script's own check: the pasted token appeared"
Assert ($s.Text.IndexOf($sentinel, [StringComparison]::OrdinalIgnoreCase) -lt 0) "script's own check: the token appeared"
Assert (-not (Test-Path -LiteralPath (Join-Path $s.Data 'stub-seen.txt'))) "script's own check: lodestar ran"

# ---- a coordinator's folder left by an uninstall: back it up or delete it ----
$r = Invoke-Join 'ok' -WithDb
Assert ($r.Result -like '*holds the database of a Lodestar*back up*delete*run setup again*') "coordinator folder: unexpected result: $($r.Result)"
Assert ($r.Result -notlike '*--data*' -and $r.Result -notlike '*REMOVE_DATA*') "coordinator folder: advice an installer user can't follow: $($r.Result)"
Assert ($r.Seen.Count -eq 0) 'coordinator folder: lodestar ran'
Assert-TokenGone $r 'coordinator folder'

# ---- the rollback and commit steps (join-finish.ps1) ----
function Invoke-Finish([string]$step, [string]$data) {
  $out = & (Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe') -NoProfile -NonInteractive -ExecutionPolicy Bypass -File (Join-Path $install 'join-finish.ps1') -Step $step -DataDir $data 2>&1 | Out-String
  Assert ($LASTEXITCODE -eq 0) "join-finish $step exited $LASTEXITCODE`: $out"
  $out
}
# A join this setup made, then undone: its files go, the key stays, so setup can join again.
$r = Invoke-Join 'ok'
Assert ($null -eq $r.Result) "rollback: the join failed: $($r.Result)"
$cluster = Join-Path $r.Data 'cluster'
Assert (Test-Path -LiteralPath (Join-Path $cluster 'joined-by-setup')) 'rollback: the join was not marked'
[void](Invoke-Finish 'rollback' $r.Data)
foreach ($gone in 'credentials', 'coordinator', 'joined-by-setup') {
  Assert (-not (Test-Path -LiteralPath (Join-Path $cluster $gone))) "rollback: $gone is still there"
}
Assert (Test-Path -LiteralPath (Join-Path $r.Data 'secrets.key')) 'rollback: removed secrets.key'
# A join that finished: the commit removes only the mark, and a later rollback removes nothing.
$r = Invoke-Join 'ok'
$cluster = Join-Path $r.Data 'cluster'
[void](Invoke-Finish 'commit' $r.Data)
Assert (-not (Test-Path -LiteralPath (Join-Path $cluster 'joined-by-setup'))) 'commit: the mark is still there'
[void](Invoke-Finish 'rollback' $r.Data)
Assert (Test-Path -LiteralPath (Join-Path $cluster 'credentials')) 'commit then rollback: the credentials were removed'
# A folder that had already joined is never marked, so its rollback removes nothing.
$r = Invoke-Join 'fail' -Joined
Assert (-not (Test-Path -LiteralPath (Join-Path $r.Data 'cluster\joined-by-setup'))) "already joined: marked as this setup's join"
[void](Invoke-Finish 'rollback' $r.Data)
Assert ([IO.File]::ReadAllText((Join-Path $r.Data 'cluster\credentials')) -eq 'earlier') "already joined: the earlier join's credentials were removed"

# ---- a data folder that isn't secured yet: refused before any token file or process ----
$r = Invoke-Join 'ok' -OpenData
Assert ($r.Result -like '*is not secured yet*') "open data: unexpected result: $($r.Result)"
Assert ($r.Seen.Count -eq 0) 'open data: lodestar ran'
foreach ($w in $r.Work) {
  Assert (@(Get-ChildItem -LiteralPath $w.FullName -Directory -Filter 't-*').Count -eq 0) 'open data: a token folder was made'
}
Assert-NoToken $r 'open data'

# ---- the helpers themselves ----

$fresh = Join-Path $tmp 'fresh'
New-PrivateFolder $fresh
Assert ($null -eq (Get-PrivateProblem $fresh)) "a new private folder is not private: $(Get-PrivateProblem $fresh)"
$threw = $false; try { New-PrivateFolder $fresh } catch { $threw = $true }
Assert $threw 'New-PrivateFolder reused an existing folder'

# A folder that lets Users in: the token is never written, the file stays empty.
$open = Join-Path $tmp 'open'
New-PrivateFolder $open
$acl = Get-Acl -LiteralPath $open
$acl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule(
  (New-Object Security.Principal.SecurityIdentifier 'S-1-5-32-545'), 'Read', 'ContainerInherit, ObjectInherit', 'None', 'Allow')))
Set-Acl -LiteralPath $open -AclObject $acl
$file = Join-Path $open 'join-token'
$threw = $false; try { Write-TokenFile $file $sentinel } catch { $threw = $true; $why = $_.Exception.Message }
Assert $threw 'Write-TokenFile wrote into a folder Users can read'
Assert ($why -match 'not private') "Write-TokenFile refused for another reason: $why"
Assert ((Get-Item -LiteralPath $file).Length -eq 0) 'Write-TokenFile wrote the token before checking the folder'

# A file name that already exists (planted): CreateNew refuses it.
$planted = Join-Path $fresh 'join-token'
[IO.File]::WriteAllText($planted, 'planted')
$threw = $false; try { Write-TokenFile $planted $sentinel } catch { $threw = $true }
Assert $threw 'Write-TokenFile reused an existing file'
Assert ([IO.File]::ReadAllText($planted) -eq 'planted') 'Write-TokenFile wrote into an existing file'

# A junction is never private.
$junction = Join-Path $tmp 'junction'
& (Join-Path $env:SystemRoot 'System32\cmd.exe') /d /c mklink /J "$junction" "$fresh" | Out-Null
Assert ($LASTEXITCODE -eq 0) 'mklink /J failed'
Assert ((Get-PrivateProblem $junction) -match 'link') 'a junction passed as private'
[IO.Directory]::Delete($junction)

# A token folder still held open: reported (after its 10 s deadline), then removed once closed.
$held = Join-Path $tmp 'held'
New-PrivateFolder $held
$heldFile = Join-Path $held 'join-token'
$h = [IO.File]::Open($heldFile, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
try {
  Assert (-not (Remove-TokenFolder $held)) 'Remove-TokenFolder reported a held folder as gone'
  Assert (Test-Path -LiteralPath $heldFile) 'the held file vanished'
} finally { $h.Dispose() }
Assert (Remove-TokenFolder $held) 'Remove-TokenFolder could not remove a released folder'
Assert (-not (Test-Path -LiteralPath $held)) 'the released folder is still there'

Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
Write-Host 'join-worker tests ok'
