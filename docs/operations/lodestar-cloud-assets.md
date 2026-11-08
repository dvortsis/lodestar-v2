# Lodestar cloud infrastructure: operational source of truth

Decision record: `docs/decisions/2026-11-08-X-cloud-infra.md`. Agents only read; every deploy, reboot, remote session or delete is run by a human (`.agents/skills/lodestar-fleet/SKILL.md`).

## Deployed services

| Monitored provider | AWS CloudFormation stack | Lambda | Schedule |
|---|---|---|---|
| AWS Windows EC2 test | `lodestar-expiration-guard` | `lodestar-aws-expiration-guard` | 15 minutes |
| Akamai Linode | `lodestar-akamai-watchdog` | `lodestar-akamai-expiration-guard` | 30 minutes |

Both services were reported active when the repository copy was reconciled on 2026-11-08; this repository has not re-checked them. The Akamai token is stored **only** in AWS Secrets Manager (`lodestar/akamai-token`). The SNS email subscription is confirmed; the AWS expiration guard's `NotificationEmail` uses `NoEcho` in the current live template. Routine agents use `LodestarObserver` and cannot assume `LodestarProvisioner`.

## Canonical templates

- `infra/aws/expiration-guard/deployed/template.json`: built from `index.py` by `build_template.py` (tests: `test_guard.py`); derived from live `lodestar-expiration-guard`, with the real VPC and Windows security group IDs replaced by **required** CloudFormation parameters. The private email address was already converted to `NotificationEmail` (`NoEcho`) in the deployed stack. Do not add literal parameter defaults.
- `infra/akamai/watchdog/deployed/lodestar-akamai-watchdog.json`: matched the live `lodestar-akamai-watchdog` template before its account-specific SNS ARN was converted to `Fn::Sub` using AWS pseudo-parameters. `index.py` matched the deployed inline Lambda code at reconciliation time. Since then the source has changed in two small ways (a dead `/networking/firewalls?page=` allowlist entry removed, and `TOKEN_REQUIRED` added so a missing token raises an alarm), so the deployed copy differs until the next stack update.
- `infra/akamai/terraform`: Akamai instance Terraform. Keep `.terraform.lock.hcl`; never commit `.tfstate`, `terraform.tfvars`, credentials or `.terraform/`.
- `.agents/skills/lodestar-fleet`: repository copy of the agent skill; synchronize deliberately to the global execution copy outside Git.
- The skill and the PowerShell scripts hold **no** account IDs or resource IDs (AMI, subnet, security group, instance role or profile). The scripts read them from the operator's local `LODESTAR_AWS_*` environment variables or parameters and have no defaults.

These public source templates are **semantically parameterized representations**, not byte-identical snapshots of the live stack. The finalizer did not deploy them, update the AWS stacks, or change the Linode fleet. Before any future CloudFormation update, review a change set and supply existing parameter values securely. In particular, updating the AWS expiration-guard source requires `NotificationEmail`, `LodestarVpcId` and `WindowsSecurityGroupId`. Do not infer an email address from the repository.

## Excluded from public source control

Alternative watchdog drafts, a former local AWS guard candidate and outdated cloud scripts were backed up on the operator's machine and left out of the repository. No original Terraform operational state or credentials were moved.

## Constraints and remaining tests

- The two automatic reapers have passed non-destructive inventory/dry-run checks; **actual expired-instance deletion remains unverified**. Do not present untested deletion as proven.
- The Akamai watchdog does not automatically remove detached firewalls, volumes or reserved addresses.
- The migrated Terraform directory must **not** execute `terraform apply` until private state and working-directory ownership are reconciled. A new empty state may recreate machines, and out-of-band expirations can likewise trigger recreation.
- Never grant an unrestricted local coding agent access to administrator cached credentials in the same Windows session. IAM role separation alone is not a local process sandbox.
- Secret scans are best-effort. Before pushing a public branch, review staged changes and Gitleaks results, and confirm all Terraform and Git validations.

## Secure PIA VPN credential delivery and leak protection

The Private Internet Access (PIA) VPN integration delivers least-privilege egress protection for disposable Linodes without distributing AWS credentials to the test fleet:

1. **Central Credential Store**: AWS Secrets Manager `lodestar/pia-vpn` in `us-east-1` stores master keys `PIA_USERNAME` and `PIA_PASSWORD`.
2. **Restricted IAM Role**: `LodestarPiaSecretReader` trusts `lodestar-admin` with MFA and allows only `secretsmanager:GetSecretValue` on `lodestar/pia-vpn`. All other secrets and AWS actions are denied. Temporary STS sessions are capped at 1 hour.
3. **Zero AWS Credentials on Linode**: Linodes never connect to AWS Secrets Manager and hold no AWS access keys, session tokens, or IAM credentials.
4. **Verified Cryptography & Dynamic Discovery**: The deployment tool authenticates to the PIA REST API using temporary STS credentials, generates an ephemeral Curve25519 WireGuard keypair using verified X25519 (`cryptography` library matching RFC 7748 test vectors), discovers live WireGuard endpoints via PIA's authoritative server list (`https://serverlist.piaservers.net/vpninfo/servers/v4`), and registers the public key with the designated East Coast PIA server (`us-east`, `us-new-jersey`, `us-new-york`).
5. **Runtime Storage in `/run` tmpfs**: The resulting `wg0.conf` is streamed over host-key-verified SSH directly into `/run/lodestar/vpn/wg0.conf` (permissions `0600 root:root`, directory `0700 root:root`). Because `/run` is mounted as tmpfs (RAM), credentials are never written to disk.
6. **Auditable Sudo Execution Model**: Scoped sudoers permissions (`/etc/sudoers.d/99-lodestar-automation`) allow `lodestar` user to run required deployment binaries (`docker`, `install`, `rm`, `stat`, `sha256sum`, `test`) with `sudo -n` without granting unrestricted `docker` group membership.
7. **Compose Transfer and Integrity Check**: The canonical Docker Compose file is validated locally, transferred over SSH, checksum-verified (`sha256sum`), and validated remotely with `docker compose config` before VPN startup.
8. **Reboot and Lifecycle Behavior**: The VPN configuration survives container restarts while the VM remains active. On VM reboot or shutdown, `/run` is wiped automatically. To re-provision after reboot, the operator runs `Lodestar-Fleet.ps1 vpn-provision`.
9. **Pinned Immutable Container**: Gluetun runs pinned to verified multi-platform index digest `qmcgaw/gluetun:v3.40.0@sha256:2b42bfa046757145a5155acece417b65b4443c8033fb88661a8e9dcf7fda5a00` (Linux AMD64 manifest `sha256:8ec31c4b83e1f3173d43728f78a4a7ad45237fef366a4b5461e5aee93f517fba`) with `FIREWALL=on` (strict kill switch), `sysctls: net.ipv6.conf.all.disable_ipv6=1`, `no-new-privileges:true`, and minimal capabilities (`NET_ADMIN` only).
10. **Independent Leak Gate**: Before crawler startup, independent checks inside the container verify active WireGuard handshake, egress IP != host public IP, non-Akamai ISP/ASN, internal DNS resolution, and iptables `OUTPUT DROP` enforcement. If any check fails, the crawler refuses to start and containers are stopped.
11. **Zero Plaintext Secret Exposure**: Private keys, tokens, and passwords are redacted from logs, stdout, and error output. Configuration export requires explicit output path.
