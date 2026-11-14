# X-pia-vm-helper: PIA bridge from #632, part b (on-VM helper, compose, cloud-init) on main's sudo model

Runner brief, 2026-11-14. Opus, core, /security-review.

## Decided (owner-decisions §2.71; runner, on the owner's delegation, docs/decisions/2026-11-14-runner-trackq-order.md)
- #632 is the owner's branch. Read it (`git fetch origin pull/632/head:refs/remotes/pr/632`), never push to it.
- Build from main on `slice/X-pia-vm-helper`. Take #632's on-VM parts only:
  - `infra/akamai/terraform/cloud-init/ubuntu24.yaml.tftpl`;
  - `infra/akamai/terraform/pia-example/**`;
  - the `main.tf` lines those need;
  - `infra/akamai/terraform/tests/test_linux_privilege_boundary.sh`.
- **Keep main's sudo model.** The fleet steps and deliver-pia keep running as root through non-interactive sudo. Add #632's root-owned helper, with its fixed verb list, **alongside** main's grant; don't replace it. Never weaken or change what main's fleet steps can do.
- Add a test that ties the template's sudoers lines to every privileged command that main's fleet steps (`infra/fleet/steps/**`) and deliver-pia (`infra/pia-bridge/**`) actually invoke, so the two can't drift apart. The test reads those files and doesn't edit them; they're outside your owns.
- The later narrowing to fixed-verb helpers only is X-fleet-sudo-narrow, not this slice.

## Owns
- infra/akamai/terraform/cloud-init/**
- infra/akamai/terraform/pia-example/**
- infra/akamai/terraform/main.tf
- infra/akamai/terraform/tests/test_linux_privilege_boundary.sh
- docs/decisions/*-X-pia-vm-helper.md

Everything else in #632 is out of scope: scripts/**, the bridge .py and its tests, the PowerShell, infra/aws/**, infra/fleet/**. Those belong to X-pia-bridge-manual and X-pia-bridge-docs, or are dropped for main's (part a).

## Rules
- No `terraform init`, plan or apply. No cloud calls of any kind, and no AWS or Linode tools. Never read or print AWS_* variables.
- The config check must run on stock Ubuntu 24.04. Validate the template offline (render it with test values, then cloud-init schema validation if available, plus shellcheck and your test script).
- A pinned certificate file (`ca.rsa.4096.crt`): check it against the provider's published one, say in the decisions file where it came from, and give its SHA-256.
- No real IPs, account ids, tokens or endpoint addresses. Use documentation ranges in tests.
- The decisions file records: what was taken from #632 and what was left out, the sudo model kept, the tie test, and the follow-up X-fleet-sudo-narrow.
- Steward skill: self-review, open the PR titled `X-pia-vm-helper: <title>`, watch it, fix CI, then send the runner "ready <pr> <sha>". The runner merges.
- No fixed sleeps. No model identifiers. UTC only.
