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

  One exception (owner decision 2.52): when the one problem with such a folder is an
  Allow entry for the administrator running the install (Windows Explorer adds one,
  on the folder and everything in it, when you click Continue on its permission
  prompt), that entry is removed everywhere and the folder re-secured. The MSI passes the
  installing user's SID as -InstallerSid, and it counts only if that account is in the local
  Administrators group. The folder is locked first and then everything in it is checked again:
  any link, other owner or other account's entry anywhere inside still stops the install.

  Matches crates/lodestar/src/windows/acl.rs in the service.
#>
param([Parameter(Mandatory)][string]$Path, [string]$Service = 'Lodestar', [string]$InstallerSid = '')
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

# Whether $sid is a direct member of the local Administrators group: $false when the group
# cannot be read (a domain member that cannot be looked up, say), so Repair then refuses.
function Test-LocalAdmin($sid) {
  try {
    $name = $admins.Translate([Security.Principal.NTAccount]).Value.Split('\')[-1]
    $group = [ADSI]"WinNT://$env:COMPUTERNAME/$name,group"
    foreach ($m in @($group.Invoke('Members'))) {
      $bytes = $m.GetType().InvokeMember('objectSid', 'GetProperty', $null, $m, $null)
      if ((New-Object Security.Principal.SecurityIdentifier($bytes, 0)) -eq $sid) { return $true }
    }
  } catch { Log "could not read the Administrators group: $($_.Exception.Message)" }
  return $false
}

# Runs icacls, reading its error output instead of throwing on it: Windows PowerShell turns a
# native tool's stderr into errors, which 'Stop' would make fatal before the exit code is seen.
function Invoke-Icacls([string[]]$arguments, [string]$fix) {
  $icacls = Join-Path $env:SystemRoot 'System32\icacls.exe'
  $ErrorActionPreference = 'Continue'
  $out = & $icacls @arguments 2>&1 | Out-String
  $code = $LASTEXITCODE
  $ErrorActionPreference = 'Stop'
  if ($code -ne 0) {
    Log "icacls $($arguments -join ' ') failed ($code): $out"
    Stop-Install "icacls could not change $dir (exit $code). As an administrator, run: $fix"
  }
}

# Stops the install unless everything inside $folder is a real file or folder, owned by
# SYSTEM, Administrators or the service, with Allow entries only for those three, OWNER RIGHTS
# (how the service locks its logs folder, granting only that trusted owner) and CREATOR OWNER
# (which names only whoever creates an item there, and only those three can). No Deny.
# Same rule as the service's log-folder check (windows/acl.rs, log_folder_private).
$ownerSids = @(
  (New-Object Security.Principal.SecurityIdentifier 'S-1-3-4'),   # OWNER RIGHTS
  (New-Object Security.Principal.SecurityIdentifier 'S-1-3-0'))   # CREATOR OWNER
function Assert-PrivateInside($folder) {
  # Fail closed: if the scan itself cannot run, the install stops (trap, exit 2).
  foreach ($item in @(Get-ChildItem -LiteralPath $folder -Force -Recurse -ErrorAction Stop)) {
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
      Stop-Install "$($item.FullName) is a link or junction inside the data folder. Remove it and install again."
    }
    $acl = Get-Acl -LiteralPath $item.FullName
    $o = $acl.GetOwner([Security.Principal.SecurityIdentifier])
    if ($trusted -notcontains $o) {
      Stop-Install "$($item.FullName) is owned by $o, not SYSTEM, Administrators or the Lodestar service. Someone else may have put it there: move the folder's contents somewhere else, check them, then install again."
    }
    foreach ($rule in $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
      $who = $rule.IdentityReference
      if ($rule.AccessControlType -ne 'Allow' -or ($trusted -notcontains $who -and $ownerSids -notcontains $who)) {
        Stop-Install "$($item.FullName) has its own permissions for $($rule.IdentityReference). Someone else may have changed it: move the folder's contents somewhere else, check them, then install again."
      }
    }
  }
}

