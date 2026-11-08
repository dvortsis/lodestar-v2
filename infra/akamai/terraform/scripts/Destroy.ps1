# Destroys all resources in THIS Terraform state. Deletion is irreversible.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$logs = Join-Path $root 'logs'
New-Item -ItemType Directory -Force -Path $logs | Out-Null
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$log = Join-Path $logs "destroy-$stamp.log"
$plan = Join-Path $root "lodestar-destroy-$stamp.tfplan"
function Audit([string]$Message) {
    $line = '{0} {1}' -f (Get-Date).ToUniversalTime().ToString('o'), $Message
    Write-Host $line; Add-Content -LiteralPath $log -Value $line
}
try {
    if ([string]::IsNullOrWhiteSpace($env:LINODE_TOKEN)) { throw 'LINODE_TOKEN environment variable is missing.' }
    Push-Location $root
    try {
        & terraform init -input=false -no-color 2>&1 | ForEach-Object { Audit ([string]$_) }
        if ($LASTEXITCODE -ne 0) { throw 'terraform init failed.' }
        $state = @(& terraform state list)
        if ($LASTEXITCODE -ne 0) { throw 'Unable to read Terraform state. Do not assume resources are gone.' }
        Audit "Resources in the current state: $($state.Count)"
        foreach ($item in $state) { Audit ([string]$item) }
        if ($state.Count -eq 0) { Audit 'No managed resources found. Nothing to destroy.'; return }
        & terraform plan -destroy -input=false -no-color -out="$plan" 2>&1 | ForEach-Object { Audit ([string]$_) }
        if ($LASTEXITCODE -ne 0) { throw 'Destroy plan failed.' }
        $approval = Read-Host "Type exactly 'DESTROY LODESTAR' to permanently delete resources above"
        if ($approval -cne 'DESTROY LODESTAR') { Audit 'Cancelled by user.'; return }
        & terraform apply -input=false -no-color "$plan" 2>&1 | ForEach-Object { Audit ([string]$_) }
        if ($LASTEXITCODE -ne 0) { throw 'Destroy failed. Check Linode Cloud Manager for stragglers immediately.' }
        $remaining = @(& terraform state list)
        if ($LASTEXITCODE -ne 0) { throw 'Destroy applied, but state validation failed.' }
        Audit "Destroy verification: $($remaining.Count) managed resources remain in this Terraform state."
        if ($remaining.Count -ne 0) { throw 'Terraform state is not empty; investigate remaining resources.' }
        Audit 'PASS: No resources remain in Terraform state. Independently check Linode Cloud Manager.'
    } finally { Pop-Location }
} catch {
    Audit ("FAILED: {0}" -f $_.Exception.Message)
    throw
} finally {
    if (Test-Path -LiteralPath $plan) { Remove-Item -LiteralPath $plan -Force -ErrorAction SilentlyContinue }
}
