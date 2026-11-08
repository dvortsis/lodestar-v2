# scripts/Invoke-Preflight.ps1
# Secure interactive runner for Lodestar Akamai preflight and Terraform plan.
# Prompts for Akamai PAT in local process memory only. Preserves existing keys.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$logs = Join-Path $root 'logs'
New-Item -ItemType Directory -Force -Path $logs | Out-Null
$stamp = (Get-Date).ToUniversalTime().ToString('yyyyMMdd-HHmmssZ')
$runLog = Join-Path $logs "runner-$stamp.log"

function Audit-Log([string]$Message) {
    $line = '{0} {1}' -f (Get-Date).ToUniversalTime().ToString('o'), $Message
    Write-Host $line
    Add-Content -LiteralPath $runLog -Value $line
}

try {
    Audit-Log '=== Lodestar Akamai Interactive Setup & Preflight ==='

    # 1. SSH Key Pair Check & Preservation
    $sshDir = Join-Path $HOME '.ssh'
    New-Item -ItemType Directory -Path $sshDir -Force | Out-Null
    $keyPath = Join-Path $sshDir 'lodestar_akamai_ed25519'
    $pubPath = "$keyPath.pub"

    if (-not (Test-Path -LiteralPath $keyPath)) {
        Audit-Log "SSH key pair not found. Generating Ed25519 key at $keyPath"
        Write-Host "`n>>> Enter a passphrase for the new SSH key when prompted by ssh-keygen:" -ForegroundColor Cyan
        & ssh-keygen -t ed25519 -a 100 -f $keyPath -C 'lodestar-akamai'
        if ($LASTEXITCODE -ne 0) { throw "ssh-keygen failed with code $LASTEXITCODE" }

        Audit-Log 'Verifying newly created SSH private and public key pair match...'
        Write-Host "`n>>> Verifying private key: enter key passphrase if prompted:" -ForegroundColor Cyan
        $derivedPub = (& ssh-keygen -y -f $keyPath)
        if ($LASTEXITCODE -ne 0) { throw "ssh-keygen -y failed to read private key ($keyPath)." }
        $storedPub = (Get-Content -LiteralPath $pubPath -Raw).Trim()

        $derivedParts = ($derivedPub.Trim() -split '\s+')[0..1] -join ' '
        $storedParts = ($storedPub -split '\s+')[0..1] -join ' '
        if ($derivedParts -ne $storedParts) {
            throw "Key mismatch: public key in $pubPath does not match private key $keyPath!"
        }
        Audit-Log 'PASS: New SSH key pair created and verified.'
    } else {
        Audit-Log "SSH private key already exists at $keyPath (preserving existing key; will not overwrite)."
        if (-not (Test-Path -LiteralPath $pubPath)) {
            throw "SSH public key missing: $pubPath"
        }
        $storedPub = (Get-Content -LiteralPath $pubPath -Raw).Trim()
        if ($storedPub -notmatch '^ssh-ed25519\s+[A-Za-z0-9+/=]+') {
            throw "Public key in $pubPath is not a valid OpenSSH Ed25519 key."
        }
        Audit-Log "PASS: Existing SSH public key verified at $pubPath."
    }

    # 2. Akamai PAT Token
    if ([string]::IsNullOrWhiteSpace($env:LINODE_TOKEN)) {
        Write-Host "`n>>> Paste your existing Akamai / Linode Personal Access Token (hidden):" -ForegroundColor Cyan
        $secret = Read-Host -Prompt 'Akamai API Token' -AsSecureString
        $env:LINODE_TOKEN = [System.Net.NetworkCredential]::new('', $secret).Password
        Remove-Variable secret
    }

    if ([string]::IsNullOrWhiteSpace($env:LINODE_TOKEN)) {
        throw 'Akamai API token is required to proceed.'
    }
    Audit-Log 'Akamai API token loaded into process memory.'

    # 3. Run Preflight
    Audit-Log 'Invoking .\scripts\Preflight.ps1...'
    Push-Location $root
    try {
        & .\scripts\Preflight.ps1
        if ($LASTEXITCODE -ne 0) { throw "Preflight.ps1 failed with code $LASTEXITCODE" }

        # 4. Terraform Formatting, Validation, and Plan
        Audit-Log 'Running terraform fmt -check -recursive...'
        & terraform fmt -check -recursive
        if ($LASTEXITCODE -ne 0) { throw 'terraform fmt check failed.' }

        Audit-Log 'Running terraform validate...'
        & terraform validate
        if ($LASTEXITCODE -ne 0) { throw 'terraform validate failed.' }

        Audit-Log 'Running terraform plan -input=false...'
        & terraform plan -input=false
        if ($LASTEXITCODE -ne 0) { throw 'terraform plan failed.' }

        Audit-Log 'PASS: All checks, preflight, and Terraform plan completed successfully. Zero cloud resources created.'
    } finally {
        Pop-Location
    }
} catch {
    Audit-Log ("FAILED: {0}" -f $_.Exception.Message)
    throw
} finally {
    # Redact and clear token from memory
    $env:LINODE_TOKEN = $null
    Audit-Log 'LINODE_TOKEN cleared from process environment.'
}
