<#
.SYNOPSIS
  Creates or re-checks the Lodestar data folder with its final permissions. Run
  by the MSI after the service is registered (so NT SERVICE\Lodestar resolves),
  as SYSTEM.

  New folder: created with its security descriptor in the same call, so there is
  no moment when it carries ProgramData's inherited rights (Users can create
  entries there). Inheritance off; full control for SYSTEM, Administrators and
  the service's own account only.

  Existing folder (a reinstall, or something planted): refused unless it is a
  real folder (no link or junction anywhere inside), owned by SYSTEM,
  Administrators or the service, and either empty or already secured by an earlier install:
  the .lodestar-secured marker, a protected ACL that allows only SYSTEM,
  Administrators and the service, and top-level entries owned by those. Then the
  same ACL is applied again.

  Matches crates/lodestar/src/windows/acl.rs in the service.
#>
param([Parameter(Mandatory)][string]$Path, [string]$Service = 'Lodestar')
$ErrorActionPreference = 'Stop'

$system = New-Object Security.Principal.SecurityIdentifier 'S-1-5-18'
$admins = New-Object Security.Principal.SecurityIdentifier 'S-1-5-32-544'

# A deferred MSI action's output is lost, so keep a log the install failure can point to.
# This runs as SYSTEM and C:\Windows\Temp is writable by every user, so the log never goes
# to a name someone else could have prepared (a symlink would turn an append into a write
# anywhere): it goes into a new folder with a random name that only SYSTEM and
# Administrators can use, in a file opened with CreateNew, which fails if the name exists.
$script:logWriter = $null
try {
  $logDir = Join-Path $env:SystemRoot ('Temp\lodestar-secure-' + [guid]::NewGuid().ToString('N'))
  $logSec = New-Object Security.AccessControl.DirectorySecurity
  $logSec.SetAccessRuleProtection($true, $false)
  foreach ($sid in $system, $admins) {
    $logSec.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule(
      $sid, 'FullControl', 'ContainerInherit, ObjectInherit', 'None', 'Allow')))
  }
  (New-Object IO.DirectoryInfo $logDir).Create($logSec)
  $stream = [IO.File]::Open((Join-Path $logDir 'secure-datadir.log'), [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
  $script:logWriter = New-Object IO.StreamWriter($stream)
} catch {
  Write-Host "LODESTAR: no log kept: $($_.Exception.Message)"
}

# File and folder names come from whoever could write there: keep only printable ASCII, and
# break up "::" so a name cannot pass for a CI workflow command when a log is printed.
function Safe($text) { ([string]$text -replace '[^\x20-\x7E]', '?') -replace '::', ': :' }
function Log($msg) {
  if ($script:logWriter) {
    $script:logWriter.WriteLine(("{0:s} {1}" -f (Get-Date), (Safe $msg)))
    $script:logWriter.Flush()
  }
}
Log "start: $Path"

function Stop-Install($msg) {
  Log "STOPPED: $msg"
  Write-Host ("LODESTAR INSTALL STOPPED: " + (Safe $msg))
  exit 1
}

trap { Log "ERROR: $($_ | Out-String)"; exit 2 }

$dir = [IO.Path]::GetFullPath($Path).TrimEnd('\')
$marker = '.lodestar-secured'
# The service's own SID, worked out (see service-sid.ps1): translating NT SERVICE\<name>
# fails while the service has no SID type yet, as in this install.
. (Join-Path $PSScriptRoot 'service-sid.ps1')
$svc = Get-ServiceSid $Service
Log "service SID: $svc"

function New-DataSecurity {
  $sec = New-Object Security.AccessControl.DirectorySecurity
  $sec.SetAccessRuleProtection($true, $false)   # no inheritance, drop inherited entries
  foreach ($sid in $system, $admins, $svc) {
    $sec.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule(
      $sid, 'FullControl', 'ContainerInherit, ObjectInherit', 'None', 'Allow')))
  }
  $sec
}

$trusted = @($system, $admins, $svc)