# The installing administrator's SID, when the one thing that keeps the folder from being
# private is that account's own Allow entries; $null otherwise. Only a user account's SID
# (S-1-5-21-...) in the local Administrators group, never a well-known, built-in or domain
# group SID, never one already trusted. Any Deny entry, inheritance or other account means $null.
function Get-RepairableSid($folder) {
  if (-not $InstallerSid) { return $null }
  if ($InstallerSid -notmatch '^S-1-5-21(-\d+){4}$') { return $null }
  $me = New-Object Security.Principal.SecurityIdentifier $InstallerSid
  $rid = [long]($InstallerSid -split '-')[-1]
  if ($rid -ge 512 -and $rid -le 527) { return $null }   # Domain Admins, Domain Users and the other domain groups
  if ($trusted -contains $me) { return $null }
  $acl = Get-Acl -LiteralPath $folder
  if (-not $acl.AreAccessRulesProtected) { return $null }
  $found = $false
  foreach ($rule in $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
    if ($rule.AccessControlType -ne 'Allow') { return $null }
    if ($trusted -contains $rule.IdentityReference) { continue }
    if ($rule.IdentityReference -ne $me) { return $null }
    $found = $true
  }
  if (-not $found) { return $null }
  if (-not (Test-LocalAdmin $me)) { Log "$me is not in the local Administrators group"; return $null }
  return $me
}

# Creates the folder with its descriptor, then checks that what is there is ours: if someone
# created it in the meantime, its owner or its ACL will not be.
function New-DataFolder {
  (New-Object IO.DirectoryInfo $dir).Create((New-DataSecurity))
  # Create does nothing if the name already exists, and the ACL and owner checks below follow
  # links, so check for a link first: a junction planted in the gap would otherwise pass.
  if ((Get-Item -LiteralPath $dir -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
    Stop-Install "$dir is a link or junction. Remove it and install again."
  }
  # Create also succeeds when the folder already existed. A folder this call made is a few
  # seconds old; anything older was there before and is not ours to trust.
  $age = (Get-Date).ToUniversalTime() - (Get-Item -LiteralPath $dir -Force).CreationTimeUtc
  if ($age.TotalSeconds -gt 60) {
    Stop-Install "$dir already existed when it was to be created (made $([int]$age.TotalSeconds) s ago). Check it, move it away, then install again."
  }
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
    $repairSid = $null
    if ($why) {
      $repairSid = Get-RepairableSid $dir
      if (-not $repairSid) {
        Log "not repairable (installer SID '$InstallerSid'): $why"
        Stop-Install "$dir is not empty and $why, so other users may have put files in it. Move its contents somewhere else, check them, then install again."
      }
      Log "the only extra entries are the installing administrator's own ($repairSid): removing them"
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
  if ($repairSid) {
    $sidText = $repairSid.Value
    # Lock it first. With /L, icacls changes a link itself, never what it points to, so a folder
    # swapped for a junction in the meantime is never followed. Once that account's entries are
    # gone, only SYSTEM, Administrators and the service can change anything in the folder.
    Invoke-Icacls @($dir, '/remove:g', "*$sidText", '/T', '/C', '/L', '/Q') "icacls `"$dir`" /remove:g *$sidText /T /C"
    Log "removed $sidText from $dir and everything in it"
    # Then check it all again, now that nobody else can change it.
    $item = Get-Item -LiteralPath $dir -Force
    if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
      Stop-Install "$dir was replaced while Lodestar was installing. Check it, move it away, then install again."
    }
    $why = Get-NotPrivateReason $dir
    if ($why) { Stop-Install "$dir is still not private after removing ${sidText}: $why. Move its contents somewhere else, check them, then install again." }
    Assert-PrivateInside $dir
  }
  Set-Acl -LiteralPath $dir -AclObject (New-DataSecurity)
  Log "re-secured $dir"
} else {
  $parent = Split-Path -Parent $dir
  if (-not (Test-Path -LiteralPath $parent)) { Stop-Install "$parent does not exist." }
  New-DataFolder
  Log "created $dir"
}
