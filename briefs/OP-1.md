# OP-1 (Opus, security-sensitive): the fleet control plane in dry-run

Launched by the runner, session_01EYhgh4JX3AmHzKXPjYCoUg, on 2026-11-08 at about 20:00Z. Follow `.claude/skills/steward/SKILL.md`. Branch `slice/OP-1` from main (06ef6385 or later). PR title `OP-1: <title>`. The runner gives every change to `fleet.yml` or `infra/fleet/**` a fresh Opus review and a /security-review (plan §3).

## Read first
- `docs/plan/2026-11-08-cloud-operator.md`: §2 (channel and workflow hygiene), §3 (limits, lock, reaper, audit, public-log rule), §4 (credentials and revocation), §5 (first-run test), "Loosenings", and the Slices row **OP-1**. Its build requirements are binding.
- `docs/decisions/2026-11-08-P-cloud-operator.md`.
- `docs/plan/2026-11-08-cloud-test-plan.md`: the cloud screen's rules 1–6, plus Prerequisite 1 (state reconciliation).
- `infra/` as merged by #625, and `docs/operations/lodestar-cloud-assets.md`.
- The `OP-1` entry in `runner/slices.yaml`.

## Owns
- `.github/workflows/fleet.yml` (OP-1 creates it; OP-2 and OP-2b edit it after you, one at a time);
- `infra/fleet/**`, except `infra/fleet/steps/**` (OP-3) and `infra/fleet/apply.py` (OP-2);
- `docs/decisions/2026-11-08-OP-1.md`;
- **shared, one small edit:** the `infra` job in `.github/workflows/ci.yml` and the `infra` filter in `.github/scripts/ci_changes.py` (with its test), only so that OP-1's tests run in CI. X-cloud-infra-2 also edits that job; whichever of you merges second brings main in.

## What OP-1 is
**Dry-run only.** No cloud call anywhere. Live mode is OP-2's, and it stays off until `FLEET_LIVE=true`. Build:
1. **A strict comment parser** for `/fleet` commands. It's committed Python, and comment text reaches it only through an environment variable, never a shell. The allow-list is `status`, `up screen`, `down`, and `run <step> <vm>` with `<step>` from a committed list and `<vm>` one of `main` or `worker`. Everything else is refused and logged. Give it unit tests and property tests (hypothesis, or an equivalent the repo already uses).
2. **Filter-then-act jobs.**
   - A first job with no secrets and no environment checks: the channel PR (`CHANNEL_PR`), the author's **numeric** ID equals `OPERATOR_BOT_USER_ID`, `created` events only, and `OPERATOR_STOP` isn't `true`.
   - `down` and the reaper are exempt from `OPERATOR_STOP`.
   - Job-level `if:` gates come before `environment:`, and every job sets minimal `permissions:`.
3. **The reader helper** used by the operator. It returns only unedited comments (REST `updated_at` == `created_at`) whose author IDs are the runner's, the App bot's or the control plane's.
4. **The ancestor-of-main check** on every commit argument (`git merge-base --is-ancestor`).
5. **No `workflow_call`, and no reusable workflows,** in `fleet.yml`. Add a test that fails if one appears; it covers the operator workflows too once they exist.
6. **`infra/fleet/limits.yaml` and the `screen` preset:** Main plus Worker, the monthly fleet-compute cap of $40 from list prices, one fleet, `up` refused while any Lodestar Linode exists. Inventory is dry-run or stubbed here.
7. **The S3 fleet lock.**
   - A conditional write (`If-None-Match`) holding the run ID and an expiry; the chain expiry is 90 minutes.
   - Release is a conditional delete that succeeds only while the lock holds this run's ID.
   - A taken lock gives a "busy, retry" result.
   - The lock is taken only after the author, edit and PR checks.
   - Implement it against an interface, with an in-memory fake for tests. The live S3 calls are fine behind dry-run, as long as nothing executes them in CI.
8. **The status comment,** edited in place: role names, ages, expiry and cost so far. No labels, fingerprints, IPs or IDs (#627 rule 6).
9. **`fleet.json`:** the pinned commit, the expiry (at most 44 h), and slots for addresses, fingerprints and firewall IDs. It lives beside the state and is never read back from a comment.
10. **The hourly reaper:**
    - by expiry, and by creation age over 44 h, both in dry-run;
    - the "reaping" marker that pre-empts the lock on a mismatch or more than 15 minutes past expiry;
    - `down` only, once a fleet is overdue or mismatched.

    The audit's rule-shape check itself is OP-2's; leave a clear hook.
11. **The public-log leak test.** Run the workflows' scripts with marked fake values, and fail if any value reaches stdout. It also covers `operator-wake.yml` and `operator-injection.yml` when those land; OP-4 and OP-5 extend it.

## Build requirements (plan row OP-1, binding)
- Every lock-holding job (`up`, `down`, `run <step>`, delivery) sets `timeout-minutes` below 90.
- Every rule-set write first checks that the lock still holds this run's ID (fencing).
- **The owner's `workflow_dispatch` `down`:**
  - `github.actor_id` is checked against a variable in the job-level `if:`, before `environment:`;
  - there's no free-form input;
  - it is never routed through the `/fleet` parser;
  - it pre-empts the lock with the "reaping" marker;
  - a negative test shows a dispatch can do nothing but `down`.
- Account IDs, ARNs and resource IDs are environment **secrets**, never variables or literals. Nothing private is printed, and there are no Actions artifacts.

## Rules
- Never the owner's soak host.
- No live IDs, ARNs with account IDs, IPs or tokens in the repo.
- No model identifiers.
- No fixed sleeps in tests.
- Keep it within about 700 lines. If it grows past ~1,500, split the reaper into OP-1b and tell the runner first.
- Times are in UTC.

## Done
- Tests pass locally and in CI.
- Self-review against the plan row.
- Message the runner: `OP-1: PR #<n> ready for review, head <sha>`.
