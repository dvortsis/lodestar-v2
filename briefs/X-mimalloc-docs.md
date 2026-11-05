# X-mimalloc-docs: follow-ups to the mimalloc override
**Model:** Opus (the soak analyzer feeds the release gate). Fresh Opus reviewer follows. Runner brief, 2026-11-05.

## Why
X-mimalloc-override (#448, merged e62a8b3b) makes mimalloc serve C allocations too on the static musl build (the release and Docker images). Three things it left stale, and its review's nits. Soak r5 on rc.4 pins the analyzer from `main`, so the analyzer fix should land before r5 starts.

## Do
1. **`docs/soak/tools/analyze_cr0.py`:** it subtracts SQLite's memory from "other heap". On musl builds with the override, SQLite's memory is already inside mimalloc's committed bytes, so it's subtracted twice. Make the attribution correct for both builds: decide from evidence the snapshot already carries (e.g. a build/allocator field the collector records, or `docs/metrics.md`'s rows), never from guesswork; if no such field exists, add the smallest one the collector can record and the shipped binary already exposes (check `/metrics` on rc.4's code paths; don't change the shipped binary in this slice). **The memory gate (`private_bytes` slope) must be unchanged:** add a test that the gate numbers are identical before and after on the existing fixtures. Update `test_analyze_cr0.py`.
2. **`crates/lodestar/src/memstats.rs` module doc:** it says mimalloc is "the Rust global allocator (not `override`)" and that C allocations go through the C library's malloc, uncounted. Make it true for both builds. Add a musl-only `pub const C_MALLOC_IS_MIMALLOC: bool = cfg!(all(target_os = "linux", target_env = "musl"));` only if a caller needs it.
3. **`crates/lodestar/src/thp.rs:22-23`:** says `MIMALLOC_ALLOW_THP` covers only mimalloc, not the C heaps; now stale for musl.
4. **`.github/workflows/release.yml` check:** also fail if `nm` shows any `SmpAllocator` or `c.malloc.` symbol (proves no second allocator is linked); fix the step's comment ("malloc and free" → the four checked) and the "ring" mentions in it and in `crates/lodestar/Cargo.toml`'s comment. Verify the new check's grep can't false-positive on the override build (the reviewer found 0 such symbols there).
5. `docs/decisions/2026-11-05-X-mimalloc-override.md`: the "1,843 → 2,085 mappings" wording consistent with the PR body.

## Own
`docs/soak/tools/analyze_cr0.py`, `docs/soak/tools/test_analyze_cr0.py`, `docs/soak/tools/linux/collect.py` only if step 1 needs a field, `crates/lodestar/src/memstats.rs`, `crates/lodestar/src/thp.rs` (comments only), `.github/workflows/release.yml` (the nm step only), `crates/lodestar/Cargo.toml` (the comment only), `docs/decisions/2026-11-05-X-mimalloc-override.md`, and `docs/decisions/2026-11-05-X-mimalloc-docs.md`.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `python3 -I -m unittest` for the soak tools (see `docs/soak/tools/` READMEs). PR "X-mimalloc-docs: …" against `main` from `slice/X-mimalloc-docs`. Follow `.claude/skills/steward/SKILL.md`; message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with PR number and head via the claude-code-remote `send_message` tool. Never merge. Push once per round. No fixed sleeps.
