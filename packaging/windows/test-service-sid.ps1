<#
.SYNOPSIS
  Checks Get-ServiceSid against a value Windows publishes: NT SERVICE\TrustedInstaller.
  Run in CI before the MSI is built:  pwsh packaging/windows/test-service-sid.ps1
#>
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'service-sid.ps1')

$want = 'S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464'
foreach ($name in 'TrustedInstaller', 'trustedinstaller', 'TRUSTEDINSTALLER') {   # case does not matter
  $got = (Get-ServiceSid $name).Value
  if ($got -ne $want) { throw "Get-ServiceSid '$name' = $got, expected $want" }
}
$a = (Get-ServiceSid 'Lodestar').Value
if ($a -notmatch '^S-1-5-80(-\d+){5}$') { throw "Get-ServiceSid 'Lodestar' = $a is not a service SID" }
if ($a -eq (Get-ServiceSid 'Other').Value) { throw 'different services got the same SID' }
Write-Host "service SID tests ok (Lodestar = $a)"
