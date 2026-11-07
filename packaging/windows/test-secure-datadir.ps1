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

# A folder an earlier install secured: its marker, a file and a subfolder, all owned by
# Administrators, with the protected ACL secure-datadir.ps1 applies.
function New-Secured([string]$name) {
  $dir = Join-Path $root $name
  New-Item -ItemType Directory -Path $dir | Out-Null
  Set-Content -LiteralPath (Join-Path $dir '.lodestar-secured') -Value 'secured'
  New-Item -ItemType Directory -Path (Join-Path $dir 'logs') | Out-Null
  Set-Content -LiteralPath (Join-Path $dir 'logs\lodestar.log') -Value 'data'
  Icacls @($dir, '/inheritance:r', '/grant:r', '*S-1-5-18:(OI)(CI)F', '*S-1-5-32-544:(OI)(CI)F', "*${svc}:(OI)(CI)F")
  Icacls @($dir, '/setowner', '*S-1-5-32-544', '/T', '/C')
  $dir
}

# What Explorer's Continue does: the administrator, explicitly, on everything.
function Add-Me([string]$dir) { Icacls @($dir, '/grant', "*${me}:(OI)(CI)F", '/T', '/C') }

function Invoke-Secure([string]$dir, [string]$sid) {
  $a = @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', $target, '-Path', $dir)
  if ($sid) { $a += @('-InstallerSid', $sid) }
  & $ps @a | Out-Host
  $LASTEXITCODE
}

function Has-Me([string]$path) {
  @((Get-Acl -LiteralPath $path).GetAccessRules($true, $false, [Security.Principal.SecurityIdentifier]) |
    Where-Object { $_.IdentityReference.Value -eq $me }).Count -gt 0
}

$failed = @()
function Expect([string]$case, [int]$want, [int]$got) {
  if ($got -ne $want) { $script:failed += "${case}: exit $got, expected $want" }
  else { Write-Host "ok: $case (exit $got)" }
}

try {
  # Re-secured: the administrator's entries go from the folder and everything in it.
  $d = New-Secured 'admin-only'; Add-Me $d
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

  $d = New-Secured 'not-an-admin'; Icacls @($d, '/grant', "*${other}:(OI)(CI)F", '/T')
  Expect 'only its own entry, but not in Administrators' 1 (Invoke-Secure $d $other)

  $d = New-Secured 'users'; Icacls @($d, '/grant', '*S-1-5-32-545:(OI)(CI)R')
  Expect 'the Users group passed as the installer' 1 (Invoke-Secure $d 'S-1-5-32-545')

  $d = New-Secured 'domain-users'; Icacls @($d, '/grant', "*${domainUsers}:(OI)(CI)R")
  Expect 'a domain group passed as the installer' 1 (Invoke-Secure $d $domainUsers)

  $d = New-Secured 'deny'; Add-Me $d; Icacls @($d, '/deny', "*${other}:(OI)(CI)W")
  Expect 'the administrator plus a Deny entry' 1 (Invoke-Secure $d $me)

  $d = New-Secured 'second-account'; Add-Me $d; Icacls @($d, '/grant', "*${other}:(OI)(CI)R")
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
