<#
.SYNOPSIS
    Automated integration and verification test suite for Lodestar multi-cloud automation.
.DESCRIPTION
    Executes live verification tests covering role separation, expiration policy enforcement,
    MCP fail-fast mechanics, Terraform validation, and MCP tool execution.
#>
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$projectRoot = (Resolve-Path (Join-Path $scriptDir '..')).Path
$scriptsDir = Join-Path $projectRoot 'scripts'

Write-Host "================================================================================" -ForegroundColor Cyan
Write-Host "             LODESTAR MULTI-CLOUD AUTOMATION TEST SUITE                         " -ForegroundColor Cyan
Write-Host "================================================================================" -ForegroundColor Cyan

$passed = 0
$failed = 0

function Run-Test([string]$Name, [scriptblock]$Block) {
    Write-Host -NoNewline "[TEST] $Name ... "
    try {
        & $Block
        Write-Host "PASS" -ForegroundColor Green
        $script:passed++
    } catch {
        Write-Host "FAIL" -ForegroundColor Red
        Write-Host "  Error: $($_.Exception.Message)" -ForegroundColor DarkRed
        $script:failed++
    }
}

# -----------------------------------------------------------------------------
# 1. Expiration Policy Unit Tests
# -----------------------------------------------------------------------------
. (Join-Path $scriptsDir 'Test-ExpirationPolicy.ps1')

Run-Test "Expiration: Valid 24h ISO lease accepted" {
    $iso = (Get-Date).ToUniversalTime().AddHours(24).ToString("yyyy-MM-ddTHH:mm:ssZ")
    $res = Assert-LodestarExpiration -ExpiresAt $iso -MaxHours 48
    if (-not $res.IsValid -or $res.LeaseHours -lt 23.9) { throw "Validation failed unexpectedly" }
}

Run-Test "Expiration: Past timestamp rejected" {
    $threw = $false
    try { Assert-LodestarExpiration -ExpiresAt "2020-01-01T00:00:00Z" } catch { $threw = $true }
    if (-not $threw) { throw "Accepted past timestamp" }
}

Run-Test "Expiration: Lease exceeding 48h ceiling rejected" {
    $threw = $false
    $tooLong = (Get-Date).ToUniversalTime().AddHours(50).ToString("yyyy-MM-ddTHH:mm:ssZ")
    try { Assert-LodestarExpiration -ExpiresAt $tooLong -MaxHours 48 } catch { $threw = $true }
    if (-not $threw) { throw "Accepted lease > 48h" }
}

Run-Test "Expiration: Malformed non-ISO format rejected" {
    $threw = $false
    try { Assert-LodestarExpiration -ExpiresAt "2026-11-08 12:00:00" } catch { $threw = $true }
    if (-not $threw) { throw "Accepted malformed date string" }
}

# -----------------------------------------------------------------------------
# 2. AWS Role Least Privilege Tests
# -----------------------------------------------------------------------------
$awsCli = "C:\Program Files\Amazon\AWSCLIV2\aws.exe"

Run-Test "Role Security: LodestarObserver assumes identity cleanly" {
    $idJson = & $awsCli sts get-caller-identity --profile lodestar-observer-mcp 2>&1
    if ($LASTEXITCODE -ne 0) { throw "Failed to get caller identity: $idJson" }
    $id = $idJson | ConvertFrom-Json
    if ($id.Arn -notmatch 'role/LodestarObserver') { throw "Unexpected role ARN: $($id.Arn)" }
}

Run-Test "Role Security: LodestarObserver allows read-only EC2 Describe" {
    $res = & $awsCli ec2 describe-availability-zones --profile lodestar-observer-mcp --region us-east-1 2>&1
    if ($LASTEXITCODE -ne 0) { throw "EC2 describe-availability-zones failed: $res" }
}

Run-Test "Role Security: LodestarObserver DENIES ec2:RunInstances" {
    $eap = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    try {
        $res = (& $awsCli ec2 run-instances --dry-run --image-id ami-0bb49139514a40253 --instance-type c6a.xlarge --profile lodestar-observer-mcp --region us-east-1 2>&1) -join "`n"
    } finally { $ErrorActionPreference = $eap }
    if ($res -notmatch "UnauthorizedOperation") {
        throw "Security failure: ec2:RunInstances was not denied as expected ($res)"
    }
}

Run-Test "Role Security: LodestarObserver DENIES ssm:StartSession" {
    $eap = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    try {
        $res = (& $awsCli ssm start-session --target i-0123456789abcdef0 --profile lodestar-observer-mcp --region us-east-1 2>&1) -join "`n"
    } finally { $ErrorActionPreference = $eap }
    if ($res -notmatch "AccessDeniedException") {
        throw "Security failure: ssm:StartSession was not denied as expected ($res)"
    }
}

# -----------------------------------------------------------------------------
# 3. Fail-Fast Credential Process Tests
# -----------------------------------------------------------------------------
$credWrapper = Join-Path $scriptsDir 'Get-McpCredentials.ps1'

