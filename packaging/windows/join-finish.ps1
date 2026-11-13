<#
.SYNOPSIS
  Undoes or keeps a worker join when the MSI install that made it rolls back or finishes
  (join.wxs: JoinWorkerRollback, JoinWorkerCommit; docs/decisions/2026-11-13-X-wk18w-rollback.md).
  Run as SYSTEM. Never sees the join token.

  join-worker.ps1 writes <data>\cluster\joined-by-setup right after a join that this setup
  made (never when the folder had already joined). That marker is the only record of what this
  install created:
    -Step rollback  the install is being undone: if the marker is there, removes the worker's
                    credentials, its saved coordinator address, its lock file and the marker,
                    so the next setup can join again. Nothing else is touched: secrets.key is
                    kept (it seals nothing once the credentials are gone, and a new join reuses
                    it), and so is anything a worker run may have left.
    -Step commit    the install finished: removes the marker, so a later install's rollback
                    never removes this join's files.
  Without the marker, both do nothing. Nothing is followed through a link or junction: a
  folder or file that is one is left alone. Always exits 0: a rollback or commit step must not
  stop Windows Installer (the action is Return="ignore" too).
#>
param(
  [Parameter(Mandatory)][ValidateSet('rollback', 'commit')][string]$Step,
  [Parameter(Mandatory)][string]$DataDir
)
$ErrorActionPreference = 'Stop'

function Say($msg) { Write-Host ("lodestar join ${Step}: " + (($msg -replace '[^\x20-\x7E]', '?') -replace '::', ': :')) }

# A real folder or file: there, and not a link or junction.
function Test-Plain([string]$path) {
  if (-not (Test-Path -LiteralPath $path)) { return $false }
  -not ((Get-Item -LiteralPath $path -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)
}

try {
  $data = [IO.Path]::GetFullPath($DataDir).TrimEnd('\')
  $cluster = Join-Path $data 'cluster'
  $marker = Join-Path $cluster 'joined-by-setup'
  if (-not ((Test-Plain $data) -and (Test-Plain $cluster) -and (Test-Plain $marker))) {
    Say 'nothing to do (this setup made no join, or the folder is a link)'
    exit 0
  }
  $names = if ($Step -eq 'commit') { @('joined-by-setup') } else {
    # The marker goes last, so a step stopped part way still finds it next time.
    @('credentials', 'credentials.tmp', 'coordinator', 'coordinator.tmp', 'worker.lock', 'joined-by-setup')
  }
  foreach ($name in $names) {
    $path = Join-Path $cluster $name
    if (-not (Test-Path -LiteralPath $path)) { continue }
    $item = Get-Item -LiteralPath $path -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
      Say "left $path alone: it is not a plain file"
      continue
    }
    [IO.File]::Delete($path)
    Say "removed $path"
  }
} catch {
  Say "stopped: $($_.Exception.Message)"
}
exit 0
