# X-test-infra-blockers: the PIA key script and the AWS Windows launch

**Runner brief, 2026-11-12 (UTC).** Opus, infra. Small, about 200 lines with tests.
- **Branch:** `slice/X-test-infra-blockers` from fresh `origin/main`.
- **PR title:** `X-test-infra-blockers: <title>`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.
- **Why now:** the owner said blockers to testing go before new features (relayed by the cloud tester, 2026-11-12 about 16:55Z: "The goal is to get past the testing phase as quickly as possible without sacrificing testing effectiveness"). These are blockers 2 and 3. Blocker 1, the gluetun kill-switch leak, waits for the tester's diagnosis and gets its own slice.

## Hard rules
- **No cloud calls of any kind.** No AWS CLI or SDK calls, and no AWS MCP or connector tools, even if they're loaded. No `terraform init`, plan or apply. No calls to PIA either.
- These are scripts the owner and the tester run. You change them and test them offline only.
- **Never write account ids, subnet, security-group or instance ids, IPs, or credentials** into the repo or into messages. The only 12-digit numbers allowed are 111122223333 and 999988887777.
- **If a fix needs an IAM policy change, a security-group rule change, or new spending** (a NAT gateway, VPC endpoints), stop and tell the runner first. Those are the owner's.

## 1. `infra/pia-bridge/vm/pia-wireguard.sh`
- **Stage 13:** the `cn` check `^[a-z0-9.-]{1,253}$` rejects some PIA servers' certificate names, so us-brooklyn fails every time.
  - Find which characters the real names use, from PIA's documented server list format or from the script's own comments, without fetching the list.
  - Widen the check only to a strict hostname shape: labels of `[A-Za-z0-9-]`, 1–63 characters each, no leading or trailing hyphen, at most 253 characters in total. A name lowercased first is fine.
  - It must still reject anything that could inject into `--connect-to` or the URL: `:`, `/`, `@`, whitespace and control characters. That `cn` is untrusted input.
- **Stage 15:** the addKey HTTP status is discarded.
  - Capture the HTTP status with `--write-out '%{http_code}'`, and PIA's `.status` and `.message` when the reply is JSON. Print them to stderr, bounded in length, with control characters stripped.
  - Never print the token, the private key or the whole reply.
  - Keep the exit code 15, so callers don't change.
- **Tests:** an offline test script in the existing style next to it (or a bats-free bash test). Cover the cn check with good names, PIA-style names including the one that failed, and injection attempts. Cover the stage-15 message using a stubbed `curl` on `PATH`.

## 2. `infra/akamai/terraform/scripts/Deploy-AwsWindows.ps1`
- **The problem:** the instance launches without a public IPv4 into a subnet that has an internet gateway but MapPublicIpOnLaunch=false, and no NAT or VPC endpoints. So it never reaches SSM.
- **The fix the runner prefers:** request a public IPv4 at launch. Switch `--subnet-id` and `--security-group-ids` to one `--network-interfaces` entry with `DeviceIndex=0`, the same subnet and group, and `AssociatePublicIpAddress=true`.
  - SSM is outbound only, so the instance needs no inbound access.
  - Before launch, the script checks that the security group has **no inbound rules** (one `describe-security-groups` call, read-only, made when the human runs the script). It refuses to launch, with a clear message, if any exist.
  - It prints that a public IPv4 is attached, and that the instance is reachable only outbound.
- Keep every existing guard: the typed phrase, the expiry policy, state tracking, dry run, and the refusal while any instance is active.
- **IAM:** `LodestarRestrictedEC2` might restrict public IPs, for example with the `ec2:AssociatePublicIpAddress` condition key. Read the policy files in the repo (`infra/aws/**`). If it does, stop and tell the runner; the IAM change is the owner's.
- **Tests:** the script's existing Pester tests, if any, or a `-DryRun` argument-shape test that needs no AWS. Run it with `pwsh` if it's installed; if not, say so.

## Owns
- `infra/pia-bridge/vm/pia-wireguard.sh`, and a new test next to it
- `infra/akamai/terraform/scripts/Deploy-AwsWindows.ps1`, and its tests
- The matching README or runbook lines for both
- `docs/decisions/2026-11-12-X-test-infra-blockers.md`

## Done
- `shellcheck` on the changed shell scripts;
- the new tests pass;
- `pwsh` syntax parse and Pester, if available.

The cloud tester tests on its VMs straight from your branch before merge. Its session refuses messages from other sessions, so the runner records your branch in runner-state for it.

Send the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green. A fresh Opus `/security-review` follows.
