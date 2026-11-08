# Run from infra/akamai/terraform in the repository checkout: .\scripts\Preflight.ps1
# Read-only Linode API requests plus local Terraform init/validate. Never prints the token.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$logs = Join-Path $root 'logs'
New-Item -ItemType Directory -Force -Path $logs | Out-Null
$log = Join-Path $logs ("preflight-{0}.log" -f (Get-Date).ToUniversalTime().ToString('yyyyMMdd-HHmmssZ'))
function Audit([string]$Message) {
    $line = '{0} {1}' -f (Get-Date).ToUniversalTime().ToString('o'), $Message
    Write-Host $line
    Add-Content -LiteralPath $log -Value $line
}
function Assert-Tool([string]$Name) {
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) { throw "Missing required tool: $Name" }
}
function ApiGet([string]$Path) {
    $u = "https://api.linode.com/v4/$Path"
    try {
        return Invoke-RestMethod -Method Get -Uri $u -Headers @{ Authorization = "Bearer $env:LINODE_TOKEN" } -TimeoutSec 30 -ErrorAction Stop
    } catch {
        $status = 'unknown'
        if ($_.Exception.Response -and $_.Exception.Response.StatusCode) {
            $status = [int]$_.Exception.Response.StatusCode
        }
        throw "Akamai GET /$Path failed (HTTP $status). Check token scopes and account status; no changes made."
    }
}
try {
    Audit 'Read-only Akamai preflight started'
    Assert-Tool 'terraform'
    Assert-Tool 'ssh-keygen'
    if ([string]::IsNullOrWhiteSpace($env:LINODE_TOKEN)) { throw 'LINODE_TOKEN is missing from the current PowerShell session. Do not paste it in chat.' }
    $tfvars = Join-Path $root 'terraform.tfvars'
    if (-not (Test-Path -LiteralPath $tfvars -PathType Leaf)) { throw 'Copy terraform.tfvars.example to terraform.tfvars, then customize it.' }
    $config = Get-Content -LiteralPath $tfvars -Raw
    $regionMatch = [regex]::Match($config, '(?m)^\s*region\s*=\s*"([a-z0-9-]+)"')
    if (-not $regionMatch.Success) { throw 'Missing region in terraform.tfvars.' }
    $region = $regionMatch.Groups[1].Value
    if ($region -cne 'us-east') { throw "region is '$region'. Only us-east is supported: the 48-hour watchdog reaps nothing in other regions." }
    $keyMatch = [regex]::Match($config, '(?m)^\s*ssh_public_key_path\s*=\s*"([^\"]+)"')
    if (-not $keyMatch.Success) { throw 'Missing ssh_public_key_path in terraform.tfvars.' }
    $keyPath = $keyMatch.Groups[1].Value
    if ($keyPath.StartsWith('~/') -or $keyPath.StartsWith('~\')) {
        $keyPath = Join-Path $HOME ($keyPath.Substring(2))
    }
    if (-not (Test-Path -LiteralPath $keyPath -PathType Leaf)) { throw "SSH PUBLIC key not found: $keyPath" }
    $pubKey = (Get-Content -LiteralPath $keyPath -Raw).Trim()
    if ($pubKey -notmatch '^(ssh-ed25519|ecdsa-sha2-nistp256|ssh-rsa)\s+[A-Za-z0-9+/=]+') {
        throw 'Public SSH key content is not an expected OpenSSH public key.'
    }
    $cidrMatch = [regex]::Match($config, '(?m)^\s*ssh_admin_ipv4_cidr\s*=\s*"([^\"]+)"')
    if (-not $cidrMatch.Success -or $cidrMatch.Groups[1].Value -eq '203.0.113.20/32') { throw 'Replace the documentation-only ssh_admin_ipv4_cidr with your current actual public IPv4 /32.' }
    Audit "Region configured: $region"
    $null = ApiGet 'profile'
    Audit 'Akamai API token authenticated'
    $settings = ApiGet 'account/settings'
    if ($settings.interfaces_for_new_linodes -eq 'linode_only') {
        throw 'Account settings allow Linode Interfaces ONLY. This template requires legacy_config for atomic firewall attachment. No resources created; select a setting that allows Configuration Profile Interfaces before applying.'
    }
    Audit ("Network interface policy: {0}" -f $settings.interfaces_for_new_linodes)
    $null = ApiGet ("regions/{0}" -f $region)
    Audit 'Requested region was found in Akamai API'
    $main = ApiGet 'linode/types/g6-standard-4'
    $worker = ApiGet 'linode/types/g6-nanode-1'
    Audit ("Main plan: {0}, RAM {1} MB, disk {2} MB" -f $main.id, $main.memory, $main.disk)
    Audit ("Worker plan: {0}, RAM {1} MB, disk {2} MB" -f $worker.id, $worker.memory, $worker.disk)
    $null = ApiGet 'images/linode/ubuntu24.04'
    Audit 'Ubuntu 24.04 image available'
    $linodes = ApiGet 'linode/instances?page_size=100'
    Audit ("Existing Linodes returned on first page: {0} (not necessarily full account inventory)" -f @($linodes.data).Count)
    $null = ApiGet 'networking/firewalls?page_size=100'
    Audit 'Read access to account firewalls available'
    Push-Location $root
    try {
        & terraform init -input=false -no-color 2>&1 | ForEach-Object { Audit ([string]$_) }
        if ($LASTEXITCODE -ne 0) { throw "terraform init failed with code $LASTEXITCODE" }
        & terraform validate -no-color 2>&1 | ForEach-Object { Audit ([string]$_) }
        if ($LASTEXITCODE -ne 0) { throw "terraform validate failed with code $LASTEXITCODE" }
    } finally { Pop-Location }
    Audit 'PASS: Preflight completed. NO cloud resources created.'
} catch {
    Audit ("FAILED: {0}" -f $_.Exception.Message)
    throw
}
