locals {
  tags                 = ["lodestar", "test", "managed-terraform"]
  main_hourly_usd      = 0.072
  worker_hourly_usd    = 0.0075
  estimated_hourly_usd = (var.deploy_main ? local.main_hourly_usd : 0) + var.worker_count * local.worker_hourly_usd
  ssh_authorized_key   = trimspace(file(pathexpand(var.ssh_public_key_path)))
}

# Separate main and worker firewalls, so all rules are in place
# BEFORE the VM is created (firewall_id is passed during instance create).
resource "linode_firewall" "main" {
  count           = var.deploy_main ? 1 : 0
  label           = "lodestar-main-fw-${var.deployment_id}"
  inbound_policy  = "DROP"
  outbound_policy = "ACCEPT"
  tags            = local.tags

  inbound {
    label    = "ssh-admin-only"
    action   = "ACCEPT"
    protocol = "TCP"
    ports    = "22"
    ipv4     = [var.ssh_admin_ipv4_cidr]
  }
}

resource "linode_firewall" "workers" {
  count           = var.worker_count > 0 ? 1 : 0
  label           = "lodestar-worker-fw-${var.deployment_id}"
  inbound_policy  = "DROP"
  outbound_policy = "ACCEPT"
  tags            = local.tags

  inbound {
    label    = "ssh-admin-only"
    action   = "ACCEPT"
    protocol = "TCP"
    ports    = "22"
    ipv4     = [var.ssh_admin_ipv4_cidr]
  }
}

resource "linode_instance" "main" {
  count                = var.deploy_main ? 1 : 0
  label                = "lodestar-main-${var.deployment_id}"
  image                = var.image
  interface_generation = "legacy_config" # New accounts default to Linode Interfaces. Explicit legacy is needed for firewall_id.
  region               = var.region
  type                 = "g6-standard-4" # 4 vCPU, 8 GB, 160 GB SSD
  authorized_keys      = [local.ssh_authorized_key]
  firewall_id          = linode_firewall.main[0].id
  swap_size            = 512
  backups_enabled      = false
  private_ip           = false
  disk_encryption      = "enabled"
  tags                 = concat(local.tags, ["main"])

  metadata {
    user_data = base64encode(templatefile("${path.module}/cloud-init/ubuntu24.yaml.tftpl", {
      ssh_public_key = local.ssh_authorized_key
      role_label     = "main"
    }))
  }

  lifecycle {
    precondition {
      condition     = local.estimated_hourly_usd <= var.max_hourly_compute_usd
      error_message = "Compute hourly budget threshold exceeded. Review the plan and cost before continuing."
    }
  }
}

resource "linode_instance" "workers" {
  count                = var.worker_count
  label                = "lodestar-worker-${format("%02d", count.index + 1)}-${var.deployment_id}"
  image                = var.image
  interface_generation = "legacy_config" # New accounts default to Linode Interfaces. Explicit legacy is needed for firewall_id.
  region               = var.region
  type                 = "g6-nanode-1" # 1 vCPU, 1 GB, 25 GB SSD
  authorized_keys      = [local.ssh_authorized_key]
  firewall_id          = linode_firewall.workers[0].id
  swap_size            = 256
  backups_enabled      = false
  private_ip           = false
  disk_encryption      = "enabled"
  tags                 = concat(local.tags, ["worker"])

  metadata {
    user_data = base64encode(templatefile("${path.module}/cloud-init/ubuntu24.yaml.tftpl", {
      ssh_public_key = local.ssh_authorized_key
      role_label     = "worker-${format("%02d", count.index + 1)}"
    }))
  }

  lifecycle {
    precondition {
      condition     = local.estimated_hourly_usd <= var.max_hourly_compute_usd
      error_message = "Compute hourly budget threshold exceeded. Review the plan and cost before continuing."
    }
  }
}
