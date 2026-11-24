# X-escalate-full-table — brief (runner holder, 2026-11-24T09:25Z)

Kind: core, model opus. Deps: CR-SAMPLE-BACKOFF-ESCALATE (merged). Slot 2, freed by NOS-2b (#1109).
Decided by: the runner holder (the owner's Duet chat), which does the runner's work while the runner is paused. Launched now because the owner lifted the no-new-builds hold (§2.83) on 2026-11-24 ("You can ignore those instructions"); r17 had parked this slice only under that hold.

## Why
Runner records r17 (`docs/decisions/2026-11-24-runner-records-r17.md`, "The escalate gap"), from X-test-vm's esc2 screen and the runner's reading of `crates/dht/src/node/sampler.rs` on 0db3b28a (around lines 219 `make_room` and 385-391 `next`):
- With the cooldown split off and the table full, `Sampler::next()` returns `None` before it reaches the frontier, because `make_room` refuses.
- So a struck node never gets its 300 s second sample, and CR-SAMPLE-BACKOFF-ESCALATE's "a full table included" line holds only with the split on.
Main has moved since 0db3b28a: read `sampler.rs` on current main first and confirm the gap before changing anything. If it no longer reproduces, stop and tell the runner holder.

## Do
1. When the table is full, let `next()` still pop a node the sampler already tracks: its sample replaces the node's own record, which `reserve`'s `was` path already allows. A node it doesn't track still needs room as today.
2. Keep it behind the existing `dht.sample_backoff_escalate` switch, default off. Rate and outstanding caps unchanged. Never shorten a cooldown.
3. A sim test in `crates/crawler-sim/tests/sim/**`: split off, table full, escalate on; a struck node gets its second sample at about 300 s. Plus the same with escalate off, showing today's behaviour is unchanged. No fixed sleeps; use the sim clock.
4. Its own decisions file `docs/decisions/2026-11-24-X-escalate-full-table.md` corrects CR-SAMPLE-BACKOFF-ESCALATE's "a full table included" line. Never edit the older file.
5. The PR adds this slice's `runner/slices.yaml` gate entry only if one names it (the slice entry already exists); run `cd tools/status-connector && npm test` if you touch that file.

## Owns
`crates/dht/src/node/sampler.rs`, `crates/dht/src/node/sampler/**`, `crates/crawler-sim/tests/sim/**`, `docs/decisions/*-X-escalate-full-table.md`. Never beside another slice that edits `node/**` (X-split-resume-gate and X-split-spill-horizon wait until this merges).

## Process
- Branch `slice/X-escalate-full-table`; PR titled `X-escalate-full-table: Sampler backoff escalation reaches a struck node on a full table with the split off`.
- Pre-push: fmt, clippy -D warnings, cargo test --workspace. Push once per round of fixes.
- Merge only after a fresh Opus review and green ci-ok; the runner holder merges.
- Known flake: `crates/store` `unverified_cap_index::the_kept_count_matches_the_rows` fails at random (fix queued as X-flake-cap-index-move). It doesn't block this PR (§2.18); cite it on the PR if it hits.
- No AWS or cloud calls, no new accounts, no spending.
