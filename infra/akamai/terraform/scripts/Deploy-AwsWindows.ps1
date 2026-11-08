<#
.SYNOPSIS
    Privileged, approval-enforced provisioner for the Lodestar AWS Windows test instance.
.DESCRIPTION
    Human-run only, never by an agent. Requires an active LodestarProvisioner session,
    validates the expiration policy, tracks state in state\aws-fleet-state.json, refuses to
    run while any Lodestar instance is active, and asks the human to type the phrase
    DEPLOY 1 AWS WINDOWS INSTANCE before anything billable happens.

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
$logFile = Join-Path $logsDir ("aws-deploy-{0}.log" -f (Get-Date).ToUniversalTime().ToString('yyyyMMdd-HHmmssZ'))

function Audit([string]$Message) {
    $line = '{0} [AWS-DEPLOY] {1}' -f (Get-Date).ToUniversalTime().ToString('o'), $Message
    Write-Host $line
    Add-Content -LiteralPath $logFile -Value $line
}

. (Join-Path $scriptDir 'Test-ExpirationPolicy.ps1')
. (Join-Path $scriptDir 'AwsCli.ps1')

$region = 'us-east-1'
$instanceType = 'c6a.xlarge'

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

# 1. Identity: an active LodestarProvisioner session (the administrator's MFA session).
Audit "Verifying privileged AWS STS caller identity..."
$r = Invoke-AwsCli @('sts', 'get-caller-identity', '--profile', 'lodestar-provisioner', '--output', 'json')
if ($r.ExitCode -ne 0) {
    Audit "ERROR: lodestar-provisioner session is not active or unauthorized."
    Audit "[ADMIN ACTION REQUIRED] Privileged provisioning requires human administrator authentication."
    Audit "In your administrative terminal, run:"
    Audit "  aws sts get-caller-identity --profile lodestar-provisioner"
    Audit "(Note: lodestar-agent is blocked by explicit IAM deny; only lodestar-admin may assume this role)."
    throw "Active 'lodestar-provisioner' assumed-role session required for deployment."
}
$identity = $r.Out | ConvertFrom-Json
if ($identity.Arn -notmatch ':assumed-role/LodestarProvisioner/') {
    throw "Security Violation: Expected role 'LodestarProvisioner', got '$($identity.Arn)'"
}
Audit "Authenticated as $($identity.Arn)"

$expectedAccountId = $env:LODESTAR_AWS_ACCOUNT_ID
if ($expectedAccountId -cnotmatch '^[0-9]{12}$') { throw 'Set LODESTAR_AWS_ACCOUNT_ID to the approved 12-digit AWS account ID before deployment.' }
if ($identity.Account -cne $expectedAccountId) { throw 'Provisioner session is in an unexpected AWS account. No deployment performed.' }
$r = Invoke-AwsCli @('sts', 'get-caller-identity', '--profile', 'lodestar-admin', '--region', $region, '--output', 'json', '--no-cli-pager')
if ($r.ExitCode -ne 0) { throw 'Cannot validate lodestar-admin identity. No deployment performed.' }
$admin = $r.Out | ConvertFrom-Json
if ($admin.Account -cne $expectedAccountId -or $admin.Arn -cne "arn:aws:iam::${expectedAccountId}:user/lodestar-admin") { throw "Unexpected administrator identity or account. No deployment performed." }
$instanceProfileArn = "arn:aws:iam::${expectedAccountId}:instance-profile/$InstanceProfileName"

# 2. Expiration tag
if (-not $ExpiresAt) {
    $ExpiresAt = (Get-Date).ToUniversalTime().AddHours($Hours).ToString("yyyy-MM-ddTHH:mm:ssZ")
}
$expCheck = Assert-LodestarExpiration -ExpiresAt $ExpiresAt -MaxHours 48
Audit "Validated lease duration: $($expCheck.LeaseHours) hours (Expires: $($expCheck.IsoString))"

# 3. Refuse while any Lodestar instance is active. A failed inventory call stops the
#    deployment: it never counts as "no instances".
Audit "Checking live AWS cloud inventory for active Lodestar instances..."
$r = Invoke-AwsCli @('ec2', 'describe-instances', '--profile', 'lodestar-provisioner', '--region', $region,
    '--filters', 'Name=tag:Project,Values=Lodestar', 'Name=instance-state-name,Values=pending,running,stopping,stopped',
    '--query', 'Reservations[].Instances[].[InstanceId,State.Name,LaunchTime]', '--output', 'text')
if ($r.ExitCode -ne 0) {
    throw "Could not list EC2 instances (exit $($r.ExitCode)). No deployment performed."
}
$active = ConvertFrom-AwsTextRows $r.Out
if ($active.Count -gt 0) {
    Audit "WARNING: Active Lodestar instance(s) already exist in AWS!"
    foreach ($inst in $active) {
        Write-Host "  Found: InstanceId=$($inst[0]) State=$($inst[1]) LaunchTime=$($inst[2])" -ForegroundColor Yellow
    }
    throw "Deployment halted to prevent duplicate billable resources or partial deployment conflict. Terminate existing instance or recover state."
}

