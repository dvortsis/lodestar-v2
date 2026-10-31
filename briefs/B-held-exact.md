# B-held-exact: re-hold only what the store still holds
**Model:** Opus: content safety (§20), the pipeline writer and the store's filter tables (concurrency). Runner brief, 2026-10-31. The follow-up #282 left. `/security-review` is required. **This slice must never loosen content safety.** If any path would let a held hash be fetched, stored, indexed or shown, it's a blocking defect.

## Why
- After a refile sweep batch (`writer.rs`, `hold_refiled`) and a backfill batch (`writer.rs` ~573), the writer inserts the batch's held hashes into the in-memory `HeldSet` **unconditionally** (#282). That's safe, but over-strict: if a release (`release`, `release_many`, `release_rules`) runs between the store commit and the insert, the released hash is refused again until the next restart reloads `HeldSet` from the store.
- #282's review showed why the obvious shortcut (insert only when the filter version is unchanged) is **unsafe**. Holds from name and class rules aren't on a hash list, and the writer judges only by the current rules. Don't reintroduce anything like it.

## Read first
`docs/decisions/2026-10-31-X-import-refile-2.md` (item 2, and the review history on #282), `-B-import-refile.md`, `docs/architecture.md` §20.3, §20.9 and §20.11. Also `crates/pipeline/src/writer.rs` (all `held.insert` sites), `crates/pipeline/src/lib.rs` (`HeldSet`, intake's `held.contains`/`blocked_hash`), `crates/store/src/filter.rs` (the hold tables, tombstones, quarantine) and `crates/api/src/filters.rs` (`release`, `release_many`, `release_rules`).

## Do
1. **Pick the exact mechanism, and say why in the decisions file.** Either of these is acceptable:
   - **(a) a per-hash hold query:** after the batch commits, the writer asks the store (on the writer connection, in the same order the releases use) which of the batch's hashes are still held, and inserts exactly those;
   - **(b) a removal generation on `HeldSet`:** every release bumps a generation under the same lock it removes under. The writer records the generation before its store commit, and at insert time skips only hashes removed since. Prove that no non-release path can remove a hash.

   Whichever you pick, a hash the store still holds must be in `HeldSet` before intake can see it as not held. Trace each interleaving of commit, insert, release and reload, and list them in the decisions file.
2. **Cover both arms:** the sweep (`hold_refiled`) and backfill.
3. **Tests:**
   - a release between commit and insert leaves the hash released (not re-held);
   - a non-release rule change (a narrowed query, an edited class rule, an exception, a list refresh) between commit and insert leaves the hash held;
   - both arms;
   - restart reload agreement: the in-memory set equals the store's after a reload.
   Deterministic interleavings only (barriers or explicit steps), no sleeps.
4. **Decisions file:** `docs/decisions/<date>-B-held-exact.md`, plus a one-line pointer from the X-import-refile-2 decisions file's item 2.

## Own
`crates/pipeline/src/{writer.rs,lib.rs}` (HeldSet and the two insert sites), `crates/store/src/filter.rs` (the query or generation only), `crates/api/src/filters.rs` (generation bumps only, if you pick (b)), their tests, and your decisions file. **Don't touch** `crates/search/**`, `crates/dht/**` or `web/`. X-polish-1031c is in `crates/api/src/filters.rs` (the re-queue token); if you pick (b) and need the release functions there, tell the runner first so the two don't collide.

## Size
≲ 500 changed lines.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass (with `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`), plus the openapi drift check. Run `/security-review`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
