<#
.SYNOPSIS
  Build the Lodestar MSI with cargo-wix.

.EXAMPLE
  cargo build --release --locked -p lodestar --target x86_64-pc-windows-msvc
  packaging\windows\build-msi.ps1 -Version 0.5.0 -OutDir dist

  Needs WiX Toolset 3.x (candle.exe, light.exe on PATH or WIX set) and
  cargo-wix. The release binary must already be built (and signed, if signing).
#>
param(
  [Parameter(Mandatory)][string]$Version,   # MAJOR.MINOR.PATCH, no leading v
  [Parameter(Mandatory)][string]$OutDir,
  [string]$Target = 'x86_64-pc-windows-msvc'
)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "MSI versions are MAJOR.MINOR.PATCH, got '$Version'" }
$exe = Join-Path $root "target\$Target\release\lodestar.exe"
if (-not (Test-Path $exe)) { throw "build first: $exe not found" }
New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path

# The wizard needs the licence as RTF. Escape \ { } and keep line breaks.
$rtfPath = Join-Path ([IO.Path]::GetTempPath()) 'lodestar-license.rtf'
$body = (Get-Content (Join-Path $root 'LICENSE') -Raw) -replace '\\', '\\' -replace '\{', '\{' -replace '\}', '\}'
$body = ($body -replace "`r?`n", "\par`r`n")
Set-Content -Path $rtfPath -Value ('{\rtf1\ansi\deff0{\fonttbl{\f0\fmodern Consolas;}}\f0\fs18 ' + $body + '}') -Encoding ascii

$env:LODESTAR_PACKAGING = Join-Path $root 'packaging'
$env:LODESTAR_LICENSE_RTF = $rtfPath

$msi = Join-Path $OutDir "lodestar-$Version-x86_64.msi"
Push-Location $root
try {
  cargo wix --package lodestar --target $Target --no-build --nocapture `
    --include (Join-Path $PSScriptRoot 'main.wxs') `
    --install-version $Version `
    --compiler-arg '-ext' --compiler-arg 'WixUtilExtension' `
    --compiler-arg '-ext' --compiler-arg 'WixFirewallExtension' `
    --linker-arg '-ext' --linker-arg 'WixUIExtension' `
    --linker-arg '-ext' --linker-arg 'WixUtilExtension' `
    --linker-arg '-ext' --linker-arg 'WixFirewallExtension' `
    --output $msi
  if ($LASTEXITCODE -ne 0) { throw "cargo wix failed ($LASTEXITCODE)" }
} finally { Pop-Location }

# No checksum here: the MSI is signed after it is built (sign.ps1), and the
# checksum has to be of the signed file. release.yml writes it.
Write-Host "wrote $msi"
