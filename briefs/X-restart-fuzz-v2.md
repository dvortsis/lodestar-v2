# X-restart-fuzz-v2: The restart fuzz target reaches the version 2 spill snapshot

**Runner brief, 2026-11-24T23:10Z.** Core, Opus (an on-disk format parser of untrusted bytes). Target < 400 changed lines.
- **Branch:** `slice/X-restart-fuzz-v2` from fresh `origin/main` (with #1118, X-split-resume-gate, merged as 16fa8c8c).
- **PR title:** `X-restart-fuzz-v2: The restart fuzz target reaches the version 2 spill snapshot`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. A fresh Opus review gates the merge.
- Add this slice's own `runner/slices.yaml` entry (kind core, model opus, the owns below, notes "from #1118 and rv-1118") and a placement line in `runner/dashboard.yaml` next to X-purge-rederive's. Run `cd tools/status-connector && npm test`.

## Why
#1118 added restart snapshot version 2, which carries the spill section when something spilled. `fuzz/fuzz_targets/dht_restart.rs` seals only at `restart::VERSION` (its `seal`, line 21), so the version-2 parser is never fuzzed. rv-1118 (reviews/rv-1118.md on runner-state) found the parser strict and bounded by review and proptest. This slice proves it under the fuzzer.

## Read first
`docs/decisions/2026-11-24-X-split-resume-gate.md` (the version 2 format, the caps and the fallback gate), CR-0d's decisions file (the restart proof), `crates/dht/src/restart.rs`, `fuzz/fuzz_targets/dht_restart.rs`, `fuzz/README.md`, and `runner/standards.md`.

## Do
1. The target seals at both `VERSION` and `SNAPSHOT_SPILL_VERSION`; one more input byte picks which. Keep the existing invariants for both: no panic, whatever decodes re-encodes to the same bytes, and a resuming snapshot never seeds a deadline earlier than the one it records. For version 2, add one more invariant: a restored spill never ends a cooldown earlier than its record.
2. Old to new: a version-1 snapshot decodes the same as before. A version-2 snapshot with an empty spill is refused, or written as version 1, whichever `restart.rs` says. Assert that.
3. Add seeds to the corpus: a minimal version-2 snapshot and one at the caps, made by a small generator or a test that writes them, never hand-written bytes. Keep the CI fuzz job's time as it is.
4. The rv-1118 nit in `restart.rs` (about line 1175): seeding clones the spill section (up to 32 MiB) where it could move it. Move it if the change stays local and keeps every test green. Otherwise say why in the PR.

## Owns
`fuzz/fuzz_targets/dht_restart.rs`, `fuzz/corpus/dht_restart/**` (or wherever its seeds live), `fuzz/README.md` (one line, if needed), `crates/dht/src/restart.rs` (the seeding move only) and its tests, plus your slices.yaml entry and placement line. **Not `crates/dht/src/node/**`**: X-split-spill-horizon (#1122) owns the sampler, and the proof.rs nit waits for it.

## Done
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. If cargo-fuzz is installed, run the target for at least 5 minutes on each version locally, and say what you ran. If it isn't installed, CI's `fuzz (dht_restart)` job is the run. No fixed sleeps. Report `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green. Push once per round of fixes.

No AWS or cloud calls, no new accounts, no spending, no force-push. Held security detail stays off every branch.
