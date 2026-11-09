# OP-3 (Opus, security-sensitive): fleet step scripts over SSH, and the cloud-fleet runbook

Assigned by the runner, session_01EYhgh4JX3AmHzKXPjYCoUg, on 2026-11-09. It continues in the session that built OP-1, OP-2a, OP-2 and OP-2b (OP-2b merged as #650, 5036863b). Follow `.claude/skills/steward/SKILL.md`. Branch `slice/OP-3` from main 5036863b or later. PR title `OP-3: <title>`. The runner gives it a fresh Opus review and a /security-review.

## Read first
- `docs/plan/2026-11-08-cloud-operator.md`: the `run <step> <vm>` allow-list (~165), "Nothing private reaches a public log", the kill-switch list (~270-280, which the runbook turns into its "Stop everything" page), and the Slices row **OP-3** (~388). Binding, except where this brief records a runner refinement.
- `docs/plan/2026-11-08-cloud-test-plan.md`: Ground rules, Stop conditions, Phase 0 (the tunnel and the leak test: IPv6 and DNS included, the capture on gluetun's side), the lanes, the cloud screen and the Deliverables list.
- `docs/decisions/2026-11-08-OP-1.md`, `2026-11-08-OP-2a.md`, `2026-11-09-OP-2.md`, `2026-11-09-OP-2b.md`: the lock, fence, tombstone, `run` dispatch, job rules, the per-VM flock `/run/lodestar-pia.lock`, the deadline pattern, and `renew-vpn` as merged.
- `docs/owner-decisions.md` §2.53.
- The `OP-3` entry in `runner/slices.yaml`; its notes are binding.

## Owns
- `infra/fleet/steps/**`;
- `docs/operations/cloud-fleet-runbook.md`;
- `docs/decisions/2026-11-09-OP-3.md`;
- **Shared, small edits (approved):** `infra/fleet/*.py` and their tests, and `.github/workflows/fleet.yml`, only to dispatch the step scripts through the existing `run <step> <vm>` path and for P8 below. `infra/pia-bridge/**` only for the renew-vpn/gluetun item below.

## Build (the plan row and the notes)
1. **Step scripts** for `run <step> <vm>`: leak test, start block at a rules commit, arm switch, outage watcher, status, copy-off. Long-running processes (the block, the outage watcher) start detached on the VM and the step returns at once. Every step is bounded (a step timeout, and a session clamp like deliver-pia's), idempotent, and refuses on any unexpected input (the rules commit, the arm name, the VM role). Targets and host keys come only from `fleet.json`.
2. **The leak test** proves zero egress outside PIA with the tunnel down, IPv6 and DNS included, with the capture on gluetun's side. It also checks `/run/lodestar-pia` is gone (taking `/run/lodestar-pia.lock` with `flock -n`, as OP-2b's clean check does). A failure is a stop condition: no crawl starts on that VM.
3. **Copy-off** goes only through an allow-list scrubber, tested with fake values: raw soak logs, databases, executables and credentials never leave the VM; only the scrubbed allow-listed evidence goes to the private evidence bucket (with `ExpectedBucketOwner`), never to a log, comment or Actions artifact.
4. **Status** prints only fixed strings and counters, nothing private (no addresses, fingerprints, IDs, bucket names).
5. **The runbook** (`docs/operations/cloud-fleet-runbook.md`): the one-page "Stop everything" list first (the plan's five steps), then each command and step, what it does, what it costs and how to tell it worked; the warning (runner, 2026-11-08) that the manual Deploy/Destroy scripts share the fleet's state but not its lock, so never run them while a fleet exists. Developer/operations doc: plain and matter-of-fact (the `user-docs` skill applies to `docs/operations.md`, use its voice).

## Runner items carried from OP-2b (#650)
- **renew-vpn doesn't restart gluetun.** Decide where the restart belongs (renew-vpn, or the arm switch/block start re-reading the config) and make a renewed tunnel actually take effect, with a test. Record the choice.
- **P8 (LOW, tightening):** OP-2's cloud-fleet S3 calls (state, lock, fleet.json, flag) lack `ExpectedBucketOwner`. Add it, from a new environment secret holding the control bucket's owner (never committed; fake fixture `111122223333`; extend the leak check). Name the secret in the PR body for the owner's setup list. This is a tightening, so it's the runner's call; flag it.

## Invariants
- Never the owner's soak host; steps reach only the fleet's own Linodes by pinned host keys.
- FLEET_LIVE stays off; every action pinned by full SHA; no force-unlock; no live account IDs or ARNs with account IDs.
- No cloud, SSH or PIA call anywhere in tests or CI; no `terraform init` against S3.
- Nothing private in stdout, stderr, comments or the workflow log; extend the fake-value public-log test to every new step and path.
- No fixed sleeps in tests (fake clocks, or condition waits with deadlines of at least 10 s).

## Tests
- Unit tests with fakes for SSH, S3 and the Linode API; shell steps tested locally with stubs (as OP-2b's `pia-wireguard.sh` tests), including hostile-input cases for every value a step reads.
- The scrubber: allow-list positive and negative cases, with fake secrets that must not survive.
- Mutation-check the guards yourself before asking for review (the last three PRs' reviews were mostly about guards with no test).

## Size
About 700 lines in the plan, plus the runbook and P8. Keep it under ~1,500; if the runbook pushes it over, split the runbook into OP-3b.
