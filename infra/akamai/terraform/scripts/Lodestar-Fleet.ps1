<#
.SYNOPSIS
    Unified multi-cloud control tool for the Lodestar fleet (AWS & Akamai).
.DESCRIPTION
    Provides standardized inspect, plan, deploy, status, reboot, remote access,
    reconciliation, and destroy commands across AWS (Windows Server) and Akamai (Linux test fleet).
    Requires explicit human approval for billable actions.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateSet('status', 'inspect', 'plan', 'cost', 'access', 'deploy', 'destroy', 'reconcile')]
    [string]$Action,

    [Parameter(Mandatory = $false)]
    [string]$Target,

    [Parameter(Mandatory = $false)]
    [int]$Hours = 48
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$projectRoot = (Resolve-Path (Join-Path $scriptDir '..')).Path
$logsDir = Join-Path $projectRoot 'logs'
New-Item -ItemType Directory -Force -Path $logsDir | Out-Null
$logFile = Join-Path $logsDir ("fleet-{0}.log" -f (Get-Date).ToUniversalTime().ToString('yyyyMMdd-HHmmssZ'))

function Audit([string]$Message) {
    $line = '{0} [FLEET] {1}' -f (Get-Date).ToUniversalTime().ToString('o'), $Message
    Write-Host $line
    Add-Content -LiteralPath $logFile -Value $line
}

. (Join-Path $scriptDir 'AwsCli.ps1')
$awsCli = $script:AwsCli

