# Explicitly deploy the configuration stored in terraform.tfvars.
# Requires PowerShell, Terraform, and LINODE_TOKEN. Never creates a token.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$logs = Join-Path $root 'logs'
New-Item -ItemType Directory -Force -Path $logs | Out-Null
$stamp = (Get-Date).ToUniversalTime().ToString('yyyyMMdd-HHmmssZ')
$log = Join-Path $logs "deploy-$stamp.log"
$plan = Join-Path $root "lodestar-$stamp.tfplan"
function Audit([string]$Message) {
    $line = '{0} {1}' -f (Get-Date).ToUniversalTime().ToString('o'), $Message
    Write-Host $line; Add-Content -LiteralPath $log -Value $line
}
try {
    if ([string]::IsNullOrWhiteSpace($env:LINODE_TOKEN)) { throw 'LINODE_TOKEN environment variable is missing.' }
    # The confirmation phrase must be typed at a console: refuse piped input up front.
    . (Join-Path $PSScriptRoot 'Interactive.ps1')
    Assert-InteractiveConfirmation
    if (-not (Test-Path -LiteralPath (Join-Path $root 'terraform.tfvars'))) { throw 'terraform.tfvars is missing.' }
    Push-Location $root
    try {
        Audit 'Planning deployment. No billable actions yet.'
        & terraform init -input=false -no-color 2>&1 | ForEach-Object { Audit ([string]$_) }
        if ($LASTEXITCODE -ne 0) { throw 'terraform init failed.' }
        & terraform plan -input=false -no-color -out="$plan" 2>&1 | ForEach-Object { Audit ([string]$_) }
        if ($LASTEXITCODE -ne 0) { throw 'terraform plan failed.' }
        $rawJson = & terraform show -json "$plan"
        if ($LASTEXITCODE -ne 0) { throw 'Could not parse Terraform plan JSON.' }
        $data = ($rawJson | Out-String | ConvertFrom-Json)
        $resources = @($data.planned_values.root_module.resources | Where-Object { $_ -and $_.type -eq 'linode_instance' })
        $count = @($resources).Count
        if ($count -lt 1 -or $count -gt 7) { throw "Expected 1-7 Linode instances, but plan contains $count. Check terraform.tfvars." }
        Audit "This operation will create/manage $count planned Linode instance resources."
        Audit 'Review the full plan below before consenting.'
        & terraform show -no-color "$plan" 2>&1 | ForEach-Object { Audit ([string]$_) }
        if ($LASTEXITCODE -ne 0) { throw 'terraform show failed.' }
        $expected = "DEPLOY $count LINODES"
        $approval = Read-Host "Type exactly '$expected' to apply this BILLABLE plan"
        if ($approval -cne $expected) { Audit 'Cancelled by user. No resources created.'; return }
        & terraform apply -input=false -no-color "$plan" 2>&1 | ForEach-Object { Audit ([string]$_) }
        if ($LASTEXITCODE -ne 0) { throw 'terraform apply failed. Check cloud state and terraform state before retrying.' }
        Audit 'Deployment completed. IMPORTANT: destroy all VMs when finished, including powered-off VMs.'
    } finally { Pop-Location }
} catch {
    Audit ("FAILED: {0}" -f $_.Exception.Message)
    throw
} finally {
    if (Test-Path -LiteralPath $plan) { Remove-Item -LiteralPath $plan -Force -ErrorAction SilentlyContinue }
}
