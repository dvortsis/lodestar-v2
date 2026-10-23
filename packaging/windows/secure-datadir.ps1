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
  Administrators, and either empty or already secured by an earlier install:
  the .lodestar-secured marker, a protected ACL that allows only SYSTEM,
  Administrators and the service, and top-level entries owned by those. Then the
  same ACL is applied again.

  Matches crates/lodestar/src/windows/acl.rs in the service.
#>
param([Parameter(Mandatory)][string]$Path, [string]$Service = 'Lodestar')
$ErrorActionPreference = 'Stop'

# A deferred MSI action's output is lost, so keep a log the install failure can point to.
$log = Join-Path $env:SystemRoot 'Temp\lodestar-secure-datadir.log'
function Log($msg) { Add-Content -LiteralPath $log -Value ("{0:s} {1}" -f (Get-Date), $msg) }
Log "start: $Path"

function Stop-Install($msg) {
  Log "STOPPED: $msg"
  Write-Host "LODESTAR INSTALL STOPPED: $msg"
  exit 1
}

trap { Log "ERROR: $($_ | Out-String)"; exit 2 }

$dir = [IO.Path]::GetFullPath($Path).TrimEnd('\')
$marker = '.lodestar-secured'
$system = New-Object Security.Principal.SecurityIdentifier 'S-1-5-18'
$admins = New-Object Security.Principal.SecurityIdentifier 'S-1-5-32-544'
# The service's own SID is S-1-5-80 plus five 32-bit words of the SHA-1 of its upper-case
# name (UTF-16LE). Work it out rather than ask Windows to translate NT SERVICE\<name>: the
# lookup fails while the service has no SID type registered yet, as in this install.
$hash = [Security.Cryptography.SHA1]::Create().ComputeHash([Text.Encoding]::Unicode.GetBytes($Service.ToUpperInvariant()))
$words = 0..4 | ForEach-Object { [BitConverter]::ToUInt32($hash, 4 * $_) }
$svc = New-Object Security.Principal.SecurityIdentifier ('S-1-5-80-' + ($words -join '-'))
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
  if ($inside.Count -gt 0) {
    # The marker is only a hint: anyone who can write the folder can drop one. Trust
    # existing contents only if the folder is already locked down and everything in
    # it belongs to SYSTEM, Administrators or the service (same rules as the service's
    # start-up check, windows/acl.rs).
    $trusted = @($system, $admins, $svc)
    $acl = Get-Acl -LiteralPath $dir
    if (-not $acl.AreAccessRulesProtected) {
      Stop-Install "$dir inherits permissions from ProgramData, so other users may have put files in it. Move its contents somewhere else, check them, then install again."
    }
    foreach ($rule in $acl.Access) {
      $who = $rule.IdentityReference.Translate([Security.Principal.SecurityIdentifier])
      if ($rule.AccessControlType -eq 'Allow' -and $trusted -notcontains $who) {
        Stop-Install "$dir lets $($rule.IdentityReference) in. Only SYSTEM, Administrators and the Lodestar service may. Fix or move the folder, then install again."
      }
    }
    foreach ($child in $inside) {
      $o = (Get-Acl -LiteralPath $child.FullName).GetOwner([Security.Principal.SecurityIdentifier])
      if ($trusted -notcontains $o) {
        Stop-Install "$($child.FullName) is owned by $o, not SYSTEM, Administrators or the Lodestar service. Check it or move it away, then install again."
      }
    }
  }
  $link = Get-ChildItem -LiteralPath $dir -Force -Recurse -Attributes ReparsePoint -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($link) { Stop-Install "$($link.FullName) is a link or junction inside the data folder. Remove it and install again." }
  Set-Acl -LiteralPath $dir -AclObject (New-DataSecurity)
  Log "re-secured $dir"
} else {
  $parent = Split-Path -Parent $dir
  if (-not (Test-Path -LiteralPath $parent)) { Stop-Install "$parent does not exist." }
  (New-Object IO.DirectoryInfo $dir).Create((New-DataSecurity))
  Log "created $dir"
}
