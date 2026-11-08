# tolist(ipv4)[0] is the public address only because private_ip = false in
# main.tf: ipv4 is a set, sorted lexically, so a private 192.168.x address could
# come first if private IPs were ever enabled.
output "main_public_ipv4" {
  description = "Main VM public IPv4, null if not deployed. This is NOT your PIA exit IP."
  value       = try(tolist(linode_instance.main[0].ipv4)[0], null)
}

output "worker_public_ipv4s" {
  description = "One independent Akamai-assigned public IPv4 per worker."
  value       = { for vm in linode_instance.workers : vm.label => tolist(vm.ipv4)[0] }
}

output "main_admin_ipv4_cidrs" {
  description = "The /32s allowed to SSH to the main VM (primary admin plus extras)."
  value       = local.main_admin_ipv4_cidrs
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
  value       = length(linode_instance.main) > 0 ? "ssh -i <PRIVATE_KEY_PATH> lodestar@${tolist(linode_instance.main[0].ipv4)[0]}" : null
}
