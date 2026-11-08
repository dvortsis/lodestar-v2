<#
.SYNOPSIS
    Privileged, approval-enforced teardown for the Lodestar AWS Windows test instance.
.DESCRIPTION
    Human-run only, never by an agent. Requires an active LodestarProvisioner session,
    checks that the target is a live instance tagged Project=Lodestar, asks the human to
    type DESTROY 1 AWS WINDOWS INSTANCE, terminates it, waits for the 'terminated' state
    and only then updates state\aws-fleet-state.json.
    There is deliberately no switch that skips the typed confirmation.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $false)]
    [string]$InstanceId
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$projectRoot = (Resolve-Path (Join-Path $scriptDir '..')).Path
$stateFile = Join-Path $projectRoot 'state\aws-fleet-state.json'
$logsDir = Join-Path $projectRoot 'logs'
New-Item -ItemType Directory -Force -Path $logsDir | Out-Null
$logFile = Join-Path $logsDir ("aws-destroy-{0}.log" -f (Get-Date).ToUniversalTime().ToString('yyyyMMdd-HHmmssZ'))

function Audit([string]$Message) {
    $line = '{0} [AWS-DESTROY] {1}' -f (Get-Date).ToUniversalTime().ToString('o'), $Message
    Write-Host $line
    Add-Content -LiteralPath $logFile -Value $line
}

. (Join-Path $scriptDir 'AwsCli.ps1')
. (Join-Path $scriptDir 'Interactive.ps1')
$region = 'us-east-1'
$profileArgs = @('--profile', 'lodestar-provisioner', '--region', $region)

# 1. Identity: an active LodestarProvisioner session (the administrator's MFA session).
Audit "Verifying privileged AWS STS caller identity..."
$r = Invoke-AwsCli @('sts', 'get-caller-identity', '--profile', 'lodestar-provisioner', '--output', 'json')
if ($r.ExitCode -ne 0) {
    Audit "ERROR: lodestar-provisioner session is not active or unauthorized."
    Audit "[ADMIN ACTION REQUIRED] Privileged destruction requires human administrator authentication."
    Audit "In your administrative terminal, run:"
    Audit "  aws sts get-caller-identity --profile lodestar-provisioner"
    Audit "(Note: lodestar-agent is blocked by explicit IAM deny; only lodestar-admin may assume this role)."
    throw "Active 'lodestar-provisioner' assumed-role session required for destruction."
}
$identity = $r.Out | ConvertFrom-Json
if ($identity.Arn -notmatch ':assumed-role/LodestarProvisioner/') {
    throw "Security Violation: Expected role 'LodestarProvisioner', got '$($identity.Arn)'"
}

# 2. Find the target. The state file is used only while it says an instance may be live.
if (-not $InstanceId -and (Test-Path -LiteralPath $stateFile)) {
    $state = Get-Content -LiteralPath $stateFile -Raw | ConvertFrom-Json
    $status = [string]$state.status
    if ($state.instance_id -and $status -in @('running', 'provisioning')) {
        $InstanceId = [string]$state.instance_id
        Audit "Using instance $InstanceId from the state file (status '$status')."
    }
}

if (-not $InstanceId) {
    Audit "Searching cloud inventory for active Lodestar instances..."
    $r = Invoke-AwsCli (@('ec2', 'describe-instances') + $profileArgs + @(
        '--filters', 'Name=tag:Project,Values=Lodestar', 'Name=instance-state-name,Values=pending,running,stopping,stopped',
        '--query', 'Reservations[].Instances[].[InstanceId,State.Name]', '--output', 'text'))
    if ($r.ExitCode -ne 0) {
        throw "Could not list EC2 instances (exit $($r.ExitCode)). Do not assume the fleet is clean. Nothing terminated."
    }
    $found = ConvertFrom-AwsTextRows $r.Out
    if ($found.Count -gt 1) {
        foreach ($f in $found) { Audit "  Found: $($f[0]) ($($f[1]))" }
        throw "More than one active Lodestar instance found. Re-run with -InstanceId to choose exactly one. Nothing terminated."
    }
    if ($found.Count -eq 1) {
        $InstanceId = $found[0][0]
        Audit "Found active cloud instance: $InstanceId ($($found[0][1]))"
    }
}

