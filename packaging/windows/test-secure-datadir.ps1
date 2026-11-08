<#
.SYNOPSIS
  Checks secure-datadir.ps1's Repair rule (owner decision 2.52) on real folders: it
  re-secures a folder only when the one problem is the installing administrator's own
  Allow entry, and refuses every other case, that entry plus anything else included.
  Needs an elevated Windows PowerShell. Run in CI before the MSI is built:
    powershell -File packaging/windows/test-secure-datadir.ps1
#>
$ErrorActionPreference = 'Stop'

$target = Join-Path $PSScriptRoot 'secure-datadir.ps1'
$ps = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$icacls = Join-Path $env:SystemRoot 'System32\icacls.exe'
. (Join-Path $PSScriptRoot 'service-sid.ps1')
$svc = (Get-ServiceSid 'Lodestar').Value
$me = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
if ($me -notmatch '^S-1-5-21-') { throw "run this as a user account, not $me" }
$other = 'S-1-5-21-1000000001-1000000002-1000000003-4242'
$domainUsers = 'S-1-5-21-1000000001-1000000002-1000000003-513'
$root = Join-Path ([IO.Path]::GetTempPath()) ('lodestar-sdd-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $root | Out-Null

function Icacls([string[]]$a) {
  & $icacls @a | Out-Null
  if ($LASTEXITCODE -ne 0) { throw "icacls $($a -join ' ') failed ($LASTEXITCODE)" }
}

# An entry for any SID, through .NET: icacls refuses a SID that maps to no account (1332),
# such as the service's before it is registered, or the made-up accounts here.
function Add-Entry([string]$path, [string]$sid, [string]$rights = 'FullControl', [string]$type = 'Allow') {
  $acl = Get-Acl -LiteralPath $path
  $inherit = if ((Get-Item -LiteralPath $path -Force).PSIsContainer) { 'ContainerInherit, ObjectInherit' } else { 'None' }
  $acl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule(
    (New-Object Security.Principal.SecurityIdentifier $sid), $rights, $inherit, 'None', $type)))
  Set-Acl -LiteralPath $path -AclObject $acl
}

# A folder an earlier install secured: its marker, a file and a subfolder, all owned by
# Administrators, with the protected ACL secure-datadir.ps1 applies. The ACL goes on first,
# so what is created inside inherits only from it.
function New-Secured([string]$name) {
  $dir = Join-Path $root $name
  New-Item -ItemType Directory -Path $dir | Out-Null
  $sec = New-Object Security.AccessControl.DirectorySecurity
  $sec.SetAccessRuleProtection($true, $false)
  foreach ($sid in 'S-1-5-18', 'S-1-5-32-544', $svc) {
    $sec.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule(
      (New-Object Security.Principal.SecurityIdentifier $sid), 'FullControl', 'ContainerInherit, ObjectInherit', 'None', 'Allow')))
  }
  $sec.SetOwner((New-Object Security.Principal.SecurityIdentifier 'S-1-5-32-544'))
  Set-Acl -LiteralPath $dir -AclObject $sec
  Set-Content -LiteralPath (Join-Path $dir '.lodestar-secured') -Value 'secured'
  New-Item -ItemType Directory -Path (Join-Path $dir 'logs') | Out-Null
  Set-Content -LiteralPath (Join-Path $dir 'logs\lodestar.log') -Value 'data'
  Icacls @($dir, '/setowner', '*S-1-5-32-544', '/T', '/C')
  $dir
}

# What Explorer's Continue does: the administrator, explicitly, on everything.
function Add-Me([string]$dir) { Icacls @($dir, '/grant', "*${me}:(OI)(CI)F", '/T', '/C') }

function Invoke-Secure([string]$dir, [string]$sid) {
  $a = @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', $target, '-Path', $dir)
  if ($sid) { $a += @('-InstallerSid', $sid) }
  $script:lastOut = & $ps @a 2>&1 | Out-String
  Write-Host $script:lastOut
  $LASTEXITCODE
}

# Adds an entry written in SDDL (object and callback entries, which .NET's rule objects can't
# make) to $path; $false when this Windows won't store it, and the case is then skipped.
function Add-Sddl([string]$path, [string]$ace) {
  try {
    $acl = Get-Acl -LiteralPath $path
    $acl.SetSecurityDescriptorSddlForm($acl.Sddl + $ace, 'Access')
    Set-Acl -LiteralPath $path -AclObject $acl
    return (Get-Acl -LiteralPath $path).Sddl.Contains($ace.Substring(0, 4))
  } catch { Write-Host "could not add $ace to ${path}: $($_.Exception.Message)"; return $false }
}

