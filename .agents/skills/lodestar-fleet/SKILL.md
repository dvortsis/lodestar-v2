---
name: lodestar-fleet
description: >-
  Unified multi-cloud control workflow for the Lodestar fleet across AWS (Windows Server)
  and Akamai Cloud / Linode (Linux test fleet). Use when inspecting, planning, deploying,
  checking status, rebooting, accessing remotely, or destroying Lodestar test infrastructure.
version: 1
---

# Lodestar Multi-Cloud Fleet Control

This skill defines the unified, secure operational workflow for managing Lodestar test infrastructure across **AWS** (Windows Server) and **Akamai Cloud / Linode** (Linux test fleet).

---

## Architecture Overview

```
                      +---------------------------------------+
                      |       Lodestar Multi-Cloud Fleet      |
                      +---------------------------------------+
                                  |                 |
                   +--------------+                 +---------------+
                   |                                                |
                   v                                                v
    +------------------------------+                +-------------------------------+
    |         AWS Cloud            |                |       Akamai Cloud (Linode)   |
    | (Dedicated Windows Server)   |                |     (Disposable Linux Fleet)  |
    +------------------------------+                +-------------------------------+
    | Account:  AWS_ACCOUNT_ID       |                | Project: Downloads\lodestar-  |
    | Region:   us-east-1          |                |          akamai-terraform     |
    | IAM User: lodestar-agent     |                | Region:  us-east (Newark)     |
    | Role:     LodestarProvisioner|                | Main:    g6-standard-4 (8GB)  |
    | Subnet:   subnet-0cced923e2d2|                | Workers: g6-nanode-1 (1GB)    |
    | Security: Closed (SSM only)  |                | Inbound: DROP except /32 SSH  |
    +------------------------------+                +-------------------------------+
```

---

## 1. AWS Multi-Role Architecture (`LodestarObserver` vs `LodestarProvisioner`)

AWS access follows the principle of least privilege, segregating read-only discovery from privileged lifecycle actions:

- **`LodestarObserver`** (`arn:aws:iam::AWS_ACCOUNT_ID:role/LodestarObserver`):
  - **Purpose**: Default agent and MCP profile (`lodestar-observer-mcp`).
  - **Trust Relationship**: Assumed by IAM user `lodestar-agent`.
  - **Permissions**: Read-only EC2 inventory inspection (`ec2:Describe*`).
  - **Explicitly Denies**: EC2 provisioning, instance termination, SSM commands/sessions, IAM changes, and security group alterations.
- **`LodestarProvisioner`** (`arn:aws:iam::AWS_ACCOUNT_ID:role/LodestarProvisioner`):
  - **Purpose**: Reserved for explicitly authorized administrative provisioning and destruction.
  - **Trust Relationship**: Trusted **strictly by `lodestar-admin` with MFA**. An **explicit IAM deny** blocks `lodestar-agent` from assuming this role. All prior provisioning sessions have been revoked.
  - **Permissions**: Scoped EC2 creation/termination guarded by mandatory tags (`Project=Lodestar`, `ExpiresAt=<ISO8601>`) and SSM Session Manager.
  - **Separately Authorized Execution**: AI agents have zero access to this role. Execution requires human administrator authentication from an interactive terminal.

### Credential Process & Session Refresh Mechanics
Both profiles use the AWS CLI's native `export-credentials --format process` bridge to export JSON credentials for MCP servers (`uvx mcp-proxy-for-aws`) without hardcoding secrets:
- When the human operator authenticates with MFA via terminal (`aws sts get-caller-identity --profile <profile>`), AWS CLI caches the temporary STS session in `~/.aws/cli/cache/<hash>.json` for 1 hour.
- During this window, `lodestar-observer-mcp` exports valid credentials instantaneously without prompting.
- **MFA Session Expiration Rule**: When the STS cache expires, unattended background refresh **cannot** be guaranteed because AWS CLI requires interactive 6-digit TOTP input on `CONIN$`. An operator must re-run the `sts get-caller-identity` command in a visible terminal to refresh the cache.

