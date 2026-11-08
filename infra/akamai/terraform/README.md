# Lodestar on Akamai Cloud: disposable Linux test fleet

**Prepared 2026-11-07. Nothing in this folder deploys anything on its own.** Lives in the repository at `infra/akamai/terraform`; the AWS-side skill and the watchdog are described in `docs/operations/lodestar-cloud-assets.md`.

> **Do not run `terraform apply` from a fresh checkout.** The live fleet's Terraform state is private and not in Git. An empty state can create a second fleet. See "Constraints and remaining tests" in `docs/operations/lodestar-cloud-assets.md`.

This folder uses HashiCorp Terraform to create up to **one Linux main server plus six Linux worker servers** in Akamai Cloud (Linode). AWS remains responsible for the separate Windows Server. The setup is **deliberately off by default** (`deploy_main=false`, `worker_count=0`).

## Actual compute specifications and current base price

| Role | Plan ID | vCPU | RAM | SSD allocation | Public IPv4 | USD/hour | Transfer |
|---|---|---:|---:|---:|---|---:|---:|
| Main | `g6-standard-4` | 4 | 8 GB | 160 GB | One distinct IPv4 | $0.0720 | 5 TB/mo |
| Worker | `g6-nanode-1` | 1 | 1 GB | 25 GB | One distinct IPv4 each | $0.0075 | 1 TB/mo each |

**48-hour compute cost:** main $3.456, six workers $2.16, total **$5.616**, subject to Akamai's hourly rounding; this excludes overage, VPN subscription and other options. **One main + one worker for 48 hours:** $3.816. New accounts may have quotas below seven active instances; approval does not prove capacity. This configuration explicitly uses legacy configuration-profile interfaces because the `firewall_id` API argument applies to those interfaces. New accounts created after April 9, 2026 normally default to newer Linode interfaces but permit legacy interfaces; the preflight refuses to proceed if your account has been restricted to `linode_only`. It does not modify that account-wide setting. Standard plans bill until **DELETED, even when shutdown**.

Akamai's published plan description is *SSD*. This is not an assurance of a particular random I/O latency or local NVMe device. This setup uses the plan's **included filesystem** and does not attach paid Block Storage volumes. Verify storage behavior with your own benchmark, especially for the main search index.

## Security properties

- Akamai Cloud Firewall is **attached at Linode creation** (`firewall_id`) using explicit `interface_generation="legacy_config"`, inbound DROP except SSH TCP/22 from **your single explicit `/32`** (`ssh_admin_ipv4_cidr`). The main VM alone may also allow up to four more `/32`s in `main_extra_admin_ipv4_cidrs`, for example the AWS Windows VM's public IPv4 so it can SSH-tunnel to the UI. Every entry is validated as an IPv4 `/32`, and there is no IPv6 ingress. No inbound 8090/tcp or 6881/udp is opened on the Akamai host.
- Root login by SSH is disabled by cloud-init; `lodestar` has the supplied public key and scoped non-interactive sudo limited strictly to the root deployment helper (`/usr/local/sbin/lodestar-vpn-helper`). Direct sudo access to `docker`, `rm`, `install`, or `systemctl` is eliminated to enforce a strict privilege boundary. Do not grant the crawler container or VPN credentials sudo access.
- Region is fixed to `us-east` (Newark): the 48-hour watchdog only reaps Linodes there, so Terraform refuses any other region.
- Linode API token is read through the **local** `LINODE_TOKEN` environment variable; never placed in Terraform configuration or metadata.
- No PIA credentials, WireGuard keys or torrent metadata are stored in cloud-init, Terraform state, logs or this repository.
- OS: Ubuntu 24.04 LTS. cloud-init installs Docker Engine and Compose from Docker's **official apt repository**, plus WireGuard tools and security updates. It does **not start the crawler or VPN**.
- Enabled Linode disk encryption is requested; an unsupported region will fail provisioning instead of silently disabling it.
- No chargeable backups, Block Storage, NodeBalancers or Kubernetes clusters are deployed.
- All resources are tagged `lodestar`, `test`, `managed-terraform`.

**VPN caveat:** A unique Akamai public IPv4 address per worker does NOT mean a unique external DHT source IP. For distinct observable source IPs, configure different **PIA exit IPs per worker** and verify them. PIA's U.S. exits do not support port forwarding; outbound DHT lookups may work, but unsolicited inbound traffic typically will not.

## Prerequisites on Windows 11 (PowerShell)

1. Install Terraform locally if needed. Official install: https://developer.hashicorp.com/terraform/install. You must have a working `terraform` command. `ssh-keygen` is provided by Windows OpenSSH on many installations. Check:

   ```powershell
   terraform version
   ssh-keygen -?  # May exit nonzero while displaying usage; this is just a presence check
   ```

2. Generate a **local key pair**. Supply a passphrase when prompted:

   ```powershell
   New-Item -ItemType Directory -Path "$HOME\.ssh" -Force | Out-Null
   ssh-keygen -t ed25519 -a 100 -f "$HOME\.ssh\lodestar_akamai_ed25519" -C "lodestar-akamai"
   ```

   Do not paste your private key anywhere.

