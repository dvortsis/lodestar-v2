<#
.SYNOPSIS
    Secure PIA VPN Credential Provisioner and Gateway Controller for Akamai Linodes.
.DESCRIPTION
    Retrieves PIA credentials from AWS Secrets Manager using LodestarPiaSecretReader,
    generates ephemeral WireGuard configuration via PIA's authenticated REST API,
    streams wg0.conf directly to /run/lodestar/vpn on the Linode over host-key-verified SSH,
    starts Gluetun, and verifies zero public-IP/DNS leakage before crawler startup.
    Zero AWS credentials or PIA master passwords are installed on any Linode.
.PARAMETER Target
    Public IP address or hostname of the target Linode.
.PARAMETER Region
    PIA VPN region identifier (default: us-east).
.PARAMETER Action
    Action to perform: FullWorkflow, Provision, StartVpn, VerifyTunnel, StartCrawler, Health, Cleanup.
.PARAMETER VerifyOnly
    Test AWS secret access and exit.
.PARAMETER StartCrawler
    Launch the crawler container after leak verification passes.
.PARAMETER DryRun
    Simulate execution without modifying the remote instance.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $false, Position = 0)]
    [string]$Target,

    [Parameter(Mandatory = $false)]
    [string]$Region = 'us-east',

    [Parameter(Mandatory = $false)]
    [ValidateSet('FullWorkflow', 'Provision', 'StartVpn', 'VerifyTunnel', 'StartCrawler', 'Health', 'Cleanup')]
    [string]$Action = 'FullWorkflow',

    [Parameter(Mandatory = $false)]
    [string]$SshKey = "$HOME\.ssh\lodestar_akamai_ed25519",

    [Parameter(Mandatory = $false)]
    [string]$SshUser = 'lodestar',

    [Parameter(Mandatory = $false)]
    [string]$AwsAccountId = $env:LODESTAR_AWS_ACCOUNT_ID,

    [Parameter(Mandatory = $false)]
    [switch]$VerifyOnly,

    [Parameter(Mandatory = $false)]
    [switch]$StartCrawler,

    [Parameter(Mandatory = $false)]
    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$projectRoot = (Resolve-Path (Join-Path $scriptDir '..')).Path
$logsDir = Join-Path $projectRoot 'logs'
New-Item -ItemType Directory -Force -Path $logsDir | Out-Null
$logFile = Join-Path $logsDir ("pia-vpn-{0}.log" -f (Get-Date -Format 'yyyyMMdd-HHmmss'))

function Audit([string]$Message) {
    $line = '{0} [PIA-VPN] {1}' -f (Get-Date).ToUniversalTime().ToString('o'), $Message
    Write-Host $line
    Add-Content -LiteralPath $logFile -Value $line
}

# Resolve Python 3 executable
$pythonExe = $null
foreach ($cmd in @('python', 'py', 'python3')) {
    $found = Get-Command $cmd -ErrorAction SilentlyContinue
    if ($found) {
        $pythonExe = $found.Source
        break
    }
}

if (-not $pythonExe) {
    Audit "ERROR: Python 3 executable not found on PATH."
    throw "Python 3 is required to run the Lodestar PIA VPN bridge."
}

$bridgeScript = Join-Path $scriptDir 'lodestar_pia_bridge.py'
if (-not (Test-Path $bridgeScript)) {
    Audit "ERROR: Bridge script not found at $bridgeScript"
    throw "Bridge script missing: $bridgeScript"
}

$accountArgs = @()
if ($AwsAccountId) {
    $accountArgs = @('--account-id', $AwsAccountId)
}

# Case 1: Verify AWS Secret Access only
if ($VerifyOnly) {
    Audit "Verifying AWS STS caller identity and Secrets Manager access..."
    & $pythonExe $bridgeScript verify-aws @accountArgs
    if ($LASTEXITCODE -eq 0) {
        Audit "AWS verification passed: LodestarPiaSecretReader can access lodestar/pia-vpn."
    } else {
        Audit "AWS verification failed with exit code $LASTEXITCODE."
        throw "AWS verification failed with exit code $LASTEXITCODE."
    }
    return
}