### Privileged Operation Procedure (Zero-Credential Exposure)
To initiate privileged provisioning without exposing administrator credentials to an AI agent:
1. The AI agent generates and audits the proposed plan in read-only mode (`Lodestar-Fleet.ps1 plan` or `terraform plan`).
2. The agent presents the execution plan and exact cost impact to the administrator.
3. The administrator inspects the plan in their own terminal.
4. The administrator executes the privileged command directly in their terminal (`.\scripts\Deploy.ps1` or `.\scripts\Destroy.ps1`) using their MFA token.
5. The administrator does not share root, `lodestar-admin`, or raw credentials with the agent.

---

## 2. 48-Hour Automated Multi-Cloud Cleanup Architecture

To ensure strict zero-leakage cost discipline, an independently enforced automated cleanup system terminates ephemeral test infrastructure after 48 hours:

```
+-----------------------------------------------------------------------------------+
|                        48-Hour Independent Cleanup System                         |
+-----------------------------------------------------------------------------------+
                                   |
         +-------------------------+-------------------------+
         |                                                   |
         v                                                   v
+----------------------------------+     +------------------------------------------+
|      AWS Cloud Expiration Guard  |     |          Akamai Watchdog Routine         |
|      (DEPLOYED & ACTIVE)         |     |          (AWS Lambda via Secrets Mgr)    |
+----------------------------------+     +------------------------------------------+
| - Stack: lodestar-expiration-    |     | - Stack: lodestar-akamai-watchdog.yaml   |
|   guard (CREATE_COMPLETE)        |     | - Secret: lodestar/akamai-token          |
| - Lambda: lodestar-aws-          |     | - Lambda: lodestar-akamai-expiration-    |
|   expiration-guard (Active)      |     |   guard (rate: 30m)                      |
| - Schedule: lodestar-expiration- |     | - Target: Linode REST API v4             |
|   guard-quarter-hour (15m)       |     | - Filter: Tags "lodestar","test", >=48h  |
| - Alarm: lodestar-expiration-    |     | - Action: DELETE Linodes, verify 404     |
|   guard-errors (OK)              |     | - Cleanup: Orphaned firewalls (0 devs)   |
| - Verification: Instance waiter  |     | - Drift: terraform apply -refresh-only   |
+----------------------------------+     +------------------------------------------+
```

---

## 3. Unified Multi-Cloud CLI Tool (`Lodestar-Fleet.ps1`)
A standardized PowerShell control script is available at:
`$env:LODESTAR_REPO_ROOT\infra\akamai\terraform\scripts\Lodestar-Fleet.ps1`

Supported commands:
```powershell
Set-Location "$env:LODESTAR_REPO_ROOT\infra\akamai\terraform"

# 1. Identity & Config Inspection (Read-Only)
.\scripts\Lodestar-Fleet.ps1 inspect

# 2. Live Cloud Fleet Status (Read-Only)
.\scripts\Lodestar-Fleet.ps1 status

# 3. Cost Estimator (Hourly & 48h projections)
.\scripts\Lodestar-Fleet.ps1 cost -Hours 48

# 4. Multi-Cloud Planning & Expiration Dry-Run (Read-Only)
.\scripts\Lodestar-Fleet.ps1 plan -Hours 48

# 5. Remote Shell Access
.\scripts\Lodestar-Fleet.ps1 access -Target <AWS_INSTANCE_ID>    # Via AWS SSM (admin)
.\scripts\Lodestar-Fleet.ps1 access -Target <AKAMAI_PUBLIC_IP>   # Via SSH

# 6. Reconcile Watchdog Cloud State Drift (Drops 404 instances)
.\scripts\Lodestar-Fleet.ps1 reconcile

# 7. Deploy / Provision (Enforces Human Approval Gate)
.\scripts\Lodestar-Fleet.ps1 deploy

# 8. Destroy / Decommission (Enforces Human Approval Gate)
.\scripts\Lodestar-Fleet.ps1 destroy
```

### Direct AWS CLI Operations
```powershell
aws sts get-caller-identity --profile lodestar-provisioner
aws ec2 describe-instances --region us-east-1 --profile lodestar-provisioner --query "Reservations[].Instances[].[InstanceId,State.Name,InstanceType,Tags]" --output table
```

### B. Status & Health
```powershell
# EC2 status checks
aws ec2 describe-instance-status --region us-east-1 --profile lodestar-provisioner

# Systems Manager agent status
aws ssm describe-instance-information --region us-east-1 --profile lodestar-provisioner --query "InstanceInformationList[].[InstanceId,PingStatus,PlatformName,PlatformVersion,IPAddress]" --output table
```

