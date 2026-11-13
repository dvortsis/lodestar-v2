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
    5. on success checks that the data folder now holds the worker's credentials.

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
  # The custom action's name for the folders, so it can delete the token folder if it has
  # to stop this script.
  [Parameter(Mandatory)][ValidatePattern('\A[0-9a-f]{32}\z')][string]$WorkId,
  [ValidateRange(10, 600)][int]$TimeoutSeconds = 120,
  # Tests only: where the private folders go (default C:\Windows\Temp) and extra arguments
  # put before `worker` (a stub's mode). The MSI passes neither.
  [string]$TempRoot = '',
  [string[]]$StubArgs = @(),
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

# Why $dir isn't the data folder secure-datadir.ps1 made, or $null: it exists, it's a real
# folder (no link), its list is protected, and every entry, read raw, is a plain Allow for
# SYSTEM, Administrators or the service. lodestar --join-only doesn't check the folder's
# permissions itself (WK-18s), so the join relies on SecureDataFolder having run first.
function Get-DataDirProblem([string]$dir, [Security.Principal.SecurityIdentifier]$service) {
  if (-not [IO.Directory]::Exists($dir)) { return 'it does not exist' }
  $item = Get-Item -LiteralPath $dir -Force
  if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { return 'it is a link or junction' }
  $raw = Get-RawDescriptor $dir
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
# One line; a join token is 126 characters. More than 4 KiB is not a token.
$line = [Console]::In.ReadLine()
if ($null -eq $line) { $line = '' }
$script:token = $line.Trim()
$line = $null
if ($script:token.Length -gt 4096) { $script:token = $null; Fail 'The join token is too long. Copy it again from Settings, Workers, Add worker on the main Lodestar.' }
if ($script:token -eq '') { $script:token = $null; Fail 'No join token was given. Make one in Settings, Workers, Add worker on the main Lodestar, and paste it in.' }
if ($script:token -notmatch '\A(?i:lsj1)[A-Za-z2-7]{122}\z') {
  $script:token = $null
  Fail 'That is not a join token: it starts with lsj1 and is 126 letters and digits long. Copy it again from Settings, Workers, Add worker on the main Lodestar (it works once, for 15 minutes).'
}

# The address goes on lodestar's command line, so only https://host[:port], host[:port] or an
# [IPv6] literal; lodestar checks it again.
$addr = $Coordinator.Trim()
if ($addr -notmatch '\A(https://)?([A-Za-z0-9]([A-Za-z0-9.-]{0,252})|\[[0-9A-Fa-f:.]{2,45}\])(:[0-9]{1,5})?/?\z') {
  Fail "The main Lodestar's address should look like https://192.168.1.10:8091 (got '$addr')."
}

$DataDir = [IO.Path]::GetFullPath($DataDir).TrimEnd('\')

# ---- 2. the private folders ----
$root = if ($TempRoot) { $TempRoot } else { Join-Path $env:SystemRoot 'Temp' }
$work = Join-Path $root ('lodestar-join-' + $WorkId)
$tokenDir = Join-Path $work ('t-' + $WorkId)
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
  } elseif ($p.ExitCode -ne 0) {
    # lodestar's own last line says why (refused, expired, unreachable, wrong CA); it never
    # holds the token, and Safe() would hide it if it did.
    $last = if ($lines.Count) { ($lines[-1] -replace '^lodestar worker: ', '').Trim() } else { "exit status $($p.ExitCode)" }
    $message = "Could not join $addr`: $last"
    $exitCode = 1
  } elseif (-not [IO.File]::Exists((Join-Path $DataDir 'cluster\credentials'))) {
    $message = "lodestar finished but $DataDir holds no worker credentials. Install again with a new join token."
    $exitCode = 1
  } else {
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
      $message = "Joined $addr, but the worker's files in $DataDir could not be handed to the Lodestar service. Remove the worker in Settings, Workers on the main Lodestar, then install again with a new join token."
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
