<#
.SYNOPSIS
    Unit test for scripts/StateBackend.ps1 (OP-2a).
.DESCRIPTION
    Pure: no network, no terraform, no AWS CLI, no credentials. It only calls
    Get-StateBackendArgs with different environment values, then restores them.
    CI's infra job runs it; locally: pwsh -NoProfile -File .\tests\Test-StateBackend.ps1
#>
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path (Join-Path (Join-Path $PSScriptRoot '..') 'scripts') 'StateBackend.ps1')

$savedBucket = $env:LODESTAR_TF_STATE_BUCKET
$savedRegion = $env:LODESTAR_TF_STATE_REGION
$script:failures = 0

function Set-StateVars($Bucket, $Region) {
    if ($null -eq $Bucket) { Remove-Item Env:LODESTAR_TF_STATE_BUCKET -ErrorAction SilentlyContinue } else { $env:LODESTAR_TF_STATE_BUCKET = $Bucket }
    if ($null -eq $Region) { Remove-Item Env:LODESTAR_TF_STATE_REGION -ErrorAction SilentlyContinue } else { $env:LODESTAR_TF_STATE_REGION = $Region }
}

function Assert-Refused([string]$Name, $Bucket, $Region, [string]$Mentions) {
    Set-StateVars $Bucket $Region
    try {
        $null = Get-StateBackendArgs
        Write-Host "FAIL $Name : no refusal"
        $script:failures++
    } catch {
        $message = $_.Exception.Message
        if ($message -notmatch [regex]::Escape($Mentions)) {
            Write-Host "FAIL $Name : the refusal doesn't name $Mentions"
            $script:failures++
        } elseif ($Bucket -and $message.Contains([string]$Bucket)) {
            Write-Host "FAIL $Name : the refusal echoes the configured value"
            $script:failures++
        } else {
            Write-Host "ok   $Name"
        }
    }
}

try {
    Assert-Refused 'both unset' $null $null 'LODESTAR_TF_STATE_BUCKET'
    Assert-Refused 'bucket unset' $null 'us-east-1' 'LODESTAR_TF_STATE_BUCKET'
    Assert-Refused 'bucket blank' '   ' 'us-east-1' 'LODESTAR_TF_STATE_BUCKET'
    Assert-Refused 'region unset' 'lodestar-state-bucket' $null 'LODESTAR_TF_STATE_REGION'
    Assert-Refused 'bucket with an underscore' 'My_Bucket' 'us-east-1' 'LODESTAR_TF_STATE_BUCKET'
    Assert-Refused 'bucket with a comma' 'a,b-bucket' 'us-east-1' 'LODESTAR_TF_STATE_BUCKET'
    Assert-Refused 'bucket with an equals sign' 'bucket=other' 'us-east-1' 'LODESTAR_TF_STATE_BUCKET'
    Assert-Refused 'region without dashes' 'lodestar-state-bucket' 'useast1' 'LODESTAR_TF_STATE_REGION'
    Assert-Refused 'region with an argument' 'lodestar-state-bucket' 'us-east-1 -reconfigure' 'LODESTAR_TF_STATE_REGION'

    Set-StateVars 'lodestar-state-bucket' 'us-east-1'
    $result = @(Get-StateBackendArgs)
    $expected = @('-backend-config=bucket=lodestar-state-bucket', '-backend-config=key=terraform/lodestar.tfstate', '-backend-config=region=us-east-1')
    if ($result.Count -ne 3 -or (Compare-Object $result $expected -SyncWindow 0)) {
        Write-Host 'FAIL valid values: wrong -backend-config arguments'
        $script:failures++
    } else {
        Write-Host 'ok   valid values give the three -backend-config arguments with the fixed key'
    }
} finally {
    Set-StateVars $savedBucket $savedRegion
}

if ($script:failures -gt 0) {
    Write-Host "$($script:failures) failed"
    exit 1
}
Write-Host 'all passed'
