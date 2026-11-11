# OP-3c (Opus, docs): the cloud fleet runbook, with the "Stop everything" page and the gluetun-stop drill

Assigned by the runner on 2026-11-11, after OP-3b (#661, 9e80e138) merged. This is the last OP-3 slice.

## Read first
- The `OP-3c` entry in `runner/slices.yaml`. Its notes are binding.
- `docs/plan/2026-11-08-cloud-operator.md`: the whole plan, especially "The kill switch the owner can pull, fastest first" (around lines 266–280), the distinct-exits rule for `PIA_SERVERS` (around line 251) and the slice table's OP-3 row (around line 388).
- The decisions files for OP-1, OP-2a, OP-2, OP-2b, OP-3a and OP-3b, and `docs/decisions/2026-11-08-P-cloud-test-plan.md`.
- The merged code the runbook describes: `infra/fleet/**`, `infra/pia-bridge/**`, `.github/workflows/fleet.yml`. Describe what the code does, never what the plan hoped for.
- OP-4 (#655, the operator's wake, tripwire and apply workflows) is still open. Name its workflows as the plan does: `operator-wake.yml`, `operator-apply.yml`, `operator-tripwire.yml`. Mark any operator-side step you can't confirm on `main` "after OP-4 merges"; don't guess at its behaviour.
- The project skill `.claude/skills/user-docs/SKILL.md` for voice: plain, matter-of-fact, define terms, recommend the safe choice. The reader is the owner running the fleet, not a developer.

## Owns
- `docs/operations/cloud-fleet-runbook.md` (new), and `docs/decisions/2026-11-11-OP-3c.md`.
- A shared edit, one link line only: the index of `docs/operations/` or `docs/operations.md`, if one lists these pages.
- Nothing else. If the code contradicts the plan, record it in the decisions file and tell the runner; don't fix the code here.

## Contents
1. **What the fleet is and who runs it**, on one screen: the control plane, the operator, the owner's role.
2. **Booking a fleet session:** the prerequisites. Re-confirm `PIA_SERVERS` against the Unraid host's current exit before every session, because that exit can change; the two exits must differ from each other and from Unraid's.
3. **A screen, step by step:** up, leak test, block start, arm switches, status, copy-off, down. For each step say what you see when it works, and what to do when it fails.
4. **"Stop everything":** one page, the kill switch in the plan's order, fastest first. Include where each control lives in the GitHub, AWS, Linode and Anthropic consoles, and the residual of each step.
5. **The warning from the slices.yaml notes:** the manual Deploy and Destroy scripts share the fleet's state but not its lock, so never run them while a fleet exists.
6. **The gluetun-stop drill:**
   - stop gluetun on a test VM mid-block;
   - confirm nothing leaves outside the VPN: Lodestar stops or is cut off;
   - confirm the outage watcher records the stop, and copy-off still works.
   - Give the exact commands from the committed scripts.
7. **Renewing the VPN mid-block** (`run renew-vpn <vm>`, outside a block), host-key fingerprints from the state bucket, and where the evidence lands.
8. **What never to do:**
   - paste credentials into chat or issues;
   - run a coding agent in the Windows session that holds admin credentials;
   - stitch soak runs;
   - widen IAM without the owner.

## Rules
- No real account IDs, IPs, hostnames, exit names or secrets. Placeholders only, with 111122223333 as the only account ID.
- Times in UTC.
- Diagrams are welcome as Mermaid.
- Name each control exactly as it appears in the console, and add screenshots only if you have real ones. Don't make them up.

## Process
- Follow `.claude/skills/steward/SKILL.md`. Branch `slice/OP-3c` from the latest `origin/main`; PR title `OP-3c: <title>`.
- Self-review against the code with a fresh Opus reviewer before you say ready. Every command in the runbook must exist in the committed scripts or workflows.
- When ready, message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg): `OP-3c: PR #<n> ready for review, head <sha>`, with CI green. The runner reviews and merges.
