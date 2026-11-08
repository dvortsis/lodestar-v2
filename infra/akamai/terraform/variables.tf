variable "region" {
  type        = string
  description = "Akamai Cloud region. Only us-east (Newark, NJ): the watchdog's EXPECTED_REGION reaps nothing elsewhere."
  default     = "us-east"

  validation {
    condition     = var.region == "us-east"
    error_message = "region must be us-east, the only region the 48-hour watchdog covers (EXPECTED_REGION in infra/akamai/watchdog/deployed/index.py). Change both together."
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
    condition     = can(regex("^[a-z0-9]([a-z0-9-]{0,14}[a-z0-9])?$", var.deployment_id)) && !strcontains(var.deployment_id, "--")
    error_message = "deployment_id must be 1-16 lowercase letters, numbers or single dashes, starting and ending with a letter or number."
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

variable "main_extra_admin_ipv4_cidrs" {
  type        = list(string)
  description = "Extra management IPv4 /32s allowed to SSH to the MAIN VM only, such as the AWS Windows VM's public IPv4 for an SSH tunnel to the UI. Workers stay on ssh_admin_ipv4_cidr alone."
  default     = []

  validation {
    condition = length(var.main_extra_admin_ipv4_cidrs) <= 4 && alltrue([
      for c in var.main_extra_admin_ipv4_cidrs : can(cidrnetmask(c)) && can(regex("^([0-9]{1,3}\\.){3}[0-9]{1,3}/32$", c))
    ])
    error_message = "Each entry must be one valid IPv4 address in /32 form, at most 4 entries. Never 0.0.0.0/0 or a wider range."
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
