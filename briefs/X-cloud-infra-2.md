# X-cloud-infra-2 (Opus, /security-review): cloud infra follow-ups after #625

Same session as X-cloud-infra (session_01MMjhnCot56kB5kgihYEBsx), the owner's 5th slot. Branch `slice/X-cloud-infra-2` from main (6e6a1135 or later). PR title `X-cloud-infra-2: <title>`. Follow `.claude/skills/steward/SKILL.md`.

**Owns:**
- `infra/**`;
- `.agents/skills/lodestar-fleet/**`;
- `docs/operations/lodestar-cloud-assets.md`;
- `docs/decisions/2026-11-08-X-cloud-infra-2.md`;
- the `infra` job in `.github/workflows/ci.yml` only;
- `.github/scripts/test_ci_changes.py`, for formatting only.

**Rules (unchanged from #625):**
- No live AWS or Akamai IDs, ARNs with account IDs, IPs or tokens.
- Nothing in CI calls a cloud.
- Agents only observe.
- Typed phrases stay, with no bypass.
- Never the owner's soak host.
- New times and log names are in UTC (§2.35).

## Scope
1. **(Original) Main's firewall: an admin-address list.** A list variable for Main's Linode firewall, so the Windows VM can SSH-tunnel to Main's UI. Keep every entry a validated `/32`, inbound DROP otherwise, and no IPv6 ingress. OP-2's firewall audit reads this list (docs/plan/2026-11-08-cloud-operator.md).
2. **(Original) Windows VM root-volume size.** A parameter in `Deploy-AwsWindows.ps1`, which hard-codes 50 GiB. Validate its range.
3. **Region (both #625 reviews):**
   - `variables.tf` validates `var.region == "us-east"`, in step with the watchdog's `EXPECTED_REGION`;
   - the watchdog logs an error, and the alarm counts it, for any Linode tagged `lodestar` that fails its scope checks, instead of skipping it silently;
   - fix `terraform.tfvars.example` and the Preflight text to match.
4. **Provider lock:**
   - run `terraform providers lock -platform=linux_amd64 -platform=windows_amd64` (or add the reviewer-checked linux `h1:vafxT5fztVkwa4VWPQfSApEEJneb91K4qgndOdNtNAM=`);
   - restore `-lockfile=readonly` in the `infra` CI job;
   - optionally pin `hashicorp/setup-terraform` to a SHA.
5. **AWS guard: an expiry that can't reset.** `infra/aws/expiration-guard/deployed/index.py:50` uses `LaunchTime`, and a stop/start resets it.
   - Use the root volume's `Ebs.AttachTime`, or the earliest of that and `LaunchTime`, still capped by `ExpiresAt`.
   - Test a stop/start.
   - This tightens a deployed safety service, so the runner approves it. The owner deploys the change set; write the steps in the assets doc.
6. **Alarms that see silence.** A heartbeat alarm for each of the watchdog and the guard (Invocations, missing data = breaching). Also an alert (not only a warning) for a `Project=Lodestar` instance outside the guard's scope. Template changes are deployed by the owner, as in item 5.
7. **Typed-phrase hardening.**
   - Refuse when `[Console]::IsInputRedirected` is true or the session isn't interactive, so a piped phrase is refused. `-DryRun` keeps working.
   - Validate `Lodestar-Fleet.ps1 access -Target` as an IPv4 address (the quoting issue in PowerShell 5.1).
   - Don't hard-code `powershell.exe` in `plan`.
8. **Correctness nits:**
   - `outputs.tf`: replace the deprecated `ip_address`;
   - the `deployment_id` regex: no trailing `-` and no `--`;
   - the watchdog's `confirm_deleted`: wait on the condition with a deadline of at least 60 s, not about 15 s;
   - a test for Linode's real zone-less `created` format;
   - `test_token_value_parsing` clears `SECRET_ARN`;
   - log file names in the `.ps1` scripts use UTC (`(Get-Date).ToUniversalTime()` with a `Z`);
   - the two blank lines in `test_ci_changes.py` before `class UpdateManifest`;
   - pin `qmcgaw/gluetun` in `pia-example/compose.vpn.yaml` by digest, or say plainly that it's an unpinned example.
9. **Docs:**
   - remove the stale "fmt/validate unverified" from the X-cloud-infra decisions text and `infra/akamai/terraform/README.md:154`, since CI now runs fmt, validate and a PowerShell parse check; only runtime behaviour is unverified;
   - correct "two changes" to three (`DependsOn`) in the watchdog README and the assets doc;
   - the assets doc says the AWS guard template differs from the live one (the IAM condition and `DependsOn`);
   - add a "Not verified" bullet: the live IAM roles (LodestarObserver's denies, LodestarProvisioner's MFA trust, `LodestarRestrictedEC2`) are live-only, as the owner reported, and aren't defined in the repo.

**Leave as is (runner call):** more than 10 eligible Linodes stops all deletion. That fail-closed stop is deliberate; just document it in the assets doc.

**Size:** about 600 lines expected. If it goes past ~1,500, split items 5 and 6 into their own PR.

**Tests and checks:** the watchdog and guard suites, `terraform fmt -check` / `validate`, and the CI `infra` job. No fixed sleeps.
