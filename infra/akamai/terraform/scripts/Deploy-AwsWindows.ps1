<#
.SYNOPSIS
    Privileged, approval-enforced provisioner for the Lodestar AWS Windows test instance.
.DESCRIPTION
    Requires active LodestarProvisioner profile, validates expiration policy, tracks state
    in aws-fleet-state.json, prevents duplicate/orphaned instances, and enforces explicit
    human approval phrase: DEPLOY 1 AWS WINDOWS INSTANCE.

    Account-specific resource IDs are never stored in this public repository. Pass them as
    parameters, or set them in the local environment before running:
      LODESTAR_AWS_ACCOUNT_ID        12-digit AWS account ID
      LODESTAR_AWS_AMI_ID            approved Windows Server base AMI (ami-...)
      LODESTAR_AWS_SUBNET_ID         approved subnet (subnet-...)
      LODESTAR_AWS_SECURITY_GROUP_ID approved security group (sg-...)
      LODESTAR_AWS_INSTANCE_PROFILE  name of the SSM instance profile
    There are no defaults: the script refuses to run without all of them.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $false)]
    [string]$ExpiresAt,

    [Parameter(Mandatory = $false)]
    [int]$Hours = 48,

    [Parameter(Mandatory = $false)]
    [switch]$DryRun,

    [Parameter(Mandatory = $false)]
    [string]$AmiId = $env:LODESTAR_AWS_AMI_ID,

    [Parameter(Mandatory = $false)]
    [string]$SubnetId = $env:LODESTAR_AWS_SUBNET_ID,

    [Parameter(Mandatory = $false)]
    [string]$SecurityGroupId = $env:LODESTAR_AWS_SECURITY_GROUP_ID,

    [Parameter(Mandatory = $false)]
    [string]$InstanceProfileName = $env:LODESTAR_AWS_INSTANCE_PROFILE
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$projectRoot = (Resolve-Path (Join-Path $scriptDir '..')).Path
$stateDir = Join-Path $projectRoot 'state'
New-Item -ItemType Directory -Force -Path $stateDir | Out-Null
$stateFile = Join-Path $stateDir 'aws-fleet-state.json'
$logsDir = Join-Path $projectRoot 'logs'
New-Item -ItemType Directory -Force -Path $logsDir | Out-Null
$logFile = Join-Path $logsDir ("aws-deploy-{0}.log" -f (Get-Date -Format 'yyyyMMdd-HHmmss'))

function Audit([string]$Message) {
    $line = '{0} [AWS-DEPLOY] {1}' -f (Get-Date).ToUniversalTime().ToString('o'), $Message
    Write-Host $line
    Add-Content -LiteralPath $logFile -Value $line
}

# Import expiration validator
. (Join-Path $scriptDir 'Test-ExpirationPolicy.ps1')

$awsCli = "C:\Program Files\Amazon\AWSCLIV2\aws.exe"
if (-not (Test-Path -LiteralPath $awsCli)) {
    $cmd = Get-Command aws -ErrorAction SilentlyContinue
    if ($cmd) { $awsCli = $cmd.Source } else { throw "AWS CLI v2 executable not found." }
}

# 0. Validate account-specific parameters before any AWS call (no defaults exist).
function Assert-Id([string]$Name, [string]$Value, [string]$Pattern) {
    if ([string]::IsNullOrWhiteSpace($Value) -or $Value -cnotmatch $Pattern) {
        throw "Missing or invalid -$Name. Pass it as a parameter or set the matching LODESTAR_AWS_* environment variable. No deployment performed."
    }
}
Assert-Id 'AmiId' $AmiId '^ami-[0-9a-f]{8,17}$'
Assert-Id 'SubnetId' $SubnetId '^subnet-[0-9a-f]{8,17}$'
Assert-Id 'SecurityGroupId' $SecurityGroupId '^sg-[0-9a-f]{8,17}$'
Assert-Id 'InstanceProfileName' $InstanceProfileName '^[A-Za-z0-9+=,.@_-]{1,128}$'

# 1. Identity Verification (Enforce Privileged Provisioner Role via Administrator Identity)
Audit "Verifying privileged AWS STS caller identity..."
$eap = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
$identityJson = & $awsCli sts get-caller-identity --profile lodestar-provisioner 2>&1
$ErrorActionPreference = $eap

