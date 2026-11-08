<#
.SYNOPSIS
    The shared Terraform state backend (S3) for the scripts that run terraform init.
    Dot-source it: . (Join-Path $PSScriptRoot 'StateBackend.ps1')
.DESCRIPTION
    These scripts and the fleet control plane (fleet.yml) share one Terraform state:
    two states for the same Linodes could recreate or orphan machines (#625). So init
    always uses the fleet's S3 state bucket, with the same fixed key, and refuses to run
    when the bucket or region isn't set. It never falls back to local state.
    Set, in the operator's own PowerShell session:
      LODESTAR_TF_STATE_BUCKET  the fleet-control stack's StateBucketName output
      LODESTAR_TF_STATE_REGION  that stack's region, for example us-east-1
    plus AWS credentials (for example AWS_PROFILE) that may use that bucket.
#>

function Get-StateBackendArgs {
    $key = 'terraform/lodestar.tfstate'  # the same key fleet.yml uses (OP-2a)
    $bucket = $env:LODESTAR_TF_STATE_BUCKET
    $region = $env:LODESTAR_TF_STATE_REGION
    if ([string]::IsNullOrWhiteSpace($bucket) -or $bucket -cnotmatch '^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$') {
        throw 'Set LODESTAR_TF_STATE_BUCKET to the fleet state bucket (the fleet-control stack''s StateBucketName output). Terraform never falls back to local state. Nothing was run.'
    }
    if ([string]::IsNullOrWhiteSpace($region) -or $region -cnotmatch '^[a-z]{2}(-[a-z]+)+-[0-9]$') {
        throw 'Set LODESTAR_TF_STATE_REGION to the state bucket''s AWS region, for example us-east-1. Nothing was run.'
    }
    return @("-backend-config=bucket=$bucket", "-backend-config=key=$key", "-backend-config=region=$region")
}
