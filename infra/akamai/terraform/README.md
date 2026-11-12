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
- Root login by SSH is disabled by cloud-init; `lodestar` has the supplied public key and passwordless sudo. Do not grant the crawler container or VPN credentials sudo access.
- Region is fixed to `us-east` (Newark): the 48-hour watchdog only reaps Linodes there, so Terraform refuses any other region.
- Linode API token is read through the **local** `LINODE_TOKEN` environment variable; never placed in Terraform configuration or metadata.
- No PIA credentials, WireGuard keys or torrent metadata are stored in cloud-init, Terraform state, logs or this repository.
- OS: Ubuntu 24.04 LTS. cloud-init installs Docker Engine and Compose from Docker's **official apt repository**, plus WireGuard tools and security updates. It does **not start the crawler or VPN**.
- Enabled Linode disk encryption is requested; an unsupported region will fail provisioning instead of silently disabling it.
- No chargeable backups, Block Storage, NodeBalancers or Kubernetes clusters are deployed.
- All resources are tagged `lodestar`, `test`, `managed-terraform`.

**VPN caveat:** A unique Akamai public IPv4 address per worker does NOT mean a unique external DHT source IP. For distinct observable source IPs, configure different **PIA exit IPs per worker** and verify them. PIA's U.S. exits do not support port forwarding; outbound DHT lookups may work, but unsolicited inbound traffic typically will not.

## Prerequisites on Windows 11 (PowerShell)

1. Install Terraform **1.10 or later** locally if needed (1.10 added the S3 state locking these scripts use). Official install: https://developer.hashicorp.com/terraform/install. You must have a working `terraform` command. `ssh-keygen` is provided by Windows OpenSSH on many installations. Check:

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

5. **Point Terraform at the shared state.** Terraform keeps its record of the machines it created (its *state*) in the fleet's private S3 bucket, shared with the automated control plane, so the two never disagree about which Linodes exist. The scripts refuse to run without these two settings and never fall back to a local state file. In the same PowerShell session:

   ```powershell
   $env:LODESTAR_TF_STATE_BUCKET = '<StateBucketName from the fleet-control stack outputs>'
   $env:LODESTAR_TF_STATE_REGION = 'us-east-1'
   $env:AWS_PROFILE = '<your admin profile that may use that bucket>'
   ```

   The bucket name is private configuration: keep it out of the repository and chat.

   **If you used these scripts before, with a local state file:** `terraform init` stops, because the state location changed. Never copy the old local state into the bucket (the cloud-operator plan: a fresh S3 state is the reconciliation, and no old local state is imported). Instead:
   1. In Cloud Manager, check that no Lodestar Linodes or firewalls exist. If any do, delete them there, or run the old `Destroy.ps1` from a checkout before this change.
   2. Delete the `.terraform` folder and every `terraform.tfstate*` file in this folder.
   3. Run the script again. If Terraform ever offers to copy existing state, answer **no**.

   **Shared with the control plane:** the automated fleet (`fleet.yml`) uses the same state. Don't run `Deploy.ps1` or `Destroy.ps1` while a fleet it created exists: they would change that fleet outside the control plane's lock. Use the fleet's `down` instead.

6. In `infra/akamai/terraform`, copy the example and edit the configuration:

   ```powershell
   Copy-Item .\terraform.tfvars.example .\terraform.tfvars
   notepad .\terraform.tfvars
   ```

   Replace `203.0.113.20/32` with your actual *current management public IPv4* followed by `/32`. You can check your egress IP with `Invoke-RestMethod https://checkip.amazonaws.com` (this external service sees your current public IP). If you change commercial VPN exit IPs, SSH access may be blocked until you update the firewall via Terraform or use Akamai LISH console.

7. Run the **read-only** preflight and note any API permission/region failure:

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

### PIA WireGuard gateway

The `pia-example/` directory contains **only a VPN gateway Compose skeleton**, not your crawler and not credentials. PIA's official native WireGuard setup involves obtaining credentials and negotiating a WireGuard configuration. Keep this local and use the official project: https://github.com/pia-foss/manual-connections.

Gluetun's current PIA integration supports OpenVPN natively; PIA WireGuard requires using its **custom WireGuard** configuration. This file must be securely transferred directly to the host from your machine. Do not upload it to GitHub, Terraform, cloud-init, or any chat or AI tool. The corresponding Docker Compose example expects `pia-example/secrets/wg0.conf`, which is not in the repository (and is git-ignored).

Never start Lodestar until you verify a working PIA exit IP, Gluetun's kill switch, DNS, and IPv6 behavior. Gluetun's kill switch blocks new connections while the tunnel is down, but on its own (v3.41.3) it lets packets of connections opened earlier leave through the host's network. `compose.vpn.yaml` mounts Lodestar's post-rules (`packaging/companions/gluetun-post-rules.txt`) to close that; keep them and `VPN_INTERFACE: wg0`. If the VPN container fails or stops, the crawler must not fall back to the host network. With `network_mode: "service:gluetun"`, Gluetun handles the crawler's network namespace when configured appropriately. Verify this at runtime and test by deliberately stopping the VPN tunnel **without** putting test packets on the public network.

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
