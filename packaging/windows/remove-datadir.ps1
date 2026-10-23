<#
.SYNOPSIS
  Deletes the Lodestar data folder on request (msiexec /x ... REMOVE_DATA=1). Run by the MSI
  as SYSTEM. `rmdir /s /q` is not used: a link or junction planted inside the folder could
  point it at files that are not Lodestar's. Refuses (leaves the folder alone, exit 1) if the
  folder or anything inside it is a link, and never follows one.
#>
param([Parameter(Mandatory)][string]$Path)
$ErrorActionPreference = 'Stop'

$dir = [IO.Path]::GetFullPath($Path).TrimEnd('\')
$expected = Join-Path $env:ProgramData 'Lodestar'
if ($dir -ne $expected) {
  Write-Host "LODESTAR: not deleting $dir: it is not $expected"
  exit 1
}
if (-not (Test-Path -LiteralPath $dir)) { exit 0 }

$item = Get-Item -LiteralPath $dir -Force
if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
  Write-Host "LODESTAR: not deleting $dir: it is a link or junction"
  exit 1
}
$link = Get-ChildItem -LiteralPath $dir -Force -Recurse -Attributes ReparsePoint -ErrorAction Stop | Select-Object -First 1
if ($link) {
  Write-Host "LODESTAR: not deleting the data folder: it contains a link or junction"
  exit 1
}
[IO.Directory]::Delete($dir, $true)
