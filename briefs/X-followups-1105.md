# X-followups-1105: review follow-ups from #462 (SRC-17a) and #465 (RP-2)
**Model:** Opus (API concurrency and logging). A fresh Opus reviewer follows. Runner brief, 2026-11-05T05:20Z. Start from `origin/main` **after #462 has merged** (run `git log origin/main --oneline | grep -i 'SRC-17a'`; if it hasn't, wait on that condition and re-check every few minutes, without fixed sleeps in code).

## Why
Each fresh review left small non-blocking findings in files no running slice owns. Every item is small and has its own test.

## Do (branch `slice/X-followups-1105`)
1. **`plugin_names` single-flight** (api crate, added by #462).
   - Today, concurrent first requests, and every request while `ops.sources()` keeps failing, each call `control.sources()`, which takes the store lock.
   - Make the cache single-flight with a bounded retry (for example a `tokio::sync::OnceCell` or a mutex-guarded refresh with a back-off).
   - Test: N concurrent first calls hit `sources()` once, and a failing source doesn't cause one store-lock acquisition per request.
2. **Discover Newest skips provenance work.**
   - `search_get` with no viewer runs `plugin_names` and `Catalogue::read`, then throws the result away.
   - Skip both when the caller doesn't need provenance. Output must stay byte for byte the same (golden).
3. **`partial: true` in the search crate.**
   - Add a search-crate test where the source facet comes out `partial: true` from `availability_in`. Today only `false` is asserted there.
4. **The `filter_note` kind for pack overrides** (`crates/pipeline/src/log_events.rs`).
   - The note "pack override <ID> fails closed: unreadable" is logged as kind `other`.
   - Give it the "pack override " prefix match with kind `pack_override`, as `docs/metrics.md` names it.
   - **This needs RP-2 (#465) merged first.** If it hasn't merged when you reach this item, leave it out and say so in the PR.
   - Test: that note maps to `pack_override`.

## Own
- The `plugin_names` cache and the `search_get` path in `crates/api` (find them with `git log -p` on #462's merge). Don't touch auth or legal files; LEGAL-1 owns those.
- One test in `crates/search`'s facet tests.
- The `filter_note` mapping in `crates/pipeline/src/log_events.rs`, with its test.
- `docs/decisions/2026-11-05-X-followups-1105.md` (short).

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass.
- No fixed sleeps.

Open the PR against `main` with the title "X-followups-1105: review follow-ups from SRC-17a and RP-2". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round.
