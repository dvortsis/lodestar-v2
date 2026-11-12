<#
.SYNOPSIS
  The join token's temporary file: a folder only SYSTEM and Administrators can use, a file
  checked before the token goes in, and a delete that is checked. Dot-sourced by
  join-worker.ps1 and tested by test-join-worker.ps1. Needs dacl-check.ps1's reader
  (Initialize-RawReader) loaded first.
#>

$script:jtSystem = New-Object Security.Principal.SecurityIdentifier 'S-1-5-18'
$script:jtAdmins = New-Object Security.Principal.SecurityIdentifier 'S-1-5-32-544'
# Allowed in the access list: SYSTEM and Administrators only.
$script:jtTrusted = @($script:jtSystem, $script:jtAdmins)
# Allowed as owner: those two, or the account running this (SYSTEM in the MSI; Administrators
# or the elevated account in a test), since that account made the folder.
$script:jtOwners = @($script:jtSystem, $script:jtAdmins, [Security.Principal.WindowsIdentity]::GetCurrent().User)

# Creates $path with a protected list for SYSTEM and Administrators, set in the same call, so
# it never has the parent's inherited rights (every user can create files in C:\Windows\Temp).
# Refuses a name that already exists.
function New-PrivateFolder([Parameter(Mandatory)][string]$path) {
  if ([IO.Directory]::Exists($path) -or [IO.File]::Exists($path)) { throw "$path already exists" }
  $sec = New-Object Security.AccessControl.DirectorySecurity
  $sec.SetAccessRuleProtection($true, $false)
  foreach ($sid in $script:jtTrusted) {
    $sec.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule(
      $sid, 'FullControl', 'ContainerInherit, ObjectInherit', 'None', 'Allow')))
  }
  (New-Object IO.DirectoryInfo $path).Create($sec)
  # Create also succeeds on a name made in between: the check below catches that one.
}

# Why $path isn't private to SYSTEM and Administrators, or $null. A real file or folder (no
# link or junction), owned by one of $jtOwners, every entry read raw a plain Allow for SYSTEM
# or Administrators (Get-DaclProblem), and a folder's list protected from inheritance.
function Get-PrivateProblem([Parameter(Mandatory)][string]$path) {
  $item = Get-Item -LiteralPath $path -Force
  if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { return 'it is a link or junction' }
  $raw = Get-RawDescriptor $path
  if ($script:jtOwners -notcontains $raw.Owner) { return "it is owned by $($raw.Owner)" }
  $protected = [Security.AccessControl.ControlFlags]::DiscretionaryAclProtected
  if ($item.PSIsContainer -and -not ($raw.ControlFlags -band $protected)) {
    return 'it inherits permissions from its parent'
  }
  return Get-DaclProblem $raw $script:jtTrusted
}

# Creates $file empty (CreateNew: fails if the name exists, so nothing planted is reused),
# checks the folder and the file are private, and only then writes the token and a newline.
# Throws, with the token not written, if either check fails. Closed before returning.
function Write-TokenFile([Parameter(Mandatory)][string]$file, [Parameter(Mandatory)][string]$token) {
  $stream = [IO.File]::Open($file, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
  try {
    foreach ($p in (Split-Path -Parent $file), $file) {
      $why = Get-PrivateProblem $p
      if ($why) { throw "not private (${p}: $why)" }
    }
    $bytes = [Text.Encoding]::ASCII.GetBytes($token + "`n")
    try {
      $stream.Write($bytes, 0, $bytes.Length)
      $stream.Flush($true)
    } finally { [Array]::Clear($bytes, 0, $bytes.Length) }
  } finally { $stream.Dispose() }
}

# Deletes $dir and everything in it, retrying while a stopped process still holds the file
# (Windows can, for a moment), up to a 10 s deadline. $true once it is gone.
function Remove-TokenFolder([Parameter(Mandatory)][string]$dir, [scriptblock]$log = { }) {
  $deadline = [DateTime]::UtcNow.AddSeconds(10)
  while ($true) {
    try {
      if ([IO.Directory]::Exists($dir)) { [IO.Directory]::Delete($dir, $true) }
    } catch { & $log "could not remove the token folder yet: $($_.Exception.Message)" }
    if (-not [IO.Directory]::Exists($dir)) { return $true }
    if ([DateTime]::UtcNow -ge $deadline) { return $false }
    Start-Sleep -Milliseconds 200
  }
}
