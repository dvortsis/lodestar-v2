# X-flake-cap-index-move — brief (runner holder, 2026-11-24T08:55Z)

Decided by: the runner holder (owner's Duet chat), under CLAUDE.md "a flake in someone else's test gets fixed once" and owner-decisions §2.18. Reported by ENR-11's build session (PR #1110), which did not fix it.

## The flake
- Test: `crates/store/tests/unverified_cap_index.rs`, proptest `the_kept_count_matches_the_rows` (from a44eaced, V16). Fails at random on any PR that runs `cargo test --workspace`.
- Shrunk case: `[FindBt(0,0,true), Move(0), FindBt(0,0,true), Move(0)]`.
- Cause (runner's reading of the test on main): `Op::Move(n)` runs a raw `UPDATE unverified_items SET network = 4 WHERE network = 0 AND key = ?1`. After the first Move, a second FindBt re-inserts the BitTorrent row for the same key; the second Move then tries to make a second `(4, key)` row and hits `UNIQUE(network, key)`, so `.unwrap()` panics. The bug is in the test's model, not in the store.

## The fix (test code only)
1. Make `Op::Move` skip a key whose `(network 4, key)` row already exists (for example `AND NOT EXISTS (SELECT 1 FROM unverified_items WHERE network = 4 AND key = ?1)`), so the op still models "an item's network changed outside the API" without asking for an impossible state. Don't swallow other errors.
2. Commit the shrunk case as `crates/store/tests/unverified_cap_index.proptest-regressions` (proptest's file beside the test), so it replays every run.
3. No product code changes. If the reading above is wrong and the store itself misbehaves, stop and tell the runner holder.
4. Prove it: the shrunk case passes, then the proptest at 10,000 cases (`PROPTEST_CASES=10000`) passes; say both in the PR.

## Process
- Branch `slice/X-flake-cap-index-move`, PR titled `X-flake-cap-index-move: Keep the cap-index proptest's Move op from asking for a duplicate row`.
- The PR adds its own `runner/slices.yaml` entry (kind core, model opus, deps [], owns below) and a placement rule if none covers it; run `cd tools/status-connector && npm test`.
- Owns: `crates/store/tests/unverified_cap_index.rs`, `crates/store/tests/unverified_cap_index.proptest-regressions`, `docs/decisions/*-X-flake-cap-index-move.md`.
- Usual pre-push checks (fmt, clippy -D warnings, cargo test --workspace). A fresh Opus review before merge. No fixed sleeps.
