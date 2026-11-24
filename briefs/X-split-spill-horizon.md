# X-split-spill-horizon: Sampler split: what happens when the spill filter reaches its fallback, and how spilled cooldowns age out

**Runner brief, 2026-11-24 (UTC).** Core crawler slice, Opus. Target < 1,200 changed lines. **Start only after X-split-resume-gate (#1118) has merged.** It edits the same sampler files.
- **Branch:** `slice/X-split-spill-horizon` from fresh `origin/main`, with #1118 on it.
- **PR title:** `X-split-spill-horizon: Sampler split: what happens when the spill filter reaches its fallback, and how spilled cooldowns age out`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. If you touch the restart snapshot's format, say so in the PR: the runner's fresh Opus review covers it, and `restart.rs` is then in your owns only after you ask the runner.

## Why
Runner records r17 (`docs/decisions/2026-11-24-runner-records-r17.md`), from X-test-vm's split lane on 2026-11-23: spill slots grew about 360k an hour. At that rate they reach the 7.86M fallback after about 20 h of continuous sampling. A long-lived install, and a 24 h CR-COMB run, must never hit it.

## Read first
- `docs/owner-decisions.md` §2.77 and anything on cooldowns and the split.
- `docs/decisions/2026-11-20-X-sampler-cooldown-split.md` (the spec), r17, and X-split-resume-gate's decisions file (just merged: spill snapshot version 2, persisted hash keys, the fallback gate, the restart memory bound).
- CR-0d's decisions file (the restart proof).
- `crates/dht/src/node/sampler.rs`, `sampler/**`, the crawler-sim scenarios, `runner/standards.md`, and `docs/metrics.md` for any metric you touch.

## What to do
1. State and test what happens at the fallback today: the behaviour, the metrics, and whether any cooldown is shortened or any node sampled early. Fix the fallback if it isn't safe.
2. Age or evict spilled entries so the filter never reaches the fallback in steady state. An entry can go once its cooldown is over by its own record, never sooner. **Never shorten a cooldown.** A node whose entry was evicted early must be treated as the spec says for a lost record, never as free.
3. Memory: state the steady-state bound and measure it (sim or bench), at r17's rate of 360k an hour and at 2× that.
4. Keep CR-0d's restart proof and X-split-resume-gate's restart behaviour true. The split-off path is unchanged; pin its numbers.
5. Sim test: a run over 24 h simulated at r17's spill rate never reaches the fallback. Property test the aging logic. No fixed sleeps: use a paused clock, or condition waits of at least 10 s.
6. Decisions file `docs/decisions/<date>-X-split-spill-horizon.md`: the fallback's behaviour, the aging rule, the memory bound, and §2.19 refinements to flag. Your `runner/slices.yaml` entry exists; edit only its owns line if you must.

## Owns
`crates/dht/src/node/sampler.rs`, `sampler/**`, `crates/crawler-sim/tests/sim/**`, and your decisions file. Never beside another slice that edits `node/**`. Ask the runner before anything else.

## Done
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (and `-p lodestar-dht --features testing`). Report `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green. Push once per round of fixes.

Follow-up, not yours: X-restart-fuzz-v2 (the restart fuzz target for snapshot version 2, and sealing SNAPSHOT_SPILL_VERSION) runs after you or beside you; it owns `fuzz/` and the restart files.

No AWS or cloud calls, no new accounts, no spending, no force-push. Held security detail stays off every branch.
