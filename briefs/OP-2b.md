# OP-2b (Opus, security-sensitive): PIA delivery (`deliver-pia`, `renew-vpn`), the owner's bridge, the VM WireGuard step

Assigned by the runner, session_01EYhgh4JX3AmHzKXPjYCoUg, on 2026-11-09. It continues in OP-1's, OP-2a's and OP-2's session (OP-2 merged as #646, 025fcc02). Follow `.claude/skills/steward/SKILL.md`. Branch `slice/OP-2b` from main 025fcc02 or later. PR title `OP-2b: <title>`. The runner gives it a fresh Opus review and a /security-review.

## Read first
- `docs/plan/2026-11-08-cloud-operator.md`: §3 and §4, the `run <step> <vm>` line (~165), setup step 6, and the Slices row **OP-2b** (~387). These are binding, except where this brief records a runner refinement.
- `docs/owner-decisions.md` §2.53: the confirmed loosenings, which are the firewall token and the per-job /32.
- `docs/decisions/2026-11-08-OP-1.md`, `2026-11-08-OP-2a.md` and `2026-11-09-OP-2.md`: the lock, fence, tombstone, flag, kill paths, job rules and timeouts as merged.
- The `OP-2b` entry in `runner/slices.yaml`. Its notes are binding.
- `infra/fleet/**`, `infra/aws/fleet-control/**`, `infra/akamai/terraform/**` and `.github/workflows/fleet.yml` as merged.

## Owns
- `infra/pia-bridge/**`;
- `.github/workflows/fleet.yml` (OP-2 has merged, so it's yours now);
- `docs/decisions/2026-11-09-OP-2b.md`;
- **Shared, small edits:**
  - `infra/fleet/*.py` and their tests, for the delivery hooks and for the OP-2 follow-ups below;
  - the OP-2a template (`infra/aws/fleet-control/build_templates.py`, the generated JSON and its tests), **only** for exact keys or actions the plan row needs. The PIA role's scope is unchanged (notes). Exact keys only, never a wider prefix, with policy-test assertions.

## Build (the plan row and the notes)
1. **The jobs.**
   - The `deliver-pia` job and `renew-vpn` run in the `pia-delivery` environment, with their own gates.
   - Targets and pins come only from `fleet.json`, and `<vm>` is `main` or `worker`.
   - A `cloud-fleet` job fences on the fleet lock just before `deliver-pia`, and releases it after with a conditional delete.
   - The chain expires at 90 minutes, and each job has its own `timeout-minutes`.
2. **The job's own labelled firewall /32,** opened through the firewall token from Secrets Manager. It is swept like OP-2's job rules, and needs `actions: read` for the stale sweep.
3. **The PIA login.**
   - It is read with boto3, and every S3 call passes `ExpectedBucketOwner`.
   - Each value is masked. It goes over SSH stdin, with the remote output discarded and the host keys pinned.
   - Each worker gets a distinct server from `PIA_SERVERS`.
   - The operator never holds the PIA login.
4. **The VM step.**
   - It makes the WireGuard configuration and deletes the `/run` credentials in a trap on EXIT and HUP.
   - The next job, or the leak test, checks that `/run` is clean.
   - Test it.
5. **Invariants:**
   - FLEET_LIVE stays off;
   - every action is pinned by full SHA;
   - no force-unlock;
   - fleet.json is tombstoned and never deleted;
   - no live account IDs or ARNs containing account IDs;
   - never the owner's soak host;
   - no cloud call anywhere in tests or CI.

## OP-2 follow-ups (from #646's last delta review, rv-646-470f280b.md; do these in this PR)
- **S1 (LOW, availability).** In `lock.py:165` and the `cur is None` retry at `:155`: after a failed conditional put, re-read once and apply the same own-holder check. Otherwise a lost response followed by a 412 on the takeover path still strands our own lock. Test the case where the put commits and then returns False.
- **P1: assertions for the surviving mutations.**
  - M5, M6: a failing release in `act` and in the reaper.
  - M11: an expired own holder is taken over through `put_if_match`.
  - M13: a failed first-tombstone re-read raises FleetUnknown.
  - M18: a firewall-only mismatch whose flag re-read fails still sweeps (`fleet.py:367`).
  - M25: the reaper's "lock busy" reaches the channel.
  - M26: `test_busy_and_reaping` asserts nothing ran after "busy".
- **P2 (optional hardening, LOW).** Add `GITHUB_JOB` to the lock's run ID, or have `test_workflow` pin that the lock-holding jobs' `event_name` conditions stay disjoint. Your new jobs make this matter more, so pick one and record it.
- **A stale line in the OP-2 decisions file.** Line 56 still says the mismatch flag's grant is "Get, Put and Delete, plus its list prefix". Line 65 has the narrowed grant, Get and Put only. Correct line 56.

## Tests
- Unit tests with fakes for S3, Secrets Manager, the Linode API, the Actions API and SSH. No network.
- A leak check that no PIA value, address, host key, fingerprint, EUUID, bucket name or token reaches stdout, stderr, a comment or the workflow log.
- The /run-clean check, the trap on EXIT and HUP, and the chain-expiry and release paths.
- `build_templates.py --check`, and the fleet and fleet-control suites.
- No fixed sleeps.

Size about 550 lines plus the follow-ups. Push once per round, then send the runner "ready" with the head.
