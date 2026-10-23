<#
.SYNOPSIS
  Deletes the Lodestar data folder on request (msiexec /x ... REMOVE_DATA=1). Run by the MSI
  as SYSTEM. `rmdir /s /q` is not used: a link or junction planted inside the folder could
  point it at files that are not Lodestar's. Refuses (leaves the folder alone, exit 1) if the
  folder or anything inside it is a link, and never follows one.
#>
param([Parameter(Mandatory)][string]$Path)
$ErrorActionPreference = 'Stop'

# Output of a deferred MSI action is lost, so keep a log in a new SYSTEM/Administrators-only
# folder (same scheme as secure-datadir.ps1: never a name someone else could have prepared).
$script:logWriter = $null
try {
  $logDir = Join-Path $env:SystemRoot ('Temp\lodestar-secure-' + [guid]::NewGuid().ToString('N'))
  $logSec = New-Object Security.AccessControl.DirectorySecurity
  $logSec.SetAccessRuleProtection($true, $false)
  foreach ($sid in 'S-1-5-18', 'S-1-5-32-544') {
    $logSec.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule(
      (New-Object Security.Principal.SecurityIdentifier $sid), 'FullControl', 'ContainerInherit, ObjectInherit', 'None', 'Allow')))
  }
  (New-Object IO.DirectoryInfo $logDir).Create($logSec)
  $stream = [IO.File]::Open((Join-Path $logDir 'remove-datadir.log'), [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
  $script:logWriter = New-Object IO.StreamWriter($stream)
} catch { }
function Log($msg) {
  if ($script:logWriter) {
    $text = ([string]$msg -replace '[^\x20-\x7E]', '?') -replace '::', ': :'
    $script:logWriter.WriteLine(("{0:s} {1}" -f (Get-Date), $text)); $script:logWriter.Flush()
  }
}
trap { Log "ERROR: $($_ | Out-String)"; exit 2 }
Log "start: $Path"

$dir = [IO.Path]::GetFullPath($Path).TrimEnd('\')
$expected = Join-Path $env:ProgramData 'Lodestar'
if ($dir -ne $expected) {
  Log "not deleting $dir: it is not $expected"
  Write-Host "LODESTAR: not deleting $dir: it is not $expected"
  exit 1
}
if (-not (Test-Path -LiteralPath $dir)) { Log 'nothing to delete'; exit 0 }

$item = Get-Item -LiteralPath $dir -Force
if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
  Log "not deleting $dir: it is a link or junction"
  Write-Host "LODESTAR: not deleting $dir: it is a link or junction"
  exit 1
}
$link = Get-ChildItem -LiteralPath $dir -Force -Recurse -Attributes ReparsePoint -ErrorAction Stop | Select-Object -First 1
if ($link) {
  Log "not deleting the data folder: it contains a link or junction: $($link.FullName)"
  Write-Host "LODESTAR: not deleting the data folder: it contains a link or junction"
  exit 1
}
[IO.Directory]::Delete($dir, $true)
Log "deleted $dir"
