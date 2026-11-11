# OP-3b (Opus, security-sensitive, /security-review): fleet block steps over SSH

Assigned by the runner on 2026-11-11 (owner resumed the runner, up to 8 slots). OP-3 was split into OP-3a (merged as #652, 5da6eb15), OP-3b (this) and OP-3c (the runbook, after this).

## Read first
- `runner-state:briefs/OP-3.md` (the original OP-3 brief: invariants, tests, build items 1, 3, 4 apply here).
- `docs/decisions/2026-11-09-OP-3a.md` (what 3a built and left to 3b: the step runner, `vm-status`, `leak-test`, compose.yaml, the "step not built yet (OP-3b)" refusals, the probe/marker grammar, `renew-vpn`'s mid-block rule).
- `docs/plan/2026-11-08-cloud-operator.md` (the `run <step> <vm>` allow-list ~165, kill switches ~270-280, OP-3 row ~388) and `docs/plan/2026-11-08-cloud-test-plan.md` (Ground rules, Stop conditions, Phase 0, the lanes, the cloud screen).
- The runner's reviews of #652 on runner-state: `reviews/rv-652-*.md`.

## Owns
- `infra/fleet/steps/**`; `docs/decisions/2026-11-11-OP-3b.md`.
- Shared, small edits (approved): `infra/fleet/*.py` and tests, `.github/workflows/fleet.yml`, only to dispatch the new steps through the existing `run` path; `infra/pia-bridge/**` only where `renew-vpn`'s mid-block check must read the block marker below.

## Build
1. **start-block** at a rules commit: requires `leak-test` exit 0 **and** its final `ok leak_test` line in the same run; refuses otherwise (a stop condition). Starts the collector detached (`lodestar-collector-*`), and returns at once.
2. **A durable block marker** written by start-block before the collector starts and removed only by the block's own end or copy-off. Every mid-block check honours it (arm switch, `renew-vpn`, outage watcher), not only `docker ps`, so a stopped-but-not-finished block still counts as mid-block (closes the arm-switch gap from #652's review). Fail closed when the marker can't be read.
3. **arm switch**, **outage watcher** (detached, bounded, records outages without stopping the crawl unless the plan's stop condition says so), and **copy-off with the allow-list scrubber** (OP-3 brief item 3: raw soak logs, DBs, executables and credentials never leave the VM; only scrubbed allow-listed evidence goes to the private evidence bucket with `ExpectedBucketOwner`).
4. **The screen-rules file format** (the Lodestar image digest per screen, and the `--spec-commit` it gives): 3a pinned gluetun by digest in compose.yaml; this slice adds the rules file the block reads at the rules commit.
5. **The host's own IPv6 check** (0d's separate host check) that 3a left to 3b.

## Invariants
Never the owner's soak host; FLEET_LIVE stays off; no cloud, SSH, PIA or AWS call anywhere in tests or CI, even with dummy credentials; no `terraform init` against S3; nothing private in any output; fake account IDs only `111122223333`; extend the fake-value public-log test to every new step.

## Not here
- The `s3:ResourceAccount` condition for Terraform's S3 backend (owner follow-up; a policy change).
- The runbook (OP-3c).

## Process
- Follow `.claude/skills/steward/SKILL.md` from first push to merge-ready. Branch `slice/OP-3b` from the latest `origin/main`; PR title `OP-3b: <title>`.
- Before review: mutation-check every guard you add (the last reviews were mostly about guards with no test). No fixed sleeps in tests; condition waits with deadlines of at least 10 s.
- Push once per round of fixes. Keep the PR under ~1,500 changed lines (generated files aside); if it grows past that, tell the runner before splitting.
- Anything beyond this brief (a new crate, a `Cargo.toml` change, a loosening of security or content safety, an owner-set number) goes to the runner first.
- When ready: message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `OP-3b: PR #<n> ready for review, head <sha>` with CI green. Blocked: `OP-3b: blocked on <what>`. The runner reviews and merges; you never merge.