# Why the folder is not private yet, or $null if it is: protected (no inheritance from
# ProgramData) and every Allow entry names SYSTEM, Administrators or the service. A NULL
# DACL and object-specific ACEs are not looked at: wherever this is used the ACL is
# re-applied afterwards, so they cannot survive.
function Get-NotPrivateReason($folder) {
  $acl = Get-Acl -LiteralPath $folder
  if (-not $acl.AreAccessRulesProtected) { return 'it inherits permissions from ProgramData' }
  foreach ($rule in $acl.Access) {
    $who = $rule.IdentityReference.Translate([Security.Principal.SecurityIdentifier])
    if ($rule.AccessControlType -eq 'Allow' -and $trusted -notcontains $who) {
      return "it lets $($rule.IdentityReference) in"
    }
  }
  return $null
}

# Creates the folder with its descriptor, then checks that what is there is ours: if someone
# created it in the meantime, its owner or its ACL will not be.
function New-DataFolder {
  (New-Object IO.DirectoryInfo $dir).Create((New-DataSecurity))
  $owner = (Get-Acl -LiteralPath $dir).GetOwner([Security.Principal.SecurityIdentifier])
  $why = Get-NotPrivateReason $dir
  if ($trusted -notcontains $owner -or $why) {
    Stop-Install "$dir was created by someone else while Lodestar was installing (owner $owner). Check it, move it away, then install again."
  }
}

if (Test-Path -LiteralPath $dir) {
  $item = Get-Item -LiteralPath $dir -Force
  if (-not $item.PSIsContainer) { Stop-Install "$dir is a file, not a folder." }
  if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
    Stop-Install "$dir is a link or junction. Remove it and install again."
  }
  # The service creates a missing folder under its own SID, so that owner is fine too.
  $owner = (Get-Acl -LiteralPath $dir).GetOwner([Security.Principal.SecurityIdentifier])
  if ($trusted -notcontains $owner) {
    Stop-Install "$dir already exists and is owned by $owner, not SYSTEM, Administrators or the Lodestar service. Check it, move it away, then install again."
  }
  $inside = @(Get-ChildItem -LiteralPath $dir -Force -ErrorAction Stop)

  if ($inside.Count -eq 0) {
    $why = Get-NotPrivateReason $dir
    if ($why) {
      # Empty but not private: someone may hold a handle opened while it was open to
      # them (with WRITE_DAC, say), and changing the ACL in place would not close it.
      # Make a new folder instead.
      Log "replacing the empty folder $dir ($why)"
      try { [IO.Directory]::Delete($dir, $false) }
      catch { Stop-Install "$dir is empty but $why, and it could not be removed (something has it open): $($_.Exception.Message) Close whatever uses it, or delete it yourself, then install again." }
      New-DataFolder
      Log "created $dir"
      exit 0
    }
  } else {
    if (-not (Test-Path -LiteralPath (Join-Path $dir $marker))) {
      Stop-Install "$dir already exists, is not empty and was not secured by Lodestar, so another user could have put files in it. Move its contents somewhere else, check them, then install again (an empty folder is fine)."
    }
    # The marker is only a hint: anyone who can write the folder can drop one. Trust
    # existing contents only if the folder is already locked down and everything in
    # it belongs to SYSTEM, Administrators or the service (same rules as the service's
    # start-up check, windows/acl.rs).
    $why = Get-NotPrivateReason $dir
    if ($why) {
      Stop-Install "$dir is not empty and $why, so other users may have put files in it. Move its contents somewhere else, check them, then install again."
    }
    foreach ($child in $inside) {
      $o = (Get-Acl -LiteralPath $child.FullName).GetOwner([Security.Principal.SecurityIdentifier])
      if ($trusted -notcontains $o) {
        Stop-Install "$($child.FullName) is owned by $o, not SYSTEM, Administrators or the Lodestar service. Check it or move it away, then install again."
      }
    }
  }
  # Fail closed: if the scan itself cannot run, the install stops (trap, exit 2).
  $link = Get-ChildItem -LiteralPath $dir -Force -Recurse -Attributes ReparsePoint -ErrorAction Stop | Select-Object -First 1
  if ($link) { Stop-Install "$($link.FullName) is a link or junction inside the data folder. Remove it and install again." }
  Set-Acl -LiteralPath $dir -AclObject (New-DataSecurity)
  Log "re-secured $dir"
} else {
  $parent = Split-Path -Parent $dir
  if (-not (Test-Path -LiteralPath $parent)) { Stop-Install "$parent does not exist." }
  New-DataFolder
  Log "created $dir"
}