Run-Test "MCP Wrapper: Returns valid JSON schema when session cached" {
    $jsonOutput = (& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $credWrapper -RoleName LodestarObserver -Profile lodestar-observer 2>&1) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw "Wrapper returned nonzero: $jsonOutput" }
    $parsed = $jsonOutput | ConvertFrom-Json
    if ($parsed.Version -ne 1 -or -not $parsed.AccessKeyId -or -not $parsed.SessionToken) {
        throw "Invalid credential JSON schema returned: $jsonOutput"
    }
}

Run-Test "MCP Wrapper: Fails fast on nonexistent/expired session" {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $eap = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    try {
        $errOutput = (& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $credWrapper -RoleName NonExistentRole 2>&1) -join "`n"
    } finally { $ErrorActionPreference = $eap }
    $sw.Stop()
    if ($LASTEXITCODE -eq 0) { throw "Expected nonzero exit code on invalid session" }
    if ($sw.ElapsedMilliseconds -gt 3000) { throw "Fail-fast exceeded 3s timeout ($($sw.ElapsedMilliseconds)ms)" }
    if ($errOutput -notmatch "expired or is not initialized") { throw "Missing expected error message: $errOutput" }
}

# -----------------------------------------------------------------------------
# 4. Akamai Terraform Formatting and Validation
# -----------------------------------------------------------------------------
Run-Test "Terraform: Formatting check passes" {
    Push-Location $projectRoot
    try {
        & terraform fmt -check -recursive
        if ($LASTEXITCODE -ne 0) { throw "terraform fmt check failed" }
    } finally { Pop-Location }
}

Run-Test "Terraform: Syntax and provider validation passes" {
    Push-Location $projectRoot
    try {
        & terraform validate
        if ($LASTEXITCODE -ne 0) { throw "terraform validate failed" }
    } finally { Pop-Location }
}

# -----------------------------------------------------------------------------
# 5. AWS Privileged Provisioner Authorization & Isolation Tests
# -----------------------------------------------------------------------------
$deployAws = Join-Path $scriptsDir 'Deploy-AwsWindows.ps1'

Run-Test "IAM Security: lodestar-agent is explicitly DENIED sts:AssumeRole on LodestarProvisioner" {
    $eap = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    try {
        $expectedAccountId = $env:LODESTAR_AWS_ACCOUNT_ID
        if ($expectedAccountId -cnotmatch '^[0-9]{12}$') { throw 'Set LODESTAR_AWS_ACCOUNT_ID to the approved AWS account ID before IAM testing.' }
        $res = (& $awsCli sts assume-role --role-arn "arn:aws:iam::${expectedAccountId}:role/LodestarProvisioner" --role-session-name test-deny-check --profile lodestar-agent 2>&1) -join "`n"
    } finally { $ErrorActionPreference = $eap }
    if ($LASTEXITCODE -eq 0 -or $res -notmatch "AccessDenied") {
        throw "Security failure: lodestar-agent was not denied assume-role for LodestarProvisioner ($res)"
    }
}

Run-Test "IAM Security: lodestar-agent CAN assume LodestarObserver" {
    $expectedAccountId = $env:LODESTAR_AWS_ACCOUNT_ID
    if ($expectedAccountId -cnotmatch '^[0-9]{12}$') { throw 'Set LODESTAR_AWS_ACCOUNT_ID to the approved AWS account ID before IAM testing.' }
    $res = (& $awsCli sts assume-role --role-arn "arn:aws:iam::${expectedAccountId}:role/LodestarObserver" --role-session-name test-obs-check --profile lodestar-agent 2>&1) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw "lodestar-agent failed to assume LodestarObserver: $res" }
    $parsed = $res | ConvertFrom-Json
    if ($parsed.AssumedRoleUser.Arn -notmatch 'role/LodestarObserver') {
        throw "Unexpected role ARN assumed: $($parsed.AssumedRoleUser.Arn)"
    }
}

Run-Test "AWS Deploy: Refuses execution when administrator provisioner session is not active" {
    $eap = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    try {
        $res = (& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $deployAws -DryRun -Hours 48 2>&1) -join "`n"
    } finally { $ErrorActionPreference = $eap }
    if ($LASTEXITCODE -eq 0 -or $res -notmatch "lodestar-provisioner session is not active or unauthorized") {
        throw "Expected Deploy-AwsWindows.ps1 to block execution without active administrator session ($res)"
    }
}

# -----------------------------------------------------------------------------
# 6. Akamai State Drift Reconciliation
# -----------------------------------------------------------------------------
Run-Test "Akamai Reconcile: terraform plan -refresh-only succeeds" {
    Push-Location $projectRoot
    try {
        & terraform plan -refresh-only
        if ($LASTEXITCODE -ne 0) { throw "terraform plan -refresh-only failed" }
    } finally { Pop-Location }
}

$resultColor = if ($failed -eq 0) { "Green" } else { "Red" }
Write-Host "================================================================================" -ForegroundColor Cyan
Write-Host "TEST RESULTS: Passed: $passed, Failed: $failed" -ForegroundColor $resultColor
Write-Host "================================================================================" -ForegroundColor Cyan

if ($failed -gt 0) { exit 1 } else { exit 0 }
