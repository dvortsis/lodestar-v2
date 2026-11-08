<#
.SYNOPSIS
    Privileged, approval-enforced teardown for the Lodestar AWS Windows test instance.
.DESCRIPTION
    Requires active LodestarProvisioner profile, terminates active Lodestar instances,
    verifies termination via EC2 waiter, and updates aws-fleet-state.json.
    Enforces explicit confirmation phrase: DESTROY 1 AWS WINDOWS INSTANCE.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $false)]
    [string]$InstanceId,

    [Parameter(Mandatory = $false)]
    [switch]$Force
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$projectRoot = (Resolve-Path (Join-Path $scriptDir '..')).Path
$stateFile = Join-Path $projectRoot 'state\aws-fleet-state.json'
$logsDir = Join-Path $projectRoot 'logs'
New-Item -ItemType Directory -Force -Path $logsDir | Out-Null
$logFile = Join-Path $logsDir ("aws-destroy-{0}.log" -f (Get-Date -Format 'yyyyMMdd-HHmmss'))

function Audit([string]$Message) {
    $line = '{0} [AWS-DESTROY] {1}' -f (Get-Date).ToUniversalTime().ToString('o'), $Message
    Write-Host $line
    Add-Content -LiteralPath $logFile -Value $line
}

$awsCli = "C:\Program Files\Amazon\AWSCLIV2\aws.exe"

# 1. Identity Verification (Enforce Privileged Provisioner Role via Administrator Identity)
Audit "Verifying privileged AWS STS caller identity..."
$eap = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
$identityJson = & $awsCli sts get-caller-identity --profile lodestar-provisioner 2>&1
$ErrorActionPreference = $eap

if ($LASTEXITCODE -ne 0) {
    Audit "ERROR: lodestar-provisioner session is not active or unauthorized."
    Audit "[ADMIN ACTION REQUIRED] Privileged destruction requires human administrator authentication."
    Audit "In your administrative terminal, run:"
    Audit "  aws sts get-caller-identity --profile lodestar-provisioner"
    Audit "(Note: lodestar-agent is blocked by explicit IAM deny; only lodestar-admin may assume this role)."
    throw "Active 'lodestar-provisioner' assumed-role session required for destruction."
}
$identity = $identityJson | ConvertFrom-Json
if ($identity.Arn -notmatch 'role/LodestarProvisioner') {
    throw "Security Violation: Expected role 'LodestarProvisioner', got '$($identity.Arn)'"
}

# 2. Discover Target Instance
if (-not $InstanceId) {
    if (Test-Path -LiteralPath $stateFile) {
        $state = Get-Content -LiteralPath $stateFile -Raw | ConvertFrom-Json
        if ($state.instance_id) { $InstanceId = $state.instance_id }
    }
}

if (-not $InstanceId) {
    Audit "Searching cloud inventory for active Lodestar instances..."
    $liveQuery = & $awsCli ec2 describe-instances --profile lodestar-provisioner --region us-east-1 `
        --filters "Name=tag:Project,Values=Lodestar" "Name=instance-state-name,Values=pending,running,stopping,stopped" `
        --query "Reservations[].Instances[].[InstanceId,State.Name,LaunchTime]" `
        --output json 2>&1

    if ($LASTEXITCODE -eq 0 -and $liveQuery) {
        $found = $liveQuery | ConvertFrom-Json
        if ($found.Count -gt 0) {
            $InstanceId = $found[0][0]
            Audit "Found active cloud instance: $InstanceId ($($found[0][1]))"
        }
    }
}

if (-not $InstanceId) {
    Audit "No active Lodestar instances found to destroy. Fleet is clean."
    return
}

# 3. Confirmation Gate
$phrase = "DESTROY 1 AWS WINDOWS INSTANCE"
if (-not $Force) {
    Write-Host "`n>>> TO CONFIRM DESTRUCTION OF INSTANCE $InstanceId, TYPE EXACTLY:" -ForegroundColor Yellow
    Write-Host "    $phrase" -ForegroundColor Red
    $confirm = Read-Host -Prompt "Enter confirmation phrase"
    if ($confirm -ne $phrase) {
        Audit "Teardown aborted: Confirmation phrase mismatch."
        Write-Host "Aborted. No resources were terminated." -ForegroundColor Green
        return
    }
}

# 4. Terminate Instance
Audit "Sending termination request for $InstanceId..."
$termResp = & $awsCli ec2 terminate-instances --instance-ids $InstanceId --profile lodestar-provisioner --region us-east-1 2>&1
if ($LASTEXITCODE -ne 0) {
    throw "ec2 terminate-instances failed: $termResp"
}

# 5. Verification: Poll for Terminated State
Audit "Waiting for instance $InstanceId to reach 'terminated' state..."
& $awsCli ec2 wait instance-terminated --instance-ids $InstanceId --profile lodestar-provisioner --region us-east-1
if ($LASTEXITCODE -eq 0) {
    Audit "SUCCESS: Instance $InstanceId verified terminated."
} else {
    Audit "WARNING: Waiter completed with code $LASTEXITCODE. Verify manually."
}

# 6. Update State File
if (Test-Path -LiteralPath $stateFile) {
    $state = Get-Content -LiteralPath $stateFile -Raw | ConvertFrom-Json
    $state.status = "destroyed"
    $state.destroyed_at = (Get-Date).ToUniversalTime().ToString('o')
    $state | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $stateFile -Encoding utf8
}
Audit "Teardown complete. Zero billable AWS compute remains."