### C. Remote Access (Zero Open Inbound Ports)
Inbound security group `sg-0db038e39ea004fb2` denies all inbound traffic. Connect exclusively via AWS Systems Manager Session Manager:
```powershell
aws ssm start-session --target <INSTANCE_ID> --region us-east-1 --profile lodestar-provisioner
```

### D. Safe Reboot
```powershell
aws ec2 reboot-instances --instance-ids <INSTANCE_ID> --region us-east-1 --profile lodestar-provisioner
```

### E. Provisioning (Requires Explicit Human Consent)
The restricted IAM role policy `LodestarRestrictedEC2` enforces:
- Approved AMI only (Windows Server 2022 / 2025 Base)
- Subnet: `subnet-0cced923e2d20e864`
- Security Group: `sg-0db038e39ea004fb2`
- Instance Type: `c6a.xlarge`
- Encrypted root volume <= 100 GiB
- Required tags: `Project=Lodestar`, `Environment=Test`, `ManagedBy=Terraform`, `ExpiresAt=<ISO8601>`
- Attached role: `arn:aws:iam::AWS_ACCOUNT_ID:role/lodestar-foundation-SsmInstanceRole-HRnWC1gAZsqn`

### F. Terminate / Destroy (Requires Explicit Human Consent)
```powershell
aws ec2 terminate-instances --instance-ids <INSTANCE_ID> --region us-east-1 --profile lodestar-provisioner
```

---

## 2. Akamai Cloud Operations (Terraform Fleet)

Project directory:
`$env:LODESTAR_REPO_ROOT\infra\akamai\terraform`

### A. Inspect & Preflight (Read-Only)
```powershell
Set-Location "$env:LODESTAR_REPO_ROOT\infra\akamai\terraform"
.\scripts\Preflight.ps1
```

### B. Deployment Plan (Read-Only)
```powershell
terraform plan -input=false
```

### C. Provisioning / Deploy (Requires Explicit Human Consent)
```powershell
.\scripts\Deploy.ps1
```
*Prompts the user to review the full plan and type `DEPLOY <N> LINODES`.*

### D. Status & Outputs
```powershell
terraform output
```

### E. Remote Access
```powershell
# Main VM:
ssh -i "$HOME\.ssh\lodestar_akamai_ed25519" lodestar@<MAIN_PUBLIC_IP>

# Worker VM:
ssh -i "$HOME\.ssh\lodestar_akamai_ed25519" lodestar@<WORKER_PUBLIC_IP>
```

### F. Destroy Fleet (Stops All Billing)
```powershell
.\scripts\Destroy.ps1
```
*Prompts the user to type `DESTROY LODESTAR`.*
> **Important:** Powering off Linodes does NOT stop billing. Always execute `Destroy.ps1` to terminate compute resources and stop charges.

---

---

## 4. Setup, Recovery, and Teardown Procedures

### A. Routine Operations (Read-Only Observer Mode)
For routine inspection, status checks, and dry-run planning, agents operate under `LodestarObserver`:
```powershell
Set-Location "$env:LODESTAR_REPO_ROOT\infra\akamai\terraform"

# Run automated validation test suite
.\tests\Test-LodestarAutomation.ps1

# Inspect multi-cloud configuration & identities
.\scripts\Lodestar-Fleet.ps1 inspect

# Check fleet status
.\scripts\Lodestar-Fleet.ps1 status
```

### B. AWS Windows Setup & Recovery Procedures
1. **Interactive MFA Authentication**:
   ```powershell
   aws sts get-caller-identity --profile lodestar-provisioner
   ```
2. **Pre-flight & Dry-Run**:
   ```powershell
   .\scripts\Deploy-AwsWindows.ps1 -DryRun -Hours 48
   ```
3. **Privileged Deployment**:
   ```powershell
   .\scripts\Deploy-AwsWindows.ps1 -Hours 48
   ```
   Requires typing confirmation phrase: `DEPLOY 1 AWS WINDOWS INSTANCE`. State is tracked in `state\aws-fleet-state.json`.
