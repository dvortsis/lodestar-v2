# X-sec-S1-17 (Opus, /security-review)

Runner brief, 2026-11-20. Finding S1-17 (low; surface 1, the DHT reply path).

**The detail comes by message from the runner only.** It never goes into a branch file, this brief, runner-state, a commit message, the PR title or body, or a decisions file, until the fix has merged.

## Owns
- crates/dht/src/node/lookup.rs
- crates/dht/src/node/actor.rs (only the part the runner's message names)
- crates/dht/src/node/lookup/** and crates/dht/tests/** for new tests
- docs/decisions/2026-11-20-X-sec-S1-17.md (neutral wording: name the id, severity and surface only, and describe the change as a hardening of how reply values are budgeted)

## Rules
- Fix on `main` after X-sampler-cooldown-split (#981, merged 20f4bbae) and CR-LOOKUP-END (#977). Branch from the latest origin/main.
- Run `/security-review` on your diff before you ask for review.
- No reproducer input in fuzz/seeds/. Tests in this PR may exercise the fixed behaviour; keep their names and comments neutral.
- No new switch, unless the runner's message says so.
- ≲ 300 changed lines.
- Checks: cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test -p lodestar-dht (the whole crate). No fixed sleeps. No cloud calls.
