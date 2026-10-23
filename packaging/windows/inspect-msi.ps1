<#
.SYNOPSIS
  Prints the MSI tables that decide what the installer does with the service, for CI
  diagnostics:  inspect-msi.ps1 <file.msi>
#>
param([Parameter(Mandatory)][string]$Msi)
$ErrorActionPreference = 'Stop'
$installer = New-Object -ComObject WindowsInstaller.Installer
$db = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @((Resolve-Path $Msi).ProviderPath, 0))
foreach ($q in 'SELECT * FROM `ServiceInstall`', 'SELECT * FROM `ServiceControl`', 'SELECT `Component`,`Directory_`,`Attributes`,`Condition`,`KeyPath` FROM `Component`', 'SELECT `Property`,`Value` FROM `Property` WHERE `Property`=''Template''') {
  Write-Host "--- $q"
  try {
    $view = $db.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $db, @($q))
    $view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
    while ($true) {
      $rec = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
      if ($null -eq $rec) { break }
      $n = $rec.GetType().InvokeMember('FieldCount', 'GetProperty', $null, $rec, $null)
      Write-Host (($(1..$n | ForEach-Object { $rec.GetType().InvokeMember('StringData', 'GetProperty', $null, $rec, @($_)) })) -join ' | ')
    }
    $view.GetType().InvokeMember('Close', 'InvokeMethod', $null, $view, $null) | Out-Null
  } catch { Write-Host "(no such table: $($_.Exception.Message))" }
}