3. In Akamai Cloud Manager, create a **Personal Access Token** with the minimum scopes your deployment requires: `linodes:read_write`, `firewall:read_write`, `images:read_only`, and `account:read_only` as required by API calls. Token management: https://cloud.linode.com/profile/tokens. If a scope is unavailable or Terraform requires more, resolve it deliberately rather than granting all scopes automatically. Set an expiry. Store it in a password manager.

4. In a fresh **local PowerShell** session, load the token *without printing it or committing it*. For example, copy it from your password manager then read it as a SecureString:

   ```powershell
   $secret = Read-Host 'Paste Akamai API token (hidden)' -AsSecureString
   $env:LINODE_TOKEN = [System.Net.NetworkCredential]::new('', $secret).Password
   Remove-Variable secret
   ```

   The environment variable still resides in your current process and any processes it launches. Close PowerShell to clear it. Never run debug logging that could disclose it.

5. In `infra/akamai/terraform`, copy the example and edit the configuration:

   ```powershell
   Copy-Item .\terraform.tfvars.example .\terraform.tfvars
   notepad .\terraform.tfvars
   ```

   Replace `203.0.113.20/32` with your actual *current management public IPv4* followed by `/32`. You can check your egress IP with `Invoke-RestMethod https://checkip.amazonaws.com` (this external service sees your current public IP). If you change commercial VPN exit IPs, SSH access may be blocked until you update the firewall via Terraform or use Akamai LISH console.

6. Run the **read-only** preflight and note any API permission/region failure:

   ```powershell
   .\scripts\Preflight.ps1
   ```

   If PowerShell script execution is blocked, inspect the scripts, then run from PowerShell 7 using `pwsh -NoProfile -File .\scripts\Preflight.ps1`. Do not disable organizational execution-policy controls to work around a block.

## Plan and deploy a pilot, then scale

Edit `terraform.tfvars`:

```hcl
deploy_main  = true
worker_count = 1
```

Then from the project root:

```powershell
.\scripts\Deploy.ps1
```

This creates a saved Terraform plan and requires you to **type `DEPLOY 2 LINODES`** before any billable resource is created. The first pilot costs approximately **$0.0795/hour**, plus network overage if applicable.

After verifying Docker, disk behavior, outbound WireGuard connectivity and account capacity, set `worker_count=6` and run the same script. Review the plan again. Terraform will add five workers; the main and existing worker remain if their configuration has not changed.

Inspect instance IPs after apply:

```powershell
terraform output
```

### First-boot diagnostics on Ubuntu

SSH into the newly created main VM using the key and IP from `terraform output`:

```powershell
ssh -i "$HOME\.ssh\lodestar_akamai_ed25519" lodestar@<MAIN_IP>
```

On the VM:

```bash
sudo cloud-init status --wait --long
sudo tail -n 100 /var/log/lodestar/bootstrap.log
sudo docker version
sudo docker compose version
ls -l /dev/net/tun
lsblk -o NAME,SIZE,TYPE,ROTA,TRAN,MOUNTPOINTS
```

`lsblk` is evidence of the guest-visible device, **not proof of physical backing media**. Validate IOPS and latency on the real search-index path using a controlled benchmark before using it in production.

### PIA WireGuard gateway and automated credential bridge

The `pia-example/` directory contains the VPN gateway Compose stack and the crawler service template. Credentials delivery is automated via the secure bridge (`scripts/Deploy-PiaVpn.ps1` and `scripts/lodestar_pia_bridge.py`):

1. **Central AWS Credential Store**: `lodestar/pia-vpn` in AWS Secrets Manager (`us-east-1`), read exclusively via the restricted `LodestarPiaSecretReader` role by the trusted deployment host with MFA enforcement.
2. **Zero Linode AWS Credentials**: No AWS keys, IAM profiles, session tokens, or Secrets Manager access on any Linode.
3. **Verified Cryptography & Dynamic Discovery**: Ephemeral WireGuard keypair generated with verified X25519 (`cryptography` library, matching RFC 7748 test vectors). WireGuard endpoints dynamically discovered via PIA authoritative server list (`serverlist.piaservers.net/vpninfo/servers/v4`).
4. **Ephemeral Runtime Storage in `/run` tmpfs**: Ephemeral `wg0.conf` is streamed directly to `/run/lodestar/vpn/wg0.conf` (root-only mode `0600` on RAM-backed `tmpfs`). No persistent plaintext credentials on disk.
5. **Constrained Privilege Boundary via Root Deployment Helper**: Sudoers permissions (`/etc/sudoers.d/99-lodestar-automation`) permit non-interactive execution strictly of `/usr/local/sbin/lodestar-vpn-helper` (mode `0750 root:root`), with parameterized subcommands and input sanitization, eliminating direct sudo access to arbitrary binaries.
6. **Root-Owned Static Compose Configuration**: The canonical Compose file lives in root-owned `/etc/lodestar/vpn/compose.yaml` (mode `0644 root:root`) pre-installed via cloud-init. Arbitrary container specifications and host directory bind mounts from unprivileged users are rejected. Crawler startup authorization is enforced directly within the privileged helper before container execution.
7. **Pinned Immutable Gluetun Digest**: Gluetun pinned to verified multi-platform index digest `sha256:2b42bfa046757145a5155acece417b65b4443c8033fb88661a8e9dcf7fda5a00` (Linux AMD64 manifest `sha256:8ec31c4b83e1f3173d43728f78a4a7ad45237fef366a4b5461e5aee93f517fba`) with IPv6 disabled and strict kill switch (`FIREWALL=on`).
8. **Independent Leak Gate**: Pre-crawler verification validates active WireGuard handshake, public egress IP mismatch against host IP, non-Akamai ISP/ASN, internal DNS resolution, and iptables `OUTPUT DROP` rules. If any check fails, containers are shut down and crawler startup is blocked.
9. **Zero Plaintext Secret Exposure**: Private keys, tokens, and passwords are strictly redacted from logs, stdout, and error traces. Local configuration export requires explicit destination path.

