# X-gluetun-pin-ci-filter (runner brief, 2026-11-13)

The spec is the slices.yaml entry `X-gluetun-pin-ci-filter` (rv-815 O6). It's a cloud-testing slot (§2.62).

**Owns:**
- `.github/scripts/ci_changes.py`;
- `.github/scripts/test_ci_changes.py`.

**Why:** the gluetun post-rules pin checks don't run when only the rules files or the docs that quote them change.
- `test_the_docs_pin_the_post_rules_they_give` (`infra/fleet/tests/test_steps.py`) runs in the `infra` job, which `INFRA` gates.
- The crate-vs-packaging copy check (`crates/companions/tests/templates.rs`) runs in the Rust jobs.

**Do:**
1. Widen `INFRA` in `ci_changes.py` to `packaging/companions/gluetun-post-rules.txt`, `docs/user/03-keep-lodestar-safe.md` and `docs/operations.md`.
2. Add `packaging/companions/` to `RUST_ALSO`, so `templates.rs` runs when only the packaging copy changes.
3. `COMPANION_PINS` (the companion-pins job) is not the gate. Leave it.
4. Add a `test_ci_changes.py` case for each new path, showing which output turns on.
5. Run `python3 .github/scripts/test_ci_changes.py` locally.

**Hard limits:** no workflow `permissions:` changes, no secrets, no workflow file edits.

Steward skill; PR title "X-gluetun-pin-ci-filter: …". Sonnet builds it; the runner reviews it.
