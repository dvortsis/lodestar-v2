# X-polish-1031c: search, filters API and DHT hint follow-ups
**Model:** Opus: search concurrency, the filters API (content safety) and the DHT node (untrusted input). Runner brief, 2026-10-31. Optional findings from the #283, #278 and #279 reviews. No schema change. `/security-review` is required, because item 2 touches the classification re-queue (content safety): **it must not loosen anything.**

## Read first
CLAUDE.md, `runner/standards.md`, `docs/decisions/2026-10-31-X-search-files-perf.md`, `-X-search-files-2.md`, `-X-polish-1031b.md`, `docs/architecture.md` §3, §7 and §20.9.

## Do
1. **Search (#283 review):**
   - In `crates/search/src/files.rs` (~709-717, 789), when one segment's scan fails with a non-cancel error, stop the sibling segments: set a local `AtomicBool` beside the executor call, and have `stopped()` check it as well as the token.
   - Add a line to the X-search-files-2 decisions file: cold file scans now share the file pool's ≤4 threads across requests, so the 10M measurement must include concurrent cold queries.
   - Comment at ~801 that tantivy 0.27's range doc set returns `num_docs/10` as its size hint (an upstream TODO), so the bitset choice for `size:` ranges is an estimate.
   - Make the scan test (~1368) pair parts to segments by first id, not by position.
2. **A shutdown token for the classification re-queue (#278 review).** The re-queue in `crates/api/src/filters.rs` stays **uncancellable by deadline** on purpose: cancelling it would leave a new Is-adult rule's existing matches unmarked. Give it the server's **shutdown** token only, so a stopping server doesn't wait on it. On shutdown mid-re-queue, make sure the work is redone at the next start, or say exactly why it doesn't need to be. Nothing may be left unmarked once the server is running again. Add a test: shutdown mid-re-queue, restart, and the matches are marked.
3. **The DHT hint compaction bound (#279 review).** In `crates/dht/src/node/hints.rs` (~134, `compact_order` ~217), make the amortised bound unconditional: after compacting, set `next_compact = max(ORDER_CAPACITY, 2 * order.len())` and check that, or skip `push_back` when `order.back() == Some(&(now, key, s))`. Add a test where `retain` can't drop entries, so the deque still stays bounded and no capture rescans every time.
4. **Tests:** condition waits only, no fixed sleeps, deadlines ≥ 10 s.
5. **Decisions file:** `docs/decisions/<date>-X-polish-1031c.md`, one line per item.

## Own
`crates/search/src/files.rs` and its tests, the X-search-files-2 decisions file (one line), `crates/api/src/filters.rs` (the re-queue token only) and its tests, any startup hook needed to redo an interrupted re-queue (name it in the PR), `crates/dht/src/node/hints.rs` and its tests, and your decisions file. **Don't touch** `crates/pipeline/**`, `crates/store/src/import.rs`, `crates/filter/**` or `web/`: X-import-refile-2 (#282), S-logs-1 and M-security-settings are open.

## Size
≲ 400 changed lines.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass (with `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`), plus the bench gates. Run `/security-review`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