```powershell
# Verify AWS access to PIA secret:
.\scripts\Lodestar-Fleet.ps1 vpn-verify

# Provision WireGuard and start VPN on a Linode:
.\scripts\Lodestar-Fleet.ps1 vpn-provision -Target <LINODE_IP> -Region us-east

# Verify tunnel health and leak protection:
.\scripts\Lodestar-Fleet.ps1 vpn-status -Target <LINODE_IP>

# Start crawler behind verified VPN:
.\scripts\Lodestar-Fleet.ps1 vpn-crawler -Target <LINODE_IP>

# Clean up runtime credentials and containers:
.\scripts\Lodestar-Fleet.ps1 vpn-cleanup -Target <LINODE_IP>
```

**Networking reminder:** Cloud Firewalls only guard the host's network interface, **not traffic after it has entered the PIA tunnel**. A port such as UDP/6881 is only reachable through PIA if the particular exit permits/forwards it. Never expose the web UI on public TCP/8090. Use a restricted SSH tunnel to bind it locally after setting up the app.

## STOP BILLING: Terraform destroy is required

```powershell
.\scripts\Destroy.ps1
```

You must type **`DESTROY LODESTAR`** after reviewing the resources. This permanently deletes the managed Linodes and firewalls. Powering a Linode off does NOT stop billing. Confirm no stragglers in https://cloud.linode.com/linodes and https://cloud.linode.com/networking/firewalls.

**Important:** The separate Akamai watchdog (`infra/akamai/watchdog/deployed`) deletes in-scope Linodes 48 hours after creation, but its actual deletion path has **not** been verified end to end, and it never deletes firewalls, volumes or reserved addresses. Do not rely on it: do not run deployments unattended or beyond the planned test duration. Nothing here enforces an account spend limit. Keep the local Terraform state backed up securely during testing: deleting state before destroying resources will orphan billable VMs.

## What has NOT been verified from your account

- Your new account's Linode concurrency cap, account spend limit, or current credit/deposit status.
- Your account network interface policy. The read-only preflight checks that legacy configuration-profile interfaces remain permitted.
- Regional capacity for `g6-standard-4` and six `g6-nanode-1` instances.
- That the particular SSD plan achieves your search-index random-IOPS target.
- That Akamai accepts your precise DHT workload: the published AUP does not explicitly name it as authorized.
- An operational PIA WireGuard tunnel, port forwarding, the actual Lodestar image/MSI, or the crawling behavior.
- Anything against your account. CI checks `terraform fmt`, `terraform init` and `terraform validate` on every change (no credentials, no plan), but nothing has run `terraform plan` or `apply` from this repository.

## Authoritative references checked 2026-11-07

- Akamai pricing: https://www.akamai.com/cloud/pricing/cpu
- Akamai billing: https://techdocs.akamai.com/cloud-computing/docs/understanding-how-billing-works
- Akamai API tokens and scopes: https://techdocs.akamai.com/linode-api/reference/get-started
- Terraform provider 4.7.0: https://registry.terraform.io/providers/linode/linode/latest/docs
- Terraform `linode_instance`: https://registry.terraform.io/providers/linode/linode/latest/docs/resources/instance
- Terraform `linode_firewall`: https://registry.terraform.io/providers/linode/linode/latest/docs/resources/firewall
- Linode Cloud Firewalls: https://techdocs.akamai.com/cloud-computing/docs/cloud-firewall
- New-account network interface defaults: https://techdocs.akamai.com/cloud-computing/docs/select-network-interfaces-for-new-linodes
- Linode metadata/cloud-init: https://techdocs.akamai.com/cloud-computing/docs/add-user-data-when-deploying-a-compute-instance
- Docker Ubuntu: https://docs.docker.com/engine/install/ubuntu/
- PIA WireGuard manual: https://github.com/pia-foss/manual-connections
- Gluetun PIA support: https://github.com/qdm12/gluetun-wiki/blob/main/setup/providers/private-internet-access.md
- Gluetun custom WireGuard: https://github.com/qdm12/gluetun-wiki/blob/main/setup/providers/custom.md
- Gluetun kill switch: https://github.com/qdm12/gluetun-wiki/blob/main/faq/firewall.md
