<#
.SYNOPSIS
  Authenticode-sign the given files with signtool. SKIPPED (exit 0, with a notice)
  when the certificate secret is not set, so unsigned builds keep working.

  Needs, in the environment (GitHub secrets mapped by release.yml):
    WINDOWS_CERT_PFX_BASE64  the code-signing certificate (.pfx), base64
    WINDOWS_CERT_PASSWORD    its password
  Optional: WINDOWS_TIMESTAMP_URL (default http://timestamp.digicert.com)

  OWNER ACTION: obtain a code-signing certificate and add the two secrets. Until
  then every Windows artifact is unsigned and SmartScreen will warn (docs say so).
#>
param([Parameter(Mandatory, ValueFromRemainingArguments)][string[]]$Files)
$ErrorActionPreference = 'Stop'
if (-not $env:WINDOWS_CERT_PFX_BASE64) {
  Write-Host '::notice::Code signing SKIPPED: WINDOWS_CERT_PFX_BASE64 is not set (owner action).'
  exit 0
}
$signtool = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin\*\x64\signtool.exe' |
  Sort-Object FullName -Descending | Select-Object -First 1
if (-not $signtool) { throw 'signtool.exe not found' }
$pfx = Join-Path ([IO.Path]::GetTempPath()) 'lodestar-sign.pfx'
[IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($env:WINDOWS_CERT_PFX_BASE64))
$ts = if ($env:WINDOWS_TIMESTAMP_URL) { $env:WINDOWS_TIMESTAMP_URL } else { 'http://timestamp.digicert.com' }
try {
  foreach ($f in $Files) {
    & $signtool.FullName sign /fd SHA256 /f $pfx /p $env:WINDOWS_CERT_PASSWORD /tr $ts /td SHA256 $f
    if ($LASTEXITCODE -ne 0) { throw "signtool failed on $f" }
    & $signtool.FullName verify /pa $f
    if ($LASTEXITCODE -ne 0) { throw "signature on $f does not verify" }
  }
} finally { Remove-Item $pfx -Force -ErrorAction SilentlyContinue }