if ($LASTEXITCODE -ne 0) {
    Audit "ERROR: lodestar-provisioner session is not active or unauthorized."
    Audit "[ADMIN ACTION REQUIRED] Privileged provisioning requires human administrator authentication."
    Audit "In your administrative terminal, run:"
    Audit "  aws sts get-caller-identity --profile lodestar-provisioner"
    Audit "(Note: lodestar-agent is blocked by explicit IAM deny; only lodestar-admin may assume this role)."
    throw "Active 'lodestar-provisioner' assumed-role session required for deployment."
}
$identity = $identityJson | ConvertFrom-Json
if ($identity.Arn -notmatch 'role/LodestarProvisioner') {
    throw "Security Violation: Expected role 'LodestarProvisioner', got '$($identity.Arn)'"
}
Audit "Authenticated as $($identity.Arn)"

# 2. Expiration Tag Calculation & Strict Validation
if (-not $ExpiresAt) {
    $ExpiresAt = (Get-Date).ToUniversalTime().AddHours($Hours).ToString("yyyy-MM-ddTHH:mm:ssZ")
}
$expCheck = Assert-LodestarExpiration -ExpiresAt $ExpiresAt -MaxHours 48
Audit "Validated lease duration: $($expCheck.LeaseHours) hours (Expires: $($expCheck.IsoString))"

# 3. State Tracking & Duplicate/Orphan Detection
Audit "Checking persistent state in $stateFile..."
$existingState = $null
if (Test-Path -LiteralPath $stateFile) {
    try {
        $existingState = Get-Content -LiteralPath $stateFile -Raw | ConvertFrom-Json
    } catch {}
}

Audit "Checking live AWS cloud inventory for active Lodestar instances..."
$liveQuery = & $awsCli ec2 describe-instances --profile lodestar-provisioner --region us-east-1 `
    --filters "Name=tag:Project,Values=Lodestar" "Name=instance-state-name,Values=pending,running,stopping,stopped" `
    --query "Reservations[].Instances[].[InstanceId,State.Name,LaunchTime,Tags[?Key=='ExpiresAt'].Value | [0]]" `
    --output json 2>&1

$activeInstances = @()
if ($LASTEXITCODE -eq 0) {
    $parsedQuery = $liveQuery | ConvertFrom-Json
    if ($parsedQuery -and $parsedQuery.Count -gt 0) {
        $activeInstances = $parsedQuery
    }
}

if ($activeInstances.Count -gt 0) {
    Audit "WARNING: Active Lodestar instance(s) already exist in AWS!"
    foreach ($inst in $activeInstances) {
        Write-Host "  Found: InstanceId=$($inst[0]) State=$($inst[1]) LaunchTime=$($inst[2]) ExpiresAt=$($inst[3])" -ForegroundColor Yellow
    }
    throw "Deployment halted to prevent duplicate billable resources or partial deployment conflict. Terminate existing instance or recover state."
}

# 4. Deployment Parameters
$amiId = $AmiId
$instanceType = "c6a.xlarge"
$subnetId = $SubnetId
$securityGroupId = $SecurityGroupId
$expectedAccountId = $env:LODESTAR_AWS_ACCOUNT_ID
if ($expectedAccountId -cnotmatch '^[0-9]{12}$') { throw 'Set LODESTAR_AWS_ACCOUNT_ID to the approved 12-digit AWS account ID before deployment.' }
$identityJson = & $awsCli sts get-caller-identity --profile lodestar-admin --region us-east-1 --output json --no-cli-pager
if ($LASTEXITCODE -ne 0) { throw 'Cannot validate lodestar-admin identity. No deployment performed.' }
$identity = ($identityJson -join "`n") | ConvertFrom-Json
if ($identity.Account -cne $expectedAccountId -or $identity.Arn -cne "arn:aws:iam::${expectedAccountId}:user/lodestar-admin") { throw "Unexpected administrator identity or account. No deployment performed." }
$instanceProfileArn = "arn:aws:iam::${expectedAccountId}:instance-profile/$InstanceProfileName"

Write-Host @"
================================================================================
                    LODESTAR AWS WINDOWS PROVISIONING PLAN
================================================================================
Target Region:         us-east-1
Instance Type:         $instanceType (4 vCPU, 8 GB RAM)
AMI ID:                $amiId (must be an approved Windows Server base AMI)
Subnet:                $subnetId
Security Group:        $securityGroupId (Deny all inbound, SSM only)
Instance Profile:      $instanceProfileArn
Hourly Rate:           `$0.1530 / hour
$($expCheck.LeaseHours)-Hour Estimated Cost:  `$$([math]::Round(0.1530 * $expCheck.LeaseHours, 3))
Mandatory Tags:        Project=Lodestar, Environment=Test, ManagedBy=Terraform,
                       ExpiresAt=$($expCheck.IsoString)
================================================================================
"@ -ForegroundColor Cyan

