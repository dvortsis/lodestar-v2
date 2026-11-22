# X-cloud-infra-2 decisions (#634, follow-ups to #625)

The runner's follow-ups after #625: the cloud test plan's two needs, and the #625 reviewers' findings. Brief: `briefs/X-cloud-infra-2.md` on `runner-state`. Nothing here is deployed. The template and guard changes reach AWS only when the owner runs a reviewed change set (`docs/operations/lodestar-cloud-assets.md`, "Deploying template changes"). The runner approved the guard change (brief item 5).

This file **supersedes one bullet** of `2026-11-08-X-cloud-infra.md`. Its "Not verified" entry on `terraform fmt -check` and `terraform validate` is out of date: CI's `infra` job now runs `fmt`, `init` and `validate`, plus a PowerShell parse check, on every change to `infra/`. Only the scripts' runtime behaviour, and anything against a live account, remain unverified.

## Terraform
- **Admin addresses are strict dotted IPv4 `/32`s.** Octets are 0–255 with no leading zeros, so `010.0.0.1/32` is refused. This applies to `ssh_admin_ipv4_cidr` and to the new list.
- **Main's firewall takes a list of admin addresses.** `main_extra_admin_ipv4_cidrs` (default empty, at most 4) adds `/32`s to Main's SSH rule only. One use is the AWS Windows VM's public IPv4, so it can SSH-tunnel to Main's UI (the cloud test plan).
  - **Why a separate variable:** `ssh_admin_ipv4_cidr` stays the primary address, so workers, `Preflight.ps1`'s check and existing `terraform.tfvars` files are unchanged.
  - **Validation:** each entry must be a dotted IPv4 `/32`. The rule has no `ipv6` list, so there is no IPv6 ingress, and inbound stays DROP otherwise. The merged list is exposed as the `main_admin_ipv4_cidrs` output for OP-2's firewall audit.
- **Region is fixed to `us-east`.** `variables.tf` and `Preflight.ps1` refuse any other value, in step with the watchdog's `EXPECTED_REGION`. A fleet elsewhere would never be reaped.
- **`deployment_id`:** no leading or trailing `-` and no `--`.
- **Outputs use `ipv4`,** replacing the deprecated `ip_address`.
- **The provider lock has both platforms' `h1:` hashes,** so CI runs `init -lockfile=readonly` again.
  - **How the hashes were checked:** I computed the `linux_amd64` hash from the provider zip on the GitHub release. It matches the reviewer's `h1:vafxT5…`. The zip's SHA-256 matches the published `SHA256SUMS` and the lock's `zh:` entry, and the same computation on `windows_amd64` reproduces the existing `h1:sr1CeDX…`.
  - **Action pin:** `hashicorp/setup-terraform` is pinned to the commit of `v3.1.2`.

## AWS expiration guard
- **A stop/start can't reset the 48-hour clock.** The guard measured from `LaunchTime`, which a stop/start resets. It now measures from the earliest of `LaunchTime` and the root EBS volume's `Ebs.AttachTime`. The root volume stays attached through a stop/start, and `ExpiresAt` still caps the deadline. Tests cover a stop/start, a recent attach, a non-root volume and the `ExpiresAt` cap.
- **Every `Project=Lodestar` instance is scanned.** The server-side filter no longer narrows to the guard's scope.
  - **Reporting:** an instance outside that scope (VPC, security group, type, tags) is logged at error level as `out_of_scope_instance`.
  - **Alarm:** a metric filter counts that line, and `lodestar-expiration-guard-out-of-scope` alarms on it.
  - **Why not alert from the code:** an alarm notifies once per state change, where a per-run alert would repeat every 15 minutes.
- **Heartbeat alarm.** `lodestar-expiration-guard-heartbeat` fires when `Invocations` is below 1 for an hour, with missing data counted as breaching. A disabled schedule is now noticed.

## Akamai watchdog
- **Linodes outside its scope are reported.** A Linode tagged `lodestar` that fails the scope checks (region, type, label, tags) is logged at error level as `out_of_scope_linode`. So is one that passes them but was created before `MIN_CREATED_UTC` or dated more than 10 minutes ahead (`reason: created_outside_window`). Before, both were skipped in silence. A metric filter and `lodestar-akamai-expiration-guard-out-of-scope` alarm on it, plus a heartbeat alarm like the guard's.
- **Deletions are confirmed against one shared deadline.**
  - **Before:** each deletion waited about 15 seconds for its own 404.
  - **Now:** the run sends all its deletions first, then polls every 5 s until each returns 404 or a single 90 s deadline passes.
  - **Bounded by the Lambda's time left:** the deadline is the lower of 90 s and the time left minus 20 s kept for the report. A new deletion isn't started with under 60 s left; it is reported as `DeferredLowTime` and retried on the next run.
  - **Timeout:** 300 s. That covers HTTP-error retries comfortably. A run of network timeouts can still use it up, and then the Lambda timeout fails the run, which the errors alarm reports.
  - **One failed status check no longer discards confirmations.** A failed check leaves that Linode pending until the deadline, instead of throwing away the confirmations already made.
  - **Unconfirmed deletions** fail the run, as before. Confirmed ones are still announced on SNS.
- **Fail-closed limit kept (runner's call):** more than 10 eligible Linodes stops all deletion for that run. It's documented in the assets doc and the watchdog README.
- **New tests:** the real zone-less `created` format, out-of-scope logging, and the confirmation deadline (with a fake clock, no sleeps). `test_token_value_parsing` no longer leaks `SECRET_ARN`.

## PowerShell
- **Root volume size is a parameter.** `Deploy-AwsWindows.ps1 -RootVolumeGiB` takes 50–100 (default 50). The upper bound is the `LodestarRestrictedEC2` policy's 100 GiB cap, as the owner reported it.
- **A piped confirmation phrase is refused.** `Interactive.ps1`'s `Assert-InteractiveConfirmation` throws when stdin is redirected, the session isn't interactive, or the host was started with `-NonInteractive`.
  - **Where it runs:** before every typed phrase, in `Deploy.ps1`, `Destroy.ps1`, `Deploy-AwsWindows.ps1` and `Destroy-AwsWindows.ps1`. `-DryRun` doesn't ask for a phrase, so it isn't affected.
- **`Lodestar-Fleet.ps1 access -Target`** accepts only an EC2 instance ID or a dotted IPv4 address.
- **`plan` runs the dry run with the PowerShell edition that is already running,** built from `$PSHOME` (`pwsh` on Core, `powershell.exe` on Desktop) rather than the process path, which under ISE is `powershell_ise.exe`. It drops `-ExecutionPolicy Bypass` on purpose: the child inherits the process-scope policy, and bypassing it was never needed for a signed or local script.
- **PowerShell ISE is refused for typed phrases:** it has no console, so its input reads as redirected. The error message says to use a console or Windows Terminal.
- **Log and plan file names use UTC,** with a trailing `Z` (§2.35).

## Docs
- **The assets doc lists every way each template differs from its live stack,** with the owner's change-set steps.
- **The live IAM roles are recorded as live-only,** not defined in the repository.
- **The gluetun image is marked as an unpinned example.** I couldn't resolve a digest from this session, so it isn't pinned.
