# OP-2 (Opus, security-sensitive): fleet live mode for `screen`

Assigned by the runner, session_01EYhgh4JX3AmHzKXPjYCoUg, on 2026-11-09 at about 01:05Z. It continues in OP-1's and OP-2a's session (OP-2a merged as #641, ca4cd0a9). Follow `.claude/skills/steward/SKILL.md`. Branch `slice/OP-2` from main ca4cd0a9 or later. PR title `OP-2: <title>`. The runner gives it a fresh Opus review and a /security-review.

## Read first
- `docs/plan/2026-11-08-cloud-operator.md`: §3 and §4 (credentials, revocation, containment for the loosenings the owner confirmed in `docs/owner-decisions.md` §2.53), and the Slices row **OP-2**. Binding, except where this brief records a runner refinement.
- `docs/decisions/2026-11-08-OP-1.md` and `docs/decisions/2026-11-08-OP-2a.md` (lock and state keys, the tombstone, the bridge's GetObject-only scope, "OP-2 needs more keys").
- The `OP-2` entry in `runner/slices.yaml` (its notes are binding: they carry the #636 and #641 review follow-ups).
- `infra/fleet/**`, `infra/aws/fleet-control/**`, `infra/akamai/terraform/**` as merged.

## Owns
- `infra/akamai/terraform/main.tf`, `infra/akamai/terraform/variables.tf`, `infra/akamai/terraform/cloud-init/**`;
- `infra/fleet/apply.py`, and new tests under `infra/fleet/tests/` for it;
- `.github/workflows/fleet.yml` (OP-1 has merged, so it's yours now; OP-2b follows you serially);
- `docs/decisions/2026-11-09-OP-2.md`;
- **shared, small edits:**
  - `infra/fleet/{fleet.py,reaper.py,state.py,limits.py,lock.py,status.py}` and their tests, for the live-mode hooks the plan row names (audit, pre-emption marker, tombstone, `max_fleets`, the `monthly_compute_cap_usd` bound);
  - `infra/aws/fleet-control/build_templates.py`, the generated `fleet-control.json` and its tests, **only** to add the exact keys for the monthly spend ledger and the mismatch flag (OP-2a's decisions file says so). Exact keys, never a wider prefix; the policy tests must still pass and should assert the new keys.

## Runner refinements over the plan row (binding)
1. **`fleet.json` is tombstoned, never deleted.** Where the plan row says `down` "deletes `fleet.json`", write the tombstone instead. You decide and record who writes the first tombstone (setup or the first `down`).
2. **"No fleet" is read only from a readable tombstone.** Any read error, a 403 included, means a fleet may exist.
3. **Never `force-unlock`.** The emergency `down` retries within the run, then deletes Lodestar-tagged Linodes and firewalls through the Linode API, filtered to the fleet account plus tag.
4. **Live mode stays off until `FLEET_LIVE=true`.** Everything is testable with fakes in dry-run.
5. **Pin `actions/checkout` (and every action) by full SHA** in any job that gets `id-token: write` or a secret.
6. The other follow-ups in the OP-2 notes: `down` with no `fleet.json`, mismatch with no fleet recorded, `max_fleets` used or removed, `monthly_compute_cap_usd` bounded in `limits.load`, and the two test-polish items (the `ps_code` scan's missed `terraform init` forms, the policy `Version` value check).

## Tests
- Unit tests with fakes for the Linode API, the Actions API, `/meta` and S3; no network.
- The firewall audit: rule shape (ACCEPT, TCP/22, one IPv4 `/32` inside GitHub's Actions ranges, at most one per live run), admin rules from X-cloud-infra-2's list, sweep before audit, mismatch raises the reaping marker.
- Pre-emption: the marker on mismatch or >15 min past expiry; `down`-only afterwards; the marker clears after `down` empties the inventory, and has its own expiry.
- The public-log leak test from OP-1 extended to any new workflow output.
- Terraform `fmt` and `validate` (`-backend=false`) still pass.

## Rules
- **No cloud call anywhere, in CI or locally, even with dummy credentials. Never run `terraform init` against an S3 backend.** Reviewers follow the same rule.
- No live account IDs, ARNs with account IDs, IPs or tokens in the repo.
- Never the owner's soak host.
- No model identifiers. No fixed sleeps in tests.
- Never loosen §4's containment or widen any credential's scope beyond the exact keys above. If the plan seems to need that, stop and message the runner.
- Size: the plan says ~450 lines; keep the PR under ~1,500 changed lines, or tell the runner before going over.
