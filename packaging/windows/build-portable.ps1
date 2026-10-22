<#
.SYNOPSIS
  Pack the portable zip: lodestar.exe, a launcher, LICENSE and a README, plus a checksum.

.EXAMPLE
  packaging\windows\build-portable.ps1 -Version 0.5.0 -OutDir dist
#>
param(
  [Parameter(Mandatory)][string]$Version,   # no leading v; may carry -beta.1
  [Parameter(Mandatory)][string]$OutDir,
  [string]$Target = 'x86_64-pc-windows-msvc'
)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$exe = Join-Path $root "target\$Target\release\lodestar.exe"
if (-not (Test-Path $exe)) { throw "build first: $exe not found" }
New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path

$name = "lodestar-$Version-x86_64-windows-portable"
$stage = Join-Path ([IO.Path]::GetTempPath()) ("lodestar-portable-" + [guid]::NewGuid())
$dir = Join-Path $stage $name
New-Item -ItemType Directory -Force $dir | Out-Null
try {
  Copy-Item $exe (Join-Path $dir 'lodestar.exe')
  Copy-Item (Join-Path $root 'LICENSE') (Join-Path $dir 'LICENSE')
  Copy-Item (Join-Path $PSScriptRoot 'portable\README.txt') (Join-Path $dir 'README.txt')
  Copy-Item (Join-Path $PSScriptRoot 'portable\Start Lodestar.cmd') (Join-Path $dir 'Start Lodestar.cmd')

  $zip = Join-Path $OutDir "$name.zip"
  if (Test-Path $zip) { Remove-Item $zip }
  Compress-Archive -Path $dir -DestinationPath $zip -CompressionLevel Optimal
} finally { Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue }

$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
"$hash  $(Split-Path $zip -Leaf)" | Set-Content -Path "$zip.sha256" -Encoding ascii
Write-Host "wrote $zip"
Write-Host "$hash  $(Split-Path $zip -Leaf)"
