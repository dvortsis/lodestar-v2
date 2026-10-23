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
  real folder (no link or junction anywhere inside), owned by SYSTEM or
  Administrators, and either empty or already secured by an earlier install
  (the .lodestar-secured marker). Then the same ACL is applied again.

  Matches crates/lodestar/src/windows/acl.rs in the service.
#>
param([Parameter(Mandatory)][string]$Path, [string]$Service = 'Lodestar')
$ErrorActionPreference = 'Stop'

function Stop-Install($msg) {
  Write-Host "LODESTAR INSTALL STOPPED: $msg"
  exit 1
}

$dir = [IO.Path]::GetFullPath($Path).TrimEnd('\')
$marker = '.lodestar-secured'
$system = New-Object Security.Principal.SecurityIdentifier 'S-1-5-18'
$admins = New-Object Security.Principal.SecurityIdentifier 'S-1-5-32-544'
try {
  $svc = (New-Object Security.Principal.NTAccount "NT SERVICE\$Service").Translate([Security.Principal.SecurityIdentifier])
} catch {
  Stop-Install "the service account NT SERVICE\$Service does not exist yet: $_"
}

function New-DataSecurity {
  $sec = New-Object Security.AccessControl.DirectorySecurity
  $sec.SetAccessRuleProtection($true, $false)   # no inheritance, drop inherited entries
  foreach ($sid in $system, $admins, $svc) {
    $sec.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule(
      $sid, 'FullControl', 'ContainerInherit, ObjectInherit', 'None', 'Allow')))
  }
  $sec
}

if (Test-Path -LiteralPath $dir) {
  $item = Get-Item -LiteralPath $dir -Force
  if (-not $item.PSIsContainer) { Stop-Install "$dir is a file, not a folder." }
  if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
    Stop-Install "$dir is a link or junction. Remove it and install again."
  }
  $owner = (Get-Acl -LiteralPath $dir).GetOwner([Security.Principal.SecurityIdentifier])
  if ($owner -ne $system -and $owner -ne $admins) {
    Stop-Install "$dir already exists and is owned by $owner, not SYSTEM or Administrators. Check it, move it away, then install again."
  }
  $inside = @(Get-ChildItem -LiteralPath $dir -Force -ErrorAction Stop)
  if ($inside.Count -gt 0 -and -not (Test-Path -LiteralPath (Join-Path $dir $marker))) {
    Stop-Install "$dir already exists, is not empty and was not secured by Lodestar, so another user could have put files in it. Move its contents somewhere else, check them, then install again (an empty folder is fine)."
  }
  $link = Get-ChildItem -LiteralPath $dir -Force -Recurse -Attributes ReparsePoint -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($link) { Stop-Install "$($link.FullName) is a link or junction inside the data folder. Remove it and install again." }
  Set-Acl -LiteralPath $dir -AclObject (New-DataSecurity)
  Write-Host "re-secured $dir"
} else {
  $parent = Split-Path -Parent $dir
  if (-not (Test-Path -LiteralPath $parent)) { Stop-Install "$parent does not exist." }
  (New-Object IO.DirectoryInfo $dir).Create((New-DataSecurity))
  Write-Host "created $dir"
}
