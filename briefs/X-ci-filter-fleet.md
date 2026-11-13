# X-ci-filter-fleet (Sonnet, CI script)

**Why.** Follow-up to #824 (X-gluetun-pin-ci-filter, merged). Its self-review and the runner's
review found three small gaps in `.github/scripts/ci_changes.py`.

**Do.**
1. Add `.github/workflows/fleet-emergency-down.yml` to `INFRA`. `infra/fleet/tests/test_emergency_cleanup.py`
   reads it, so a change to it must run the `infra` job. Anchor the pattern like the others.
2. Update the summary comment at the top of the file (about lines 20-21) so it lists every
   path outside `infra/` that turns the infra job on: the gluetun rules file, the two docs and
   the workflow above.
3. Tighten the `packaging/companions/` prefix in `RUST_ALSO` to the files the Rust tests read:
   `^packaging/companions/(gluetun-post-rules\.txt$|compose/|unraid/)`. Before you do, grep
   `crates/` for every read under `packaging/companions/` and confirm that list is complete.
   If something else is read, include it, and say so in the PR.
4. One test per change in `.github/scripts/test_ci_changes.py`, with a near miss that stays off
   (for example a README under `packaging/companions/` no longer turns on the Rust jobs).

**Owns:** `.github/scripts/ci_changes.py`, `.github/scripts/test_ci_changes.py`. No workflow or
permissions edits. No decisions file needed.

**Checks:** `python3 .github/scripts/test_ci_changes.py`. PR title `X-ci-filter-fleet: <short title>`,
branch `slice/X-ci-filter-fleet`. Follow the steward skill; the runner merges.
