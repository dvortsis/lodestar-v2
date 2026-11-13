<#
.SYNOPSIS
  Build the Lodestar MSI with cargo-wix.

.EXAMPLE
  cargo build --release --locked -p lodestar --bin lodestar --no-default-features --features web --target x86_64-pc-windows-msvc
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

# The JoinWorker custom action (join.wxs, join-ca\): C# on .NET Framework 4, packed by WiX's
# MakeSfxCA with its Windows Installer interop (DTF), both from WiX Toolset 3's SDK folder.
$candle = Get-Command candle.exe -ErrorAction SilentlyContinue
$wixBin = if ($env:WIX) { Join-Path $env:WIX 'bin' } elseif ($candle) { Split-Path -Parent $candle.Source } else { throw 'WiX Toolset 3 not found (candle.exe on PATH, or WIX set)' }
$sdk = Join-Path (Split-Path -Parent $wixBin) 'SDK'
$dtf = Join-Path $sdk 'Microsoft.Deployment.WindowsInstaller.dll'
$sfxca = Join-Path $sdk 'x64\sfxca.dll'
$makeSfx = Join-Path $sdk 'MakeSfxCA.exe'
foreach ($f in $dtf, $sfxca, $makeSfx) { if (-not (Test-Path $f)) { throw "WiX SDK file missing: $f" } }
$csc = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
if (-not (Test-Path $csc)) { throw ".NET Framework 4 compiler missing: $csc" }
$caDir = Join-Path ([IO.Path]::GetTempPath()) ('lodestar-join-ca-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $caDir | Out-Null
$caSrc = Join-Path $PSScriptRoot 'join-ca'
$caLib = Join-Path $caDir 'LodestarJoinCA.dll'
& $csc /nologo /target:library /optimize+ /warnaserror+ "/out:$caLib" "/reference:$dtf" `
  (Join-Path $caSrc 'JoinRunner.cs') (Join-Path $caSrc 'JoinWorkerCA.cs')
if ($LASTEXITCODE -ne 0) { throw "csc failed ($LASTEXITCODE)" }
$env:LODESTAR_JOIN_CA = Join-Path $caDir 'LodestarJoinCA.CA.dll'
& $makeSfx $env:LODESTAR_JOIN_CA $sfxca $caLib (Join-Path $caSrc 'CustomAction.config') $dtf
if ($LASTEXITCODE -ne 0) { throw "MakeSfxCA failed ($LASTEXITCODE)" }

$msi = Join-Path $OutDir "lodestar-$Version-x86_64.msi"
Push-Location $root
try {
  cargo wix --package lodestar --target $Target --no-build --nocapture `
    --include (Join-Path $PSScriptRoot 'main.wxs') `
    --include (Join-Path $PSScriptRoot 'join.wxs') `
    --install-version $Version `
    --compiler-arg '-ext' --compiler-arg 'WixFirewallExtension' `
    --linker-arg '-ext' --linker-arg 'WixFirewallExtension' `
    --output $msi
  if ($LASTEXITCODE -ne 0) { throw "cargo wix failed ($LASTEXITCODE)" }
} finally { Pop-Location }

# No checksum here: the MSI is signed after it is built (sign.ps1), and the
# checksum has to be of the signed file. release.yml writes it.
Write-Host "wrote $msi"
