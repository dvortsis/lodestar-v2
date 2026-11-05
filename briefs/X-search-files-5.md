# X-search-files-5: the 10M file-search gate, continued
**Model:** Opus (search core). A fresh Opus reviewer follows. Runner brief, 2026-11-05T07:22Z. This continues X-search-files-4 (#468, merged at dfc91d2d) in the same session.

## Why
After #468, the 10M gate is still missed: cold p95 is 199.5 ms and warm p95 193.9 ms, against 150 ms and 30 ms. The remaining cost is per matching file: reading each file's torrent id and size from the columns. The runner's calls on #468 still stand:
- the gate's mix stays unchanged;
- `-path:sample` stays on the file index;
- P2 and the skip of `rest` when `whole` is picked come here.

## Do (branch `slice/X-search-files-5` from `origin/main`)
1. **Torrent-level ext × size-bucket terms** for `file:(ext:… size:…)`. They're exact for one file, since a file has one extension and one size bucket. Index them under SB-6's population contract:
   - check whether the schema version must bump, and if so, follow the population and backfill rules;
   - an old index without the terms falls back to the file index and never returns empty.
2. **Option (b), torrent-level size terms,** for the bare `size:` clauses inside `file:( … )`.
3. **P2:** stop opening postings per segment when the size check falls back to the plain conjunction.
4. **Skip building the `rest` scorer per piece** when `checks` picks `whole`.
5. **Prove the results are identical.** A differential test must show every positive query shape returns the same torrents and the same priors order as the file-index path, including:
   - lone and combined clauses;
   - `OR`;
   - negation (the accepted rule stands);
   - unusual bucket edges, such as sizes exactly on a bucket boundary.

   Ranking must be unchanged. `relevance --gate` must stay at 0.9186 or above.
6. **Measure on a rebuilt 10M corpus,** by the same method as #468: `search-bench --torrents 10000000 --only "broad file"`, cold and warm. Report main against branch for the mix and for each clause, and the extra index bytes per torrent. If the container's disk won't hold the corpus, say so and measure at the largest size that fits, with the gap stated.
7. **Decisions file:** `docs/decisions/2026-11-05-X-search-files-5.md`. It covers the bytes per torrent the new terms cost (§2.26, flagged to the owner) and whether the gate is now met.

## Own
`crates/search/src/files.rs`, `crates/search/src/compile.rs`, `crates/search/src/schema.rs` and `doc.rs` (only for the new terms and their population), `crates/search/tests/files.rs`, `crates/search/tests/query.rs`, and the decisions file.

Don't touch `crates/pipeline/**`; SRC-13b and ENR-1 are running there. If the population path needs a pipeline hook, message the runner.

## Done when
These pass: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (per crate if the disk is short; say so), and `relevance --gate`.

Open the PR against `main`, titled "X-search-files-5: 10M file-search gate, continued: torrent-level ext x size-bucket terms and size terms". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the 10M table, using the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round. No fixed sleeps.