# All other actions require a target IP
if (-not $Target) {
    Audit "ERROR: -Target <Linode_IP> is required for action '$Action'."
    Write-Host @"
Usage:
  .\scripts\Deploy-PiaVpn.ps1 -Target <LINODE_IP> [-Region us-east] [-StartCrawler]
  .\scripts\Deploy-PiaVpn.ps1 -VerifyOnly
  .\scripts\Deploy-PiaVpn.ps1 -Target <LINODE_IP> -Action Health
  .\scripts\Deploy-PiaVpn.ps1 -Target <LINODE_IP> -Action Cleanup
"@
    throw "-Target <Linode_IP> is required for action '$Action'."
}

if ($Target -notmatch '^[a-zA-Z0-9.-]+$') {
    throw "Invalid target format: '$Target'"
}

if (-not (Test-Path $SshKey)) {
    Audit "WARNING: SSH private key not found at $SshKey. Connection may fail."
}

if ($DryRun) {
    Audit "[DRY-RUN] Simulating $Action for target $Target (Region: $Region)..."
    Audit "[DRY-RUN] Would assume LodestarPiaSecretReader and retrieve lodestar/pia-vpn"
    Audit "[DRY-RUN] Would negotiate WireGuard keypair and stream wg0.conf to /run/lodestar/vpn"
    Audit "[DRY-RUN] Would start Gluetun container and verify leak protection"
    Audit "[DRY-RUN] Dry run complete. Zero remote changes made."
    return
}

switch ($Action) {
    'FullWorkflow' {
        Audit "=== Executing Lodestar Secure PIA VPN Workflow for $Target ==="
        $provArgs = @('provision', '--target', $Target, '--pia-region', $Region, '--ssh-user', $SshUser, '--ssh-key', $SshKey)
        if ($StartCrawler) {
            $provArgs += '--start-crawler'
        }
        $provArgs += $accountArgs
        Audit "Provisioning wg0.conf, starting Gluetun, and verifying leak protection..."
        & $pythonExe $bridgeScript @provArgs
        if ($LASTEXITCODE -ne 0) { throw "VPN provisioning workflow failed with exit code $LASTEXITCODE." }

        if ($StartCrawler) {
            Audit "Crawler started cleanly behind verified VPN."
        } else {
            Audit "VPN gateway ready. Crawler was not started (pass -StartCrawler to launch)."
        }
    }

    'Provision' {
        Audit "Provisioning wg0.conf to $Target..."
        & $pythonExe $bridgeScript provision --target $Target --pia-region $Region --ssh-user $SshUser --ssh-key $SshKey @accountArgs
        if ($LASTEXITCODE -ne 0) { throw "Provision failed with exit code $LASTEXITCODE." }
    }

    'StartVpn' {
        Audit "Starting Gluetun container on $Target..."
        & $pythonExe $bridgeScript start-vpn --target $Target --ssh-user $SshUser --ssh-key $SshKey
        if ($LASTEXITCODE -ne 0) { throw "StartVpn failed with exit code $LASTEXITCODE." }
    }

    'VerifyTunnel' {
        Audit "Verifying VPN egress and leak protection on $Target..."
        & $pythonExe $bridgeScript verify-tunnel --target $Target --ssh-user $SshUser --ssh-key $SshKey
        if ($LASTEXITCODE -ne 0) { throw "VerifyTunnel failed with exit code $LASTEXITCODE." }
    }

    'StartCrawler' {
        Audit "Starting crawler on $Target after leak check..."
        & $pythonExe $bridgeScript start-crawler --target $Target --ssh-user $SshUser --ssh-key $SshKey
        if ($LASTEXITCODE -ne 0) { throw "StartCrawler failed with exit code $LASTEXITCODE." }
    }

    'Health' {
        Audit "Querying VPN health and egress IP for $Target..."
        & $pythonExe $bridgeScript health --target $Target --ssh-user $SshUser --ssh-key $SshKey
        if ($LASTEXITCODE -ne 0) { throw "Health check failed with exit code $LASTEXITCODE." }
    }

    'Cleanup' {
        Audit "Cleaning up runtime secrets on $Target..."
        & $pythonExe $bridgeScript cleanup --target $Target --ssh-user $SshUser --ssh-key $SshKey
        if ($LASTEXITCODE -ne 0) { throw "Cleanup failed with exit code $LASTEXITCODE." }
    }
}