if (-not $InstanceId) {
    Audit "No active Lodestar instances found to destroy. Fleet is clean."
    return
}

if ($InstanceId -cnotmatch '^i-[0-9a-f]{8,17}$') {
    throw "Invalid instance ID '$InstanceId'. Nothing terminated."
}

# 3. Check the target is a live Lodestar instance before asking.
$r = Invoke-AwsCli (@('ec2', 'describe-instances', '--instance-ids', $InstanceId) + $profileArgs + @(
    '--query', "Reservations[].Instances[].[State.Name,Tags[?Key=='Project'].Value | [0]]", '--output', 'text'))
if ($r.ExitCode -ne 0) {
    throw "Could not describe $InstanceId (exit $($r.ExitCode)). Nothing terminated."
}
$rows = ConvertFrom-AwsTextRows $r.Out
if ($rows.Count -ne 1) { throw "Instance $InstanceId not found. Nothing terminated." }
$targetState = $rows[0][0]
$targetProject = if ($rows[0].Count -gt 1) { $rows[0][1] } else { '' }
if ($targetProject -cne 'Lodestar') {
    throw "Instance $InstanceId is not tagged Project=Lodestar. Nothing terminated."
}
if ($targetState -in @('shutting-down', 'terminated')) {
    Audit "Instance $InstanceId is already $targetState. Nothing to terminate."
    return
}

# 4. Confirmation gate (always required; there is no bypass; piped input is refused)
Assert-InteractiveConfirmation
$phrase = "DESTROY 1 AWS WINDOWS INSTANCE"
Write-Host "`n>>> TO CONFIRM DESTRUCTION OF INSTANCE $InstanceId ($targetState), TYPE EXACTLY:" -ForegroundColor Yellow
Write-Host "    $phrase" -ForegroundColor Red
$confirm = Read-Host -Prompt "Enter confirmation phrase"
if ($confirm -cne $phrase) {
    Audit "Teardown aborted: Confirmation phrase mismatch."
    Write-Host "Aborted. No resources were terminated." -ForegroundColor Green
    return
}

# 5. Terminate
Audit "Sending termination request for $InstanceId..."
$r = Invoke-AwsCli (@('ec2', 'terminate-instances', '--instance-ids', $InstanceId) + $profileArgs)
if ($r.ExitCode -ne 0) {
    throw "ec2 terminate-instances failed (exit $($r.ExitCode)): $($r.Err)"
}

# 6. Verify: wait for 'terminated'. A waiter failure leaves the state file untouched.
Audit "Waiting for instance $InstanceId to reach 'terminated' state..."
$r = Invoke-AwsCli (@('ec2', 'wait', 'instance-terminated', '--instance-ids', $InstanceId) + $profileArgs)
if ($r.ExitCode -ne 0) {
    Audit "WARNING: Waiter ended with code $($r.ExitCode). Termination is NOT verified; state file left unchanged."
    throw "Termination of $InstanceId not verified. Check the EC2 console; the instance may still be billing."
}
Audit "SUCCESS: Instance $InstanceId verified terminated."

# 7. Update state
if (Test-Path -LiteralPath $stateFile) {
    $state = Get-Content -LiteralPath $stateFile -Raw | ConvertFrom-Json
    if ([string]$state.instance_id -ceq $InstanceId) {
        $state.status = "destroyed"
        $state | Add-Member -NotePropertyName destroyed_at -NotePropertyValue (Get-Date).ToUniversalTime().ToString('o') -Force
        $state | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $stateFile -Encoding utf8
    }
}
Audit "Teardown complete. Instance $InstanceId is terminated."
