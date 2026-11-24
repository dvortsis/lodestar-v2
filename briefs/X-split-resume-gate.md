# X-split-resume-gate: Sampler split: shorten or remove the resume hold after a restart from a spilled state

**Runner brief, 2026-11-24 (UTC).** Core crawler slice, Opus. On-disk format risk: a fresh Opus review of any change to the restart snapshot's format. Target < 1,200 changed lines.
- **Branch:** `slice/X-split-resume-gate` from fresh `origin/main` (after #1112, X-escalate-full-table, merged as dde6436f).
- **PR title:** `X-split-resume-gate: Sampler split: shorten or remove the resume hold after a restart from a spilled state`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Why
X-test-vm saw that after a clean restart from a spilled state, with the split on, sampling was held (reason `resumed`) for 5 h 25 min on the real network (runner records r17, `docs/decisions/2026-11-24-runner-records-r17.md`). That conflicts with owner-decisions §2.77: no sampling wait after any start, and no blanket gate carried across a restart. §2.77 requires this fix before the split can ship on by default, and in the runner's reading before it's offered as a user setting at all. The owner lifted the §2.83 hold on crawler slices (runner, 2026-11-24).

## Read first
- `docs/owner-decisions.md` §2.77 (and anything on restarts, cooldowns and the split); `docs/decisions/2026-11-20-X-sampler-cooldown-split.md` (the spec); r17's split and resume sections; CR-0d's decisions file (the restart proof) and CR-SAMPLE-BACKOFF-ESCALATE's; `docs/decisions/*-X-escalate-full-table.md` (just merged; it changed sampler.rs `held`/frontier).
- `crates/dht/src/node/sampler.rs` and `sampler/**`, `crates/dht/src/node/proof.rs`, `crates/dht/src/restart.rs` (the restart snapshot's on-disk format), and the crawler-sim scenarios.
- `runner/standards.md`; `docs/metrics.md` for any metric you touch.

## What to do
1. Find why the hold lasts hours: what `resumed` waits for once cooldowns have spilled, and which state the snapshot carries or drops.
2. Shorten or remove the hold so a restart from a spilled state samples at once for nodes whose cooldown is known to be over, without **ever shortening a cooldown**: a node still cooling down by its own record stays held, and a node whose record was lost is treated as the spec says, never as free.
3. Keep CR-0d's restart proof true. If the snapshot format must change, version it, read the old format, write a migration test, and say so in the PR so the runner's review covers the format.
4. Sim test: a restart from a spilled state starts sampling within a bounded time (state the bound) with no cooldown shortened; the split-off path is unchanged (pin its numbers). Property test if the hold logic has state. No fixed sleeps; paused clock or condition waits ≥ 10 s.
5. Decisions file `docs/decisions/<date>-X-split-resume-gate.md`: the cause, the bound, whether this clears §2.77 for the split, and anything for the owner (§2.19 refinements flagged).
6. Your `runner/slices.yaml` entry exists; edit only its owns line if you need to.

## Owns
`crates/dht/src/node/sampler.rs`, `sampler/**`, `crates/dht/src/node/proof.rs`, `proof/**`, `crates/dht/src/restart.rs`, `restart/**`, `crates/crawler-sim/tests/sim/**`, your decisions file. Never beside another slice that edits `node/**` or `restart.rs`; X-split-spill-horizon waits for you. Ask the runner before anything else.

## Done
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (and `-p lodestar-dht --features testing`). Report `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green.

Landing lane: ENR-G (Rust) is in the lane now; hold Rust pushes until it merges, and keep working locally.

No AWS or cloud calls, no new accounts, no spending, no force-push. Held security detail stays off every branch.
