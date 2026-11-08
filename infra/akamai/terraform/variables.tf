variable "region" {
  type        = string
  description = "Akamai Cloud region. us-east is Newark, NJ. Confirm region/account capacity before apply."
  default     = "us-east"

  validation {
    condition     = length(trimspace(var.region)) > 0
    error_message = "region must not be empty."
  }
}

variable "image" {
  type        = string
  description = "Official OS image. Cloud-init bootstrap is specifically for Ubuntu 24.04."
  default     = "linode/ubuntu24.04"

  validation {
    condition     = var.image == "linode/ubuntu24.04"
    error_message = "The included bootstrap currently supports linode/ubuntu24.04 only."
  }
}

variable "deployment_id" {
  type        = string
  description = "Short unique label suffix so multiple testing batches never clash."
  default     = "test1"

  validation {
    condition     = can(regex("^[a-z0-9][a-z0-9-]{0,15}$", var.deployment_id))
    error_message = "deployment_id must be 1-16 lowercase letters, numbers or dashes."
  }
}

variable "ssh_public_key_path" {
  type        = string
  description = "Local public OpenSSH Ed25519 key file. Do not supply a private key."
  default     = "~/.ssh/lodestar_akamai_ed25519.pub"
}

variable "ssh_admin_ipv4_cidr" {
  type        = string
  description = "Your management public IPv4 as a /32, such as 203.0.113.20/32. Never 0.0.0.0/0."

  validation {
    condition     = can(cidrnetmask(var.ssh_admin_ipv4_cidr)) && can(regex("/32$", var.ssh_admin_ipv4_cidr))
    error_message = "Use one valid management IPv4 in /32 form."
  }
}

variable "deploy_main" {
  type        = bool
  description = "Enable main VM. Default false so first plan cannot accidentally start compute."
  default     = false
}

variable "worker_count" {
  type        = number
  description = "Number of Nanode workers to create. Default zero, maximum six."
  default     = 0

  validation {
    condition     = var.worker_count >= 0 && var.worker_count <= 6 && floor(var.worker_count) == var.worker_count
    error_message = "worker_count must be an integer between 0 and 6."
  }
}

variable "max_hourly_compute_usd" {
  type        = number
  description = "Safety threshold for main+worker compute hourly cost, excluding network/other services."
  default     = 0.12

  validation {
    condition     = var.max_hourly_compute_usd > 0 && var.max_hourly_compute_usd <= 0.50
    error_message = "max_hourly_compute_usd must be greater than zero and at most 0.50 USD."
  }
}