# The installer's own message for an account Repair can't take, which sends people to icacls.
$manualFix = "Repair can't fix this for your account"

function Has-Me([string]$path) {
  @((Get-Acl -LiteralPath $path).GetAccessRules($true, $false, [Security.Principal.SecurityIdentifier]) |
    Where-Object { $_.IdentityReference.Value -eq $me }).Count -gt 0
}

$failed = @()
# Every refusal but the $manual ones must not send anyone to icacls.
function Expect([string]$case, [int]$want, [int]$got, [bool]$manual = $false) {
  $says = $script:lastOut -match [regex]::Escape($manualFix)
  if ($got -ne $want) { $script:failed += "${case}: exit $got, expected $want" }
  elseif ($says -ne $manual) { $script:failed += "${case}: the manual-fix message is $(if ($says) { 'there' } else { 'missing' })" }
  else { Write-Host "ok: $case (exit $got$(if ($manual) { ', manual fix' }))" }
}

try {
  # Re-secured: the administrator's entries go from the folder and everything in it.
  # Its logs folder as the service locks it: OWNER RIGHTS and CREATOR OWNER are fine.
  $d = New-Secured 'admin-only'; Add-Me $d
  Add-Entry (Join-Path $d 'logs') 'S-1-3-4'; Add-Entry (Join-Path $d 'logs') 'S-1-3-0'
  Expect 'only the installing administrator' 0 (Invoke-Secure $d $me)
  foreach ($p in @($d) + @(Get-ChildItem -LiteralPath $d -Force -Recurse | ForEach-Object FullName)) {
    if (Has-Me $p) { $failed += "the administrator's entry is still on $p" }
  }
  if (-not (Get-Acl -LiteralPath $d).AreAccessRulesProtected) { $failed += 'the re-secured folder inherits' }

  # Refused, and left as it was.
  $d = New-Secured 'no-sid'; Add-Me $d
  Expect 'no installer SID (not an administrator)' 1 (Invoke-Secure $d '')
  if (-not (Has-Me $d)) { $failed += 'a refused folder was changed' }

  $d = New-Secured 'other-sid'; Add-Me $d
  Expect "another account's SID" 1 (Invoke-Secure $d $other)

  $d = New-Secured 'not-an-admin'; Add-Entry $d $other
  Expect 'only its own entry, but not in Administrators' 1 (Invoke-Secure $d $other) $true

  $entra = 'S-1-12-1-1111111111-2222222222-3333333333-4444444444'
  $d = New-Secured 'entra'; Add-Entry $d $entra
  Expect 'a work or school account' 1 (Invoke-Secure $d $entra) $true

  # Not a member, and something inside is wrong too: never the manual fix.
  $d = New-Secured 'not-an-admin-deep'; Add-Entry $d $other; Icacls @((Join-Path $d 'logs\lodestar.log'), '/setowner', "*$me")
  Expect 'not in Administrators, and a file inside owned by a user' 1 (Invoke-Secure $d $other)

  $d = New-Secured 'users'; Icacls @($d, '/grant', '*S-1-5-32-545:(OI)(CI)R')
  Expect 'the Users group passed as the installer' 1 (Invoke-Secure $d 'S-1-5-32-545')

  $d = New-Secured 'domain-users'; Add-Entry $d $domainUsers 'Read'
  Expect 'a domain group passed as the installer' 1 (Invoke-Secure $d $domainUsers)

  $d = New-Secured 'deny'; Add-Me $d; Add-Entry $d $other 'Write' 'Deny'
  Expect 'the administrator plus a Deny entry' 1 (Invoke-Secure $d $me)

  $d = New-Secured 'second-account'; Add-Me $d; Add-Entry $d $other 'Read'
  Expect 'the administrator plus another account' 1 (Invoke-Secure $d $me)

  $d = New-Secured 'inherits'; Add-Me $d; Icacls @($d, '/inheritance:e')
  Expect 'the administrator plus inheritance' 1 (Invoke-Secure $d $me)

  $d = New-Secured 'user-owner'; Add-Me $d; Icacls @($d, '/setowner', "*$me")
  Expect 'the administrator plus a folder owned by a user' 1 (Invoke-Secure $d $me)

  $d = New-Secured 'user-file'; Add-Me $d; Icacls @((Join-Path $d 'logs'), '/setowner', "*$me")
  Expect 'the administrator plus a top-level entry owned by a user' 1 (Invoke-Secure $d $me)

  $d = New-Secured 'link'; Add-Me $d
  New-Item -ItemType Junction -Path (Join-Path $d 'logs\out') -Target $root | Out-Null
  Expect 'the administrator plus a junction inside' 1 (Invoke-Secure $d $me)

  # Found only after the lock, deep inside.
  $d = New-Secured 'deep-owner'; Add-Me $d; Icacls @((Join-Path $d 'logs\lodestar.log'), '/setowner', "*$me")
  Expect 'the administrator plus a file inside owned by a user' 1 (Invoke-Secure $d $me)

  $d = New-Secured 'deep-entry'; Add-Me $d; Icacls @((Join-Path $d 'logs\lodestar.log'), '/grant', '*S-1-1-0:R')
  Expect 'the administrator plus a file inside open to Everyone' 1 (Invoke-Secure $d $me)

  # A conditional (callback) Allow for Everyone deep inside: .NET's own rule list leaves those
  # out, so only a check of the raw entries finds it.
  $d = New-Secured 'deep-callback'; Add-Me $d
  if (Add-Sddl (Join-Path $d 'logs\lodestar.log') '(XA;;FR;;;WD;(Exists LodestarTest))') {
    Expect 'the administrator plus a callback entry inside' 1 (Invoke-Secure $d $me)
  } else { Write-Host '::warning::skipped the callback-entry case: this Windows would not store one' }

  # The same on the folder itself, where .NET's rule list would miss them too.
  $d = New-Secured 'top-object'; Add-Me $d
  if (Add-Sddl $d '(OA;;FA;;;WD)') { Expect 'the administrator plus an object entry on the folder' 1 (Invoke-Secure $d $me) }
  else { Write-Host '::warning::skipped the top object-entry case: this Windows would not store one' }
  $d = New-Secured 'top-callback'; Add-Me $d
  if (Add-Sddl $d '(XA;;FA;;;WD;(Exists LodestarTest))') { Expect 'the administrator plus a callback entry on the folder' 1 (Invoke-Secure $d $me) }
  else { Write-Host '::warning::skipped the top callback-entry case: this Windows would not store one' }

  # Only an exact user SID counts: no lower case, no trailing newline, no missing part.
  $d = New-Secured 'sid-newline'; Add-Me $d
  Expect 'the installer SID with a trailing newline' 1 (Invoke-Secure $d "$me`n")
  $d = New-Secured 'sid-lower'; Add-Me $d
  Expect 'the installer SID in lower case' 1 (Invoke-Secure $d $me.ToLowerInvariant())
  $d = New-Secured 'sid-short'; Add-Me $d
  Expect 'a malformed installer SID' 1 (Invoke-Secure $d ($me -replace '-[0-9]+$', ''))
  $d = New-Secured 'sid-unicode'; Add-Me $d
  Expect 'the installer SID with a non-ASCII digit' 1 (Invoke-Secure $d ($me.Substring(0, $me.Length - 1) + [char]0x0661))
  $d = New-Secured 'sid-space'; Add-Me $d
  Expect 'the installer SID with spaces around it' 1 (Invoke-Secure $d " $me ")

  $d = New-Secured 'no-marker'; Add-Me $d; Remove-Item -LiteralPath (Join-Path $d '.lodestar-secured')
  Expect 'the administrator but no marker' 1 (Invoke-Secure $d $me)
} finally {
  # Junctions first, so removing the tree never follows one.
  Get-ChildItem -LiteralPath $root -Recurse -Force -Attributes ReparsePoint -ErrorAction SilentlyContinue |
    ForEach-Object { [IO.Directory]::Delete($_.FullName) }
  & $icacls $root /reset /T /C /Q | Out-Null
  Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

if ($failed.Count) { throw ("secure-datadir tests failed:`n" + ($failed -join "`n")) }
Write-Host 'secure-datadir tests ok'
# The cleanup's icacls left its own exit code; the result is the tests'.
exit 0
