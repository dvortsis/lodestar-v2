<#
.SYNOPSIS
  Joins this PC to an existing Lodestar as a worker, once, during the MSI install. Run as
  SYSTEM by the JoinWorker custom action (join.wxs), after the data folder is secured and
  before the service starts.

  The join token arrives on standard input, never on a command line, and is never printed or
  logged. The script:
    1. reads one line from stdin and checks it looks like a join token (lsj1 + 122 base32
       characters), so a typo fails here with a plain message;
    2. creates a folder in C:\Windows\Temp that only SYSTEM and Administrators can use (set in
       the same call that creates it, as secure-datadir.ps1 does), checks it, creates the token
       file empty inside it, checks the file, and only then writes the token;
    3. runs `lodestar.exe worker --join <address> --token-file <file> --join-only --data <dir>`
       once, with a deadline (-TimeoutSeconds), and stops it if it runs over;
    4. deletes the token file and its folder whatever happened (finally), and checks they are
       gone;
    5. on success checks that the data folder now holds the worker's credentials, writes
       <data>\cluster\joined-by-setup (only when the folder hadn't joined before), so a
       rollback of this install removes this join's files (join-finish.ps1), and hands the
       worker's files to the service's account.

  Output: lines for the MSI log (the token is replaced by [join token] if anything ever printed
  it), and on failure one line starting "LODESTAR JOIN FAILED: " that the custom action shows
  the user. The coordinator's address may appear in it; the token never does.

  Exit status: 0 joined; 1 the join failed or was refused (the message says why); 2 an
  unexpected error in this script.
#>
param(
  [Parameter(Mandatory)][string]$Exe,
  [Parameter(Mandatory)][string]$Coordinator,
  [Parameter(Mandatory)][string]$DataDir,
  # The work folder and the token folder inside it, as the custom action named them
  # (JoinRunner.WorkFolders), so it deletes the same token folder if it has to stop this
  # script. Checked below: lodestar-join-<id> and its own t-<id>.
  [Parameter(Mandatory)][string]$WorkDir,
  [Parameter(Mandatory)][string]$TokenDir,
  [ValidateRange(10, 600)][int]$TimeoutSeconds = 120,
  # Tests only: extra arguments put before `worker` (a stub's mode), and the folder the
  # work folder must be in instead of %SystemRoot%\Temp. The MSI passes neither.
  [string[]]$StubArgs = @(),
  [string]$TestTempRoot = '',
  # Tests only: a service that exists on the test machine (the Lodestar service doesn't yet),
  # whose SID is given the worker's files.
  [ValidatePattern('\A[A-Za-z0-9_]{1,64}\z')][string]$ServiceName = 'Lodestar'
)
$ErrorActionPreference = 'Stop'

$script:token = $null
$script:logWriter = $null

# Printable ASCII only, "::" broken up so no line can pass for a CI workflow command, and the
# token replaced wherever it appears, in any case (the token is case-insensitive).
function Safe($text) {
  $t = [string]$text
  if ($script:token) { $t = [regex]::Replace($t, [regex]::Escape($script:token), '[join token]', 'IgnoreCase') }
  ($t -replace '[^\x20-\x7E]', '?') -replace '::', ': :'
}
function Log($msg) {
  $line = Safe $msg
  Write-Host "lodestar join: $line"
  if ($script:logWriter) {
    $script:logWriter.WriteLine(("{0:s}Z {1}" -f (Get-Date).ToUniversalTime(), $line))
    $script:logWriter.Flush()
  }
}
# Only before the token folder exists (afterwards, failures go through $message and finally).
function Fail([string]$msg, [int]$code = 1) {
  $line = Safe $msg
  if ($script:logWriter) { $script:logWriter.WriteLine(("{0:s}Z FAILED: {1}" -f (Get-Date).ToUniversalTime(), $line)); $script:logWriter.Flush() }
  Write-Host ("LODESTAR JOIN FAILED: " + $line)
  exit $code
}

. (Join-Path $PSScriptRoot 'dacl-check.ps1')
. (Join-Path $PSScriptRoot 'join-token-file.ps1')
. (Join-Path $PSScriptRoot 'service-sid.ps1')

# For a data folder that holds a coordinator's database.
function Get-CoordinatorFolderMessage {
  "$DataDir holds the database of a Lodestar that ran on this PC before (uninstalling keeps it). A worker needs that folder for itself. If you want to keep that Lodestar's library, back up $DataDir first. Then, as an administrator, delete $DataDir and run setup again."
}

# Why $dir isn't the data folder secure-datadir.ps1 made, or $null: it exists, it's a real
# folder (no link), its list is protected, and every entry, read raw, is a plain Allow for
# SYSTEM, Administrators or the service. lodestar --join-only doesn't check the folder's
# permissions itself (WK-18s), so the join relies on SecureDataFolder having run first.
function Get-DataDirProblem([string]$dir, [Security.Principal.SecurityIdentifier]$service) {
  if (-not [IO.Directory]::Exists($dir)) { return 'it does not exist' }
  $item = Get-Item -LiteralPath $dir -Force
  if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { return 'it is a link or junction' }
  $raw = Get-RawDescriptor $dir
  if ((@($script:jtOwners) + $service) -notcontains $raw.Owner) { return "it is owned by $($raw.Owner)" }
  if (-not ($raw.ControlFlags -band [Security.AccessControl.ControlFlags]::DiscretionaryAclProtected)) {
    return 'it inherits permissions from its parent'
  }
  return Get-DaclProblem $raw @($script:jtSystem, $script:jtAdmins, $service)
}

# One argument for a Windows command line. Every value here is a path in Program Files,
# ProgramData or Windows\Temp, or an address checked below: none may hold a quote.
function Quote([string]$a) {
  if ($a -match '"') { throw 'an argument holds a quote' }
  if ($a -match '[\s]' -or $a -eq '') { return '"' + $a.TrimEnd('\') + '"' }
  $a
}

# ---- 1. the token, from stdin ----
# Read as bytes from the pipe itself, never through PowerShell's console input (its encoding
# and buffering differ between hosts), then decoded as ASCII: a token is ASCII. One line; a
# join token is 126 characters, so more than 4 KiB is not a token.
$stdin = [Console]::OpenStandardInput()
$buffer = New-Object byte[] 4097
$got = 0
while ($got -lt $buffer.Length) {
  $n = $stdin.Read($buffer, $got, $buffer.Length - $got)
  if ($n -le 0) { break }
  $got += $n
  if ([Array]::IndexOf($buffer, [byte]10, 0, $got) -ge 0) { break }
}
$text = [Text.Encoding]::ASCII.GetString($buffer, 0, $got)
[Array]::Clear($buffer, 0, $buffer.Length)
if ($got -gt 4096) { $text = $null; Fail 'The join token is too long. Copy it again from Settings, Workers, Add worker on the main Lodestar.' }
# The first line, without the line break, spaces, or a byte-order mark (read as ASCII, its
# three bytes become '???'), which some writers put first.
$first = ($text -split "`n", 2)[0]
$text = $null
$script:token = $first.Trim().TrimStart([char]0xFEFF)
if ($script:token.StartsWith('???')) { $script:token = $script:token.Substring(3) }
$script:token = $script:token.Trim()
$first = $null
if ($script:token -eq '') { $script:token = $null; Fail 'No join token was given. Make one in Settings, Workers, Add worker on the main Lodestar, and paste it in.' }
if ($script:token -notmatch '\A(?i:lsj1)[A-Za-z2-7]{122}\z') {
  # What was wrong, for the log, without any of the token: its length, its start, and the
  # code of the first character that can't be in a token (so isn't part of one).
  $bad = -1
  for ($i = 0; $i -lt $script:token.Length; $i++) {
    if ($script:token[$i] -notmatch '[A-Za-z0-9]') { $bad = $i; break }
  }
  $what = "got $($script:token.Length) characters; starts with lsj1: $($script:token -match '\A(?i:lsj1)')"
  if ($bad -ge 0) { $what += "; character $bad is U+$('{0:X4}' -f [int]$script:token[$bad])" }
  $script:token = $null
  Write-Host "lodestar join: the token was refused ($what)"
  Fail 'That is not a join token: it starts with lsj1 and is 126 letters and digits long. Copy it again from Settings, Workers, Add worker on the main Lodestar (it works once, for 15 minutes).'
}

# The address goes on lodestar's command line, so only https://host[:port], host[:port] or an
# [IPv6] literal; lodestar checks it again.
$addr = $Coordinator.Trim()
# A join token pasted into the address field: refused without repeating it.
if ($addr -match '\A(https://)?(?i:lsj1)') {
  Fail "That looks like the join token, not the main Lodestar's address. The address looks like https://192.168.1.10:8091."
}
if ($addr -notmatch '\A(https://)?([A-Za-z0-9]([A-Za-z0-9.-]{0,252})|\[[0-9A-Fa-f:.]{2,45}\])(:[0-9]{1,5})?/?\z') {
  Fail "The main Lodestar's address should look like https://192.168.1.10:8091."
}

$DataDir = [IO.Path]::GetFullPath($DataDir).TrimEnd('\')

# ---- 2. the private folders ----
$work = [IO.Path]::GetFullPath($WorkDir).TrimEnd('\')
$tokenDir = [IO.Path]::GetFullPath($TokenDir).TrimEnd('\')
$id = (Split-Path -Leaf $work) -replace '\Alodestar-join-', ''
if ((Split-Path -Leaf $work) -notmatch '\Alodestar-join-[0-9a-f]{32}\z' -or
    (Split-Path -Parent $tokenDir) -ne $work -or (Split-Path -Leaf $tokenDir) -ne "t-$id") {
  Fail 'Unexpected work folders for the join. Install again.' 2
}
$root = Split-Path -Parent $work
# Only in %SystemRoot%\Temp (the folder the custom action uses), or a test's own folder.
$expectedRoot = if ($TestTempRoot) { [IO.Path]::GetFullPath($TestTempRoot).TrimEnd('\') } else { Join-Path $env:SystemRoot 'Temp' }
if ($root -ne $expectedRoot) { Fail 'Unexpected work folders for the join. Install again.' 2 }
$tokenFile = Join-Path $tokenDir 'join-token'
$exitCode = 2
$message = $null
try {
  try {
    New-PrivateFolder $work
    # The log stays (it never holds the token), for a failed install to point to.
    $stream = [IO.File]::Open((Join-Path $work 'join-worker.log'), [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    $script:logWriter = New-Object IO.StreamWriter($stream)
  } catch {
    Fail "Could not create a private folder in $root to work from: $($_.Exception.Message)" 2
  }
  Log "joining $addr; data folder $DataDir; log $work"
  # The raw reader compiles in the work folder, which only SYSTEM and Administrators can write.
  Initialize-RawReader $work
  $why = Get-PrivateProblem $work
  if ($why) { Fail "The private folder $work is not private: $why. Install again." 2 }
  # The worker's key and credentials go into the data folder: only once it's secured.
  $why = Get-DataDirProblem $DataDir (Get-ServiceSid $ServiceName)
  if ($why) { Fail "The data folder $DataDir is not secured yet ($why), so nothing was joined. Run the installer again." 2 }
  # A Lodestar that ran here before and was uninstalled keeps its database (the default), and
  # lodestar never makes a worker of a coordinator's folder.
  if ([IO.File]::Exists((Join-Path $DataDir 'lodestar.db'))) { Fail (Get-CoordinatorFolderMessage) }
  $credentials = Join-Path $DataDir 'cluster\credentials'
  $marker = Join-Path $DataDir 'cluster\joined-by-setup'
  $wasJoined = [IO.File]::Exists($credentials)
  # A marker left by a setup that was cut off (neither its commit nor its rollback ran) isn't
  # this setup's: a rollback of this one must never remove that join's files.
  if ([IO.File]::Exists($marker)) { [IO.File]::Delete($marker); Log 'removed a marker an earlier setup left' }

  New-PrivateFolder $tokenDir
  Write-TokenFile $tokenFile $script:token
  Log 'token file written'

  # ---- 3. join, once, with a deadline ----
  $arguments = @($StubArgs) + @('worker', '--join', $addr, '--token-file', $tokenFile, '--join-only', '--data', $DataDir)
  $psi = New-Object Diagnostics.ProcessStartInfo
  $psi.FileName = $Exe
  $psi.Arguments = ($arguments | ForEach-Object { Quote $_ }) -join ' '
  $psi.UseShellExecute = $false
  $psi.CreateNoWindow = $true
  $psi.RedirectStandardInput = $true
  $psi.RedirectStandardOutput = $true
  $psi.RedirectStandardError = $true
  # No LODESTAR_TOKEN or LODESTAR_JOIN from wherever msiexec was started: only the file joins.
  foreach ($k in 'LODESTAR_TOKEN', 'LODESTAR_JOIN', 'LODESTAR_TOKEN_IGNORED', 'LODESTAR_DATA_DIR') { [void]$psi.EnvironmentVariables.Remove($k) }
  $p = [Diagnostics.Process]::Start($psi)
  $p.StandardInput.Close()
  $out = $p.StandardOutput.ReadToEndAsync()
  $err = $p.StandardError.ReadToEndAsync()
  $timedOut = -not $p.WaitForExit($TimeoutSeconds * 1000)
  if ($timedOut) {
    Log "lodestar did not finish within $TimeoutSeconds s: stopping it"
    $taskkill = Join-Path $env:SystemRoot 'System32\taskkill.exe'
    # Its error output (the process already gone) must not stop this script.
    $ErrorActionPreference = 'Continue'
    & $taskkill /PID $p.Id /T /F 2>&1 | Out-Null
    $ErrorActionPreference = 'Stop'
    if (-not $p.WaitForExit(10000)) { try { $p.Kill() } catch { } ; [void]$p.WaitForExit(10000) }
  } else {
    $p.WaitForExit()   # the parameterless wait also waits for the output to be read
  }
  [void]$out.Wait(10000); [void]$err.Wait(10000)
  $text = ''
  if ($out.IsCompleted) { $text += $out.Result }
  if ($err.IsCompleted) { $text += $err.Result }
  $lines = @($text -split "`r?`n" | Where-Object { $_.Trim() -ne '' })
  foreach ($l in $lines) { Log "lodestar: $l" }

  if ($timedOut) {
    $message = "Joining $addr took longer than $TimeoutSeconds seconds. Check that this PC can reach the main Lodestar's cluster port (8091 unless you changed it), then make a new join token and install again."
    $exitCode = 1
  } elseif ($p.ExitCode -ne 0 -and ($lines -match "coordinator's data folder")) {
    $message = Get-CoordinatorFolderMessage
    $exitCode = 1
  } elseif ($p.ExitCode -ne 0 -and [IO.File]::Exists($credentials)) {
    # Already joined: most likely an earlier install joined and was then undone, which
    # leaves the worker's files in the data folder (X-wk18w-rollback). Uninstalling can't
    # help, since nothing is installed, so say how to start over. Still a failure.
    $message = "This PC has already joined a Lodestar: an earlier setup joined and was then undone, and left its files in $DataDir. On the main Lodestar, remove this worker in Settings, Workers. Then, as an administrator, delete $DataDir and run setup again with a new join token."
    $exitCode = 1
  } elseif ($p.ExitCode -ne 0) {
    # lodestar's own last line says why (refused, expired, unreachable, wrong CA); it never
    # holds the token, and Safe() would hide it if it did.
    $last = if ($lines.Count) { ($lines[-1] -replace '^lodestar worker: ', '').Trim() } else { "exit status $($p.ExitCode)" }
    $message = "Could not join $addr`: $last"
    $exitCode = 1
  } elseif (-not [IO.File]::Exists($credentials)) {
    $message = "lodestar finished but $DataDir holds no worker credentials. Install again with a new join token."
    $exitCode = 1
  } else {
    # This setup made the join: a rollback of it removes the join's files (join-finish.ps1).
    if (-not $wasJoined) {
      [IO.File]::WriteAllText($marker, "This setup joined. Removed when setup finishes, or with the join's files if setup is undone.`r`n")
      Log 'marked the join as made by this setup'
    }
    # The join ran as SYSTEM, so SYSTEM owns the key and the credentials, and their lists
    # allow only OWNER RIGHTS, SYSTEM and Administrators (integrations secret.rs): the
    # service's own account gets them as their owner. /L: never through a link.
    $svc = (Get-ServiceSid $ServiceName).Value
    $icacls = Join-Path $env:SystemRoot 'System32\icacls.exe'
    $owned = $true
    foreach ($target in @(@((Join-Path $DataDir 'secrets.key'), @()), @((Join-Path $DataDir 'cluster'), @('/T')))) {
      if (-not (Test-Path -LiteralPath $target[0])) { continue }
      $ErrorActionPreference = 'Continue'
      $o = & $icacls $target[0] /setowner "*$svc" @($target[1]) /L /C /Q 2>&1 | Out-String
      $code = $LASTEXITCODE
      $ErrorActionPreference = 'Stop'
      if ($code -ne 0) { Log "icacls /setowner $($target[0]) failed ($code): $o"; $owned = $false }
    }
    if ($owned) {
      Log "joined $addr"
      $exitCode = 0
    } else {
      $message = "Joined $addr, but the worker's files in $DataDir could not be handed to the Lodestar service, so setup is undone and removes them. On the main Lodestar, remove this worker in Settings, Workers. Then run setup again with a new join token."
      $exitCode = 1
    }
  }
} catch {
  $message = "Unexpected error: $($_.Exception.Message)"
  $exitCode = 2
} finally {
  # ---- 4. the token file goes, whatever happened ----
  if ([IO.Directory]::Exists($work)) {
    if (Remove-TokenFolder $tokenDir { param($m) Log $m }) {
      Log 'token file removed'
    } else {
      Log "the token file is still there: $tokenFile"
      $message = "The temporary token file $tokenFile could not be deleted. Delete its folder as an administrator. A join token works once, and for 15 minutes at most."
      $exitCode = 1
    }
  }
  # Scrubbed while the token is still known (Fail scrubs again, but by then it can't).
  if ($message) { $message = Safe $message }
  $script:token = $null
}
if ($exitCode -ne 0) { Fail $message $exitCode }
exit 0
