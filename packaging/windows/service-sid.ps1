<#
.SYNOPSIS
  Get-ServiceSid: the per-service SID of NT SERVICE\<name>, worked out without asking
  Windows to translate the name (that lookup fails until the service has a SID type).
  S-1-5-80 plus five little-endian 32-bit words of the SHA-1 of the upper-cased name
  in UTF-16LE. Dot-sourced by secure-datadir.ps1 and tested by test-service-sid.ps1.
#>
function Get-ServiceSid([Parameter(Mandatory)][string]$Name) {
  $hash = [Security.Cryptography.SHA1]::Create().ComputeHash([Text.Encoding]::Unicode.GetBytes($Name.ToUpperInvariant()))
  $words = 0..4 | ForEach-Object { [BitConverter]::ToUInt32($hash, 4 * $_) }
  New-Object Security.Principal.SecurityIdentifier ('S-1-5-80-' + ($words -join '-'))
}