Write-Host @"
================================================================================
                    LODESTAR AWS WINDOWS PROVISIONING PLAN
================================================================================
Target Region:         $region
Instance Type:         $instanceType (4 vCPU, 8 GB RAM)
AMI ID:                $AmiId (must be an approved Windows Server base AMI)
Subnet:                $SubnetId
Security Group:        $SecurityGroupId (Deny all inbound, SSM only)
Instance Profile:      $instanceProfileArn
Hourly Rate:           `$0.1530 / hour
$($expCheck.LeaseHours)-Hour Estimated Cost:  `$$([math]::Round(0.1530 * $expCheck.LeaseHours, 3))
Mandatory Tags:        Project=Lodestar, Environment=Test, ManagedBy=Terraform,
                       ExpiresAt=$($expCheck.IsoString)
================================================================================
"@ -ForegroundColor Cyan

# Shorthand syntax, not inline JSON: inline JSON needs different quoting in 5.1 and 7.
$tags = "{Key=Project,Value=Lodestar},{Key=Environment,Value=Test},{Key=ManagedBy,Value=Terraform},{Key=ExpiresAt,Value=$($expCheck.IsoString)}"
$runArgs = @('ec2', 'run-instances',
    '--image-id', $AmiId,
    '--instance-type', $instanceType,
    '--subnet-id', $SubnetId,
    '--security-group-ids', $SecurityGroupId,
    '--metadata-options', 'HttpTokens=required',
    '--block-device-mappings', 'DeviceName=/dev/sda1,Ebs={VolumeSize=50,Encrypted=true}',
    '--tag-specifications', "ResourceType=instance,Tags=[$tags]", "ResourceType=volume,Tags=[$tags]",
    '--iam-instance-profile', "Arn=$instanceProfileArn",
    '--profile', 'lodestar-provisioner', '--region', $region, '--output', 'json')

if ($DryRun) {
    Audit "DRY-RUN REQUESTED: Executing ec2 run-instances --dry-run..."
    $r = Invoke-AwsCli ($runArgs + '--dry-run')
    if ($r.Err -match 'DryRunOperation') {
        Audit "PASS: DryRunOperation succeeded. Cloud policy and parameters fully compliant."
        return
    }
    throw "DryRun failed (exit $($r.ExitCode)): $($r.Err)"
}

# 4. Mandatory human confirmation
$phrase = "DEPLOY 1 AWS WINDOWS INSTANCE"
Write-Host "`n>>> TO CONFIRM BILLABLE CREATION, TYPE EXACTLY:" -ForegroundColor Yellow
Write-Host "    $phrase" -ForegroundColor Green
$confirm = Read-Host -Prompt "Enter confirmation phrase"
if ($confirm -cne $phrase) {
    Audit "Deployment aborted: Confirmation phrase mismatch."
    Write-Host "Aborted. No resources were created." -ForegroundColor Red
    return
}

# 5. Provision. State is written before the call, so an interruption leaves a record.
Audit "Executing live EC2 RunInstances..."
$stateObj = [ordered]@{
    status      = "provisioning"
    timestamp   = (Get-Date).ToUniversalTime().ToString('o')
    expires_at  = $expCheck.IsoString
    lease_hours = $expCheck.LeaseHours
    region      = $region
    instance_id = $null
}
$stateObj | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $stateFile -Encoding utf8

$r = Invoke-AwsCli $runArgs
if ($r.ExitCode -ne 0) {
    $stateObj.status = "failed"
    $stateObj.error = "run-instances exit $($r.ExitCode)"
    $stateObj | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $stateFile -Encoding utf8
    Audit "PROVISIONING FAILED (exit $($r.ExitCode)): $($r.Err)"
    Audit "Run .\scripts\Deploy-AwsWindows.ps1 -DryRun to check for an instance that launched anyway."
    throw "ec2 run-instances failed with code $($r.ExitCode)."
}

try {
    $newInstance = ($r.Out | ConvertFrom-Json).Instances[0]
    $instId = [string]$newInstance.InstanceId
} catch {
    $instId = $null
}
if ($instId -cnotmatch '^i-[0-9a-f]{8,17}$') {
    # The call succeeded, so an instance may exist. The live duplicate check and the
    # destroy script's inventory search both find it by its Project=Lodestar tag.
    $stateObj.status = "unknown-after-launch"
    $stateObj | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $stateFile -Encoding utf8
    throw "run-instances succeeded but its output could not be read. An instance may be running: check with -DryRun and remove it with Destroy-AwsWindows.ps1."
}

$stateObj.status = "running"
$stateObj.instance_id = $instId
$stateObj.launch_time = [string]$newInstance.LaunchTime
$stateObj.private_ip = [string]$newInstance.PrivateIpAddress
$stateObj | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $stateFile -Encoding utf8
Audit "SUCCESS: Instance $instId provisioned and recorded in state."
Write-Host "Instance provisioned: $instId" -ForegroundColor Green
