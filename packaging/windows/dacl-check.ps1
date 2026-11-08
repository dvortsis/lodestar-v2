<#
.SYNOPSIS
  The raw access-list check: Get-RawDescriptor reads a file's or folder's security descriptor
  as Windows stores it, and Get-DaclProblem names the first problem with its access list the
  way crates/lodestar/src/windows/acl.rs (check_entries) does. Dot-sourced by
  secure-datadir.ps1 and tested by test-secure-datadir.ps1.

  .NET's own view (Get-Acl, GetAccessRules, .Access, .Sddl) isn't relied on: its rule list
  leaves object entries (OA, OD, with an object type) out, its descriptor for a file is
  rebuilt as a non-directory-service CommonSecurityDescriptor that need not keep them, and it
  shows a callback (conditional) entry (XA, XD) as an unconditional Allow or Deny. So the
  descriptor is read with GetNamedSecurityInfo and kept as a RawSecurityDescriptor, which
  keeps every entry as Windows stores it.
#>

# Compiles the reader once. Add-Type writes its source and output to the temp folder, so
# $PrivateTemp must be a folder only SYSTEM and Administrators can write: when this runs as
# SYSTEM, the usual temp folder (C:\Windows\Temp) is writable by every user.
function Initialize-RawReader([Parameter(Mandatory)][string]$PrivateTemp) {
  if ('Lodestar.RawSd' -as [type]) { return }
  $tmp, $temp = $env:TMP, $env:TEMP
  try {
    $env:TMP = $PrivateTemp; $env:TEMP = $PrivateTemp
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
namespace Lodestar {
  public static class RawSd {
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode)]
    static extern uint GetNamedSecurityInfoW(string name, int type, int info, IntPtr owner,
      IntPtr group, IntPtr dacl, IntPtr sacl, out IntPtr sd);
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool ConvertSecurityDescriptorToStringSecurityDescriptorW(IntPtr sd,
      uint revision, int info, out IntPtr text, out uint length);
    [DllImport("kernel32.dll")]
    static extern IntPtr LocalFree(IntPtr p);
    // SE_FILE_OBJECT; OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION.
    const int File = 1, OwnerAndDacl = 1 | 4;
    public static string Sddl(string path) {
      IntPtr sd;
      uint e = GetNamedSecurityInfoW(path, File, OwnerAndDacl, IntPtr.Zero, IntPtr.Zero,
        IntPtr.Zero, IntPtr.Zero, out sd);
      if (e != 0) throw new Win32Exception((int)e);
      try {
        IntPtr text; uint length;
        if (!ConvertSecurityDescriptorToStringSecurityDescriptorW(sd, 1, OwnerAndDacl, out text, out length))
          throw new Win32Exception(Marshal.GetLastWin32Error());
        try { return Marshal.PtrToStringUni(text); } finally { LocalFree(text); }
      } finally { LocalFree(sd); }
    }
  }
}
'@
  } finally {
    $env:TMP, $env:TEMP = $tmp, $temp
  }
}

# The descriptor of $path as Windows stores it (owner and DACL), every entry kept.
function Get-RawDescriptor([Parameter(Mandatory)][string]$path) {
  New-Object Security.AccessControl.RawSecurityDescriptor ([Lodestar.RawSd]::Sddl($path))
}

# $null only when $raw has a DACL and every entry is a plain Allow for a SID in $allowed.
function Get-DaclProblem([Security.AccessControl.RawSecurityDescriptor]$raw, $allowed) {
  if ($null -eq $raw.DiscretionaryAcl) { return 'it has no access list, so anyone can open it' }
  foreach ($ace in $raw.DiscretionaryAcl) {
    $who = if ($ace -is [Security.AccessControl.KnownAce]) { $ace.SecurityIdentifier } else { $null }
    if ($ace.AceType -ne [Security.AccessControl.AceType]::AccessAllowed) { return "it has an entry of type $($ace.AceType) for $who" }
    if ($allowed -notcontains $who) { return "it lets $who in" }
  }
  return $null
}