switch ($Action) {
    'inspect' {
        Audit "=== Multi-Cloud Identity & Permissions Inspection ==="
        Audit "1. AWS Cloud (Read-Only Observer: lodestar-observer):"
        $r = Invoke-AwsCli @('sts', 'get-caller-identity', '--profile', 'lodestar-observer', '--output', 'json')
        if ($r.ExitCode -eq 0) {
            Audit $r.Out.Trim()
        } else {
            Audit "Observer session not active. Run: aws sts get-caller-identity --profile lodestar-observer"
        }
        # The provisioner profile is deliberately not touched here: inspect is read-only and
        # agent-safe, and the provisioner session belongs to the human administrator.
        Audit "2. AWS Cloud (Privileged Provisioner): not checked by inspect. The administrator checks it with"
        Audit "   aws sts get-caller-identity --profile lodestar-provisioner"

        Audit "3. Akamai Cloud (Terraform Preflight & Provider):"
        Push-Location $projectRoot
        try {
            & terraform validate
            if ($LASTEXITCODE -eq 0) { Audit "Akamai Terraform configuration is valid." }
        } finally { Pop-Location }
    }

    'status' {
        Audit "=== Lodestar Fleet Status ==="
        Audit "[AWS] Checking Windows test instances in us-east-1 (via lodestar-observer)..."
        $r = Invoke-AwsCli @('ec2', 'describe-instances', '--profile', 'lodestar-observer', '--region', 'us-east-1',
            '--filters', 'Name=tag:Project,Values=Lodestar',
            '--query', "Reservations[].Instances[].[InstanceId, State.Name, InstanceType, LaunchTime, Tags[?Key=='ExpiresAt'].Value | [0]]",
            '--output', 'table')
        if ($r.ExitCode -eq 0) { Write-Host $r.Out } else { Audit "[AWS] Inventory unavailable (exit $($r.ExitCode)). Is the observer session active?" }

        Audit "[Akamai] Checking Linux test fleet state..."
        Push-Location $projectRoot
        try {
            $state = & terraform state list 2>&1
            if ($LASTEXITCODE -eq 0 -and $state) {
                Write-Host "Managed Terraform resources:`n$state"
                & terraform output
            } else {
                Audit "No managed Akamai resources in current state (fleet is idle)."
            }
        } finally { Pop-Location }
    }

    'cost' {
        Audit "=== Lodestar Compute Cost Projections ==="
        $aws48 = [math]::Round(0.3370 * $Hours, 3)
        $main48 = [math]::Round(0.0720 * $Hours, 3)
        $worker48 = [math]::Round(0.0075 * $Hours, 3)
        $pilotRate = [math]::Round(0.0720 + 0.0075, 4)
        $pilot48 = [math]::Round($pilotRate * $Hours, 3)
        $fullRate = [math]::Round(0.3370 + $pilotRate, 4)
        $full48 = [math]::Round($fullRate * $Hours, 3)

        Write-Host @"
--------------------------------------------------------------------------------
CLOUD    ROLE    INSTANCE TYPE   VCPU   RAM   HOURLY USD   ${Hours}-HOUR COST
--------------------------------------------------------------------------------
AWS      Windows c6a.xlarge      4      8 GB  `$0.3370      `$$aws48
Akamai   Main    g6-standard-4   4      8 GB  `$0.0720      `$$main48
Akamai   Worker  g6-nanode-1     1      1 GB  `$0.0075      `$$worker48
--------------------------------------------------------------------------------
AKAMAI PILOT TOTAL (1 Main + 1 Worker):       `$$pilotRate/hr   `$$pilot48
FULL MULTI-CLOUD FLEET (AWS + 2 Akamai):     `$$fullRate/hr   `$$full48
--------------------------------------------------------------------------------
Note: AWS rate is on-demand us-east-1 including the Windows licence (about `$0.153 compute + `$0.184 licence).
Excludes EBS storage, data transfer overage, storage IOPS, and VPN subscriptions.
Linode instances bill until deleted, even when shut down.
"@
    }

    'plan' {
        Audit "=== Multi-Cloud Dry-Run Planning ==="
        Audit "[AWS] Executing non-destructive EC2 RunInstances Dry-Run..."
        Push-Location $projectRoot
        try {
            # The same PowerShell edition that runs this script, from $PSHOME (not the
            # process path, which is powershell_ise.exe or an embedding host under ISE).
            $exe = if ($PSVersionTable.PSEdition -eq 'Core') { 'pwsh' } else { 'powershell.exe' }
            if ($IsLinux -or $IsMacOS) { $exe = 'pwsh' }
            $psHost = Join-Path $PSHOME $exe
            & $psHost -NoProfile -File (Join-Path $scriptDir 'Deploy-AwsWindows.ps1') -DryRun -Hours $Hours
        } finally { Pop-Location }

        Audit "[Akamai] Generating read-only Terraform execution plan..."
        Push-Location $projectRoot
        try {
            & terraform plan -input=false
        } finally { Pop-Location }
    }

    'access' {
        if (-not $Target) {
            Write-Host @"
Usage:
  .\scripts\Lodestar-Fleet.ps1 access -Target <AWS_INSTANCE_ID>    (Connects via AWS SSM Session Manager)
  .\scripts\Lodestar-Fleet.ps1 access -Target <AKAMAI_IP>          (Connects via SSH using lodestar key)
"@
            return
        }
        $ip = $null
        $isIpv4 = $Target -match '^(\d{1,3}\.){3}\d{1,3}$' -and [System.Net.IPAddress]::TryParse($Target, [ref]$ip) -and
                  $ip.AddressFamily -eq [System.Net.Sockets.AddressFamily]::InterNetwork
        if ($Target -cnotmatch '^i-[0-9a-f]{8,17}$' -and -not $isIpv4) {
            throw "Invalid -Target '$Target'. Use an EC2 instance ID (i-...) or an Akamai VM's IPv4 address."
        }
        if ($Target -cmatch '^i-[0-9a-f]{8,17}$') {
            Audit "Starting AWS Systems Manager session to $Target (human-run only: requires the administrator's MFA session)..."
            & $awsCli ssm start-session --target $Target --region us-east-1 --profile lodestar-provisioner
        } else {
            Audit "Connecting to Akamai Linux VM at $Target via SSH..."
            ssh -i "$HOME\.ssh\lodestar_akamai_ed25519" "lodestar@$Target"
        }
    }

    'deploy' {
        Audit "=== BILLABLE PROVISIONING REQUESTED ==="
        Audit "Deployment requires explicit human approval in an interactive terminal."
        Write-Host @"
--------------------------------------------------------------------------------
                    PRIVILEGED DEPLOYMENT COMMANDS
--------------------------------------------------------------------------------
1. To provision Akamai pilot Linux Linodes (1 Main + 1 Worker):
   .\scripts\Deploy.ps1
   (Enforces confirmation: DEPLOY <N> LINODES, where <N> is the planned count)

2. To provision AWS Windows Server 2022/2025 instance (c6a.xlarge):
   .\scripts\Deploy-AwsWindows.ps1 -Hours $Hours
   (Enforces administrator authentication, state check, expiration validation,
    and confirmation: DEPLOY 1 AWS WINDOWS INSTANCE)
--------------------------------------------------------------------------------
"@ -ForegroundColor Yellow
    }

    'destroy' {
        Audit "=== DECOMMISSION / DESTROY REQUESTED ==="
        Audit "Destruction requires explicit human approval in an interactive terminal."
        Write-Host @"
--------------------------------------------------------------------------------
                    PRIVILEGED TEARDOWN COMMANDS
--------------------------------------------------------------------------------
1. To destroy Akamai pilot Linux Linodes and Cloud Firewalls:
   .\scripts\Destroy.ps1
   (Enforces confirmation: DESTROY LODESTAR)

2. To destroy AWS Windows Server instance:
   .\scripts\Destroy-AwsWindows.ps1
   (Enforces administrator authentication, confirmation: DESTROY 1 AWS WINDOWS INSTANCE,
    and polls for verified instance termination)
--------------------------------------------------------------------------------
"@ -ForegroundColor Yellow
    }

    'reconcile' {
        Audit "=== Multi-Cloud State Drift Reconciliation ==="
        Audit "[Akamai] Checking for Terraform state drift caused by external watchdog deletion..."
        Push-Location $projectRoot
        try {
            Audit "Running terraform plan -refresh-only to detect cloud drift..."
            & terraform plan -refresh-only
            Write-Host @"
--------------------------------------------------------------------------------
To synchronize Terraform state with live Akamai Cloud (dropping watchdog-deleted Linodes):
  terraform apply -refresh-only
--------------------------------------------------------------------------------
"@ -ForegroundColor Cyan
        } finally { Pop-Location }

        Audit "[AWS] Checking persistent state in state\aws-fleet-state.json..."
        $awsStateFile = Join-Path $projectRoot 'state\aws-fleet-state.json'
        if (Test-Path $awsStateFile) {
            $state = Get-Content $awsStateFile -Raw | ConvertFrom-Json
            if ($state.instance_id -and [string]$state.status -ne 'destroyed') {
                Audit "Verifying instance $($state.instance_id) in live AWS inventory..."
                $r = Invoke-AwsCli @('ec2', 'describe-instances', '--instance-ids', [string]$state.instance_id,
                    '--profile', 'lodestar-observer', '--region', 'us-east-1',
                    '--query', 'Reservations[].Instances[].State.Name', '--output', 'text')
                $exit = $r.ExitCode
                $chk = if ($exit -eq 0) { $r.Out } else { $r.Err }
                # Only a definite answer from EC2 marks the instance gone. An expired
                # session, throttling or a network error must not rewrite the state.
                $gone = ($exit -eq 0 -and $chk.Trim() -ceq 'terminated') -or
                        ($exit -ne 0 -and $chk -match 'InvalidInstanceID\.NotFound')
                if (-not $gone -and $exit -ne 0) {
                    Audit "Could not verify instance $($state.instance_id) (exit $exit). State left unchanged."
                }
                if ($gone) {
                    Audit "Instance $($state.instance_id) found terminated or gone. Updating state to 'destroyed'."
                    $state.status = "destroyed"
                    $state | Add-Member -NotePropertyName reconciled_at -NotePropertyValue (Get-Date).ToUniversalTime().ToString('o') -Force
                    $state | ConvertTo-Json -Depth 5 | Set-Content $awsStateFile -Encoding utf8
                }
            }
        }
    }
}