if ($DryRun) {
    Audit "DRY-RUN REQUESTED: Executing ec2 run-instances --dry-run..."
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $dryRunOutput = & $awsCli ec2 run-instances --dry-run `
            --image-id $amiId `
            --instance-type $instanceType `
            --subnet-id $subnetId `
            --security-group-ids $securityGroupId `
            --metadata-options "HttpTokens=required" `
            --block-device-mappings '[{\"DeviceName\":\"/dev/sda1\",\"Ebs\":{\"VolumeSize\":50,\"Encrypted\":true}}]' `
            --tag-specifications "ResourceType=instance,Tags=[{Key=Project,Value=Lodestar},{Key=Environment,Value=Test},{Key=ManagedBy,Value=Terraform},{Key=ExpiresAt,Value=$($expCheck.IsoString)}]" `
                                 "ResourceType=volume,Tags=[{Key=Project,Value=Lodestar},{Key=Environment,Value=Test},{Key=ManagedBy,Value=Terraform},{Key=ExpiresAt,Value=$($expCheck.IsoString)}]" `
            --iam-instance-profile "Arn=$instanceProfileArn" `
            --profile lodestar-provisioner --region us-east-1 2>&1
    } finally {
        $ErrorActionPreference = $prevEap
    }

    if ($dryRunOutput -match "DryRunOperation") {
        Audit "PASS: DryRunOperation succeeded. Cloud policy and parameters fully compliant."
        return
    } else {
        throw "DryRun failed: $dryRunOutput"
    }
}

# 5. Mandatory Human Confirmation Gate
$phrase = "DEPLOY 1 AWS WINDOWS INSTANCE"
Write-Host "`n>>> TO CONFIRM BILLABLE CREATION, TYPE EXACTLY:" -ForegroundColor Yellow
Write-Host "    $phrase" -ForegroundColor Green
$confirm = Read-Host -Prompt "Enter confirmation phrase"

if ($confirm -cne $phrase) {
    Audit "Deployment aborted: Confirmation phrase mismatch ('$confirm' != '$phrase')."
    Write-Host "Aborted. No resources were created." -ForegroundColor Red
    return
}

# 6. Execute Provisioning
Audit "Executing live EC2 RunInstances..."
# Update state file to provisioning
$stateObj = @{
    status       = "provisioning"
    timestamp    = (Get-Date).ToUniversalTime().ToString('o')
    expires_at   = $expCheck.IsoString
    lease_hours  = $expCheck.LeaseHours
    region       = "us-east-1"
    instance_id  = $null
}
$stateObj | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $stateFile -Encoding utf8

try {
    $runResp = & $awsCli ec2 run-instances `
        --image-id $amiId `
        --instance-type $instanceType `
        --subnet-id $subnetId `
        --security-group-ids $securityGroupId `
        --metadata-options "HttpTokens=required" `
        --block-device-mappings '[{\"DeviceName\":\"/dev/sda1\",\"Ebs\":{\"VolumeSize\":50,\"Encrypted\":true}}]' `
        --tag-specifications "ResourceType=instance,Tags=[{Key=Project,Value=Lodestar},{Key=Environment,Value=Test},{Key=ManagedBy,Value=Terraform},{Key=ExpiresAt,Value=$($expCheck.IsoString)}]" `
                             "ResourceType=volume,Tags=[{Key=Project,Value=Lodestar},{Key=Environment,Value=Test},{Key=ManagedBy,Value=Terraform},{Key=ExpiresAt,Value=$($expCheck.IsoString)}]" `
        --iam-instance-profile "Arn=$instanceProfileArn" `
        --profile lodestar-provisioner --region us-east-1 `
        --output json 2>&1

    if ($LASTEXITCODE -ne 0) {
        throw "ec2 run-instances failed with code ${LASTEXITCODE}: $runResp"
    }

    $runJson = $runResp | ConvertFrom-Json
    $newInstance = $runJson.Instances[0]
    $instId = $newInstance.InstanceId

    $stateObj.status = "running"
    $stateObj.instance_id = $instId
    $stateObj.launch_time = $newInstance.LaunchTime
    $stateObj.private_ip = $newInstance.PrivateIpAddress
    $stateObj | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $stateFile -Encoding utf8

    Audit "SUCCESS: Instance $instId provisioned and recorded in state."
    Write-Host "Instance provisioned: $instId" -ForegroundColor Green
} catch {
    $stateObj.status = "failed"
    $stateObj.error = $_.Exception.Message
    $stateObj | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $stateFile -Encoding utf8
    Audit "PROVISIONING FAILED: $($_.Exception.Message)"
    throw
}