4. **Recovery from Partial Deployments**:
   If an unexpected failure or disconnection occurs during provisioning:
   - Run `.\scripts\Deploy-AwsWindows.ps1 -DryRun`: The script scans live EC2 inventory for untracked instances tagged `Project=Lodestar`.
   - If an active instance exists, the script halts deployment and displays the instance details.
   - Run `.\scripts\Destroy-AwsWindows.ps1` to cleanly terminate orphaned resources before reprovisioning.
5. **Teardown**:
   ```powershell
   .\scripts\Destroy-AwsWindows.ps1
   ```
   Requires typing confirmation phrase: `DESTROY 1 AWS WINDOWS INSTANCE`.

### C. Akamai Linux Pilot Setup & Teardown
1. **Interactive Preflight**:
   ```powershell
   .\scripts\Invoke-Preflight.ps1
   ```
2. **Privileged Deployment**:
   ```powershell
   .\scripts\Deploy.ps1
   ```
   Requires typing confirmation phrase: `DEPLOY 2 LINODES`.
3. **Teardown**:
   ```powershell
   .\scripts\Destroy.ps1
   ```
   Requires typing confirmation phrase: `DESTROY LODESTAR`.

### D. Automated 48-Hour Cleanup Watchdogs
- **AWS Watchdog (Live Deployed CloudFormation Stack)**:
  - Infrastructure stack: `lodestar-expiration-guard` (Status: `CREATE_COMPLETE`)
  - Authoritative repository definition: ``cleanup\aws\lodestar-expiration-guard.yaml``
  - Active Lambda: `lodestar-aws-expiration-guard`
  - Enabled Schedule: `lodestar-expiration-guard-quarter-hour` (`rate(15 minutes)`)
  - CloudWatch Alarm: `lodestar-expiration-guard-errors` (Status: `OK`)
  - *Status*: **Live & active in AWS account AWS_ACCOUNT_ID**.
- **Akamai Watchdog (AWS Lambda via Secrets Manager)**:
  - Infrastructure definition: ``cleanup\akamai\lodestar-akamai-watchdog.yaml``
  - Lambda code: ``cleanup\akamai\lambda\akamai_reaper.py``
  - Local runner: ``cleanup\akamai\Invoke-AkamaiReaper.ps1``
  - Retrieves token securely from AWS Secrets Manager (`lodestar/akamai-token`).
  - Deletes instances older than 48 hours, verifies HTTP 404, and cleans up empty firewalls.
  - *Status*: **Code complete; prepared for deployment**.

### E. Handling Terraform State Drift
When the Akamai Watchdog terminates Linodes outside Terraform, the local state file retains obsolete instance records.
To reconcile state cleanly without accidentally reprovisioning:
```powershell
# 1. Preview drift (detects 404 instances to drop from state)
terraform plan -refresh-only

# 2. Reconcile state file with live cloud
terraform apply -refresh-only

# Alternatively, run via unified fleet tool:
.\scripts\Lodestar-Fleet.ps1 reconcile
```

---

## 5. Mandatory Safety Guardrails

1. **Human Authorization Gate**:
   - Billable provisioning (`Deploy.ps1`, `ec2:RunInstances`) requires explicit confirmation.
   - Cloud resource deletion (`Destroy.ps1`, `ec2:TerminateInstances`) requires explicit confirmation.
2. **Credential Redaction**:
   - `LINODE_TOKEN` is loaded strictly into ephemeral process memory and never written to files, logs, or MCP JSON.
   - AWS credentials originate from short-term temporary tokens via AWS Sign-in or STS AssumeRole.
3. **Restricted Privileges**:
   - AI agents must never be granted administrator privileges. Agents operate under `LodestarProvisioner` (AWS) and scoped Linode PATs (Akamai).
4. **Expiration Safeguard**:
   - All AWS test instances must carry an `ExpiresAt` tag to enforce automated cleanup of test VMs.

### Local repository environment

Set the local `LODESTAR_REPO_ROOT` environment variable to the checkout root before running the examples. The repository deliberately does not embed a personal Windows home path or a specific AWS account ID. `AWS_ACCOUNT_ID` in ARN examples is a placeholder. Privileged scripts require `LODESTAR_AWS_ACCOUNT_ID` from local configuration and verify the administrator's AWS identity before provisioning.
