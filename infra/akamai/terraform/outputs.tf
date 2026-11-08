output "main_public_ipv4" {
  description = "Main VM public IPv4, null if not deployed. This is NOT your PIA exit IP."
  value       = try(linode_instance.main[0].ip_address, null)
}

output "worker_public_ipv4s" {
  description = "One independent Akamai-assigned public IPv4 per worker."
  value       = { for vm in linode_instance.workers : vm.label => vm.ip_address }
}

output "estimated_compute_usd_per_hour" {
  description = "Only VM compute, not traffic overage, billing rounding, or VPN fees."
  value       = local.estimated_hourly_usd
}

output "estimated_compute_usd_for_48_hours" {
  description = "48 hours for enabled main and enabled workers."
  value       = local.estimated_hourly_usd * 48
}

output "ssh_admin_example" {
  description = "SSH command skeleton for the main server."
  value       = length(linode_instance.main) > 0 ? "ssh -i <PRIVATE_KEY_PATH> lodestar@${linode_instance.main[0].ip_address}" : null
}
