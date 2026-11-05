# X-search-files-6: close the 10M file-search gate (the last three clauses)
**Model:** Opus (search core). The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-05T22:25Z.

## Why
§3's "File-level query joined to torrents < 150 ms" row at 10M is still missed after #505 (X-search-files-5): cold p95 228.4 ms against 150, warm p95 87.0 ms against 30. The owner put memory and performance first. `docs/decisions/2026-11-05-X-search-files-5.md` "What's left" names three clauses, each a ninth of the mix, that set the p95:
1. `file:(ext:mkv,mp4,avi -path:sample)` (273 ms): a negation inside a file clause runs on the file index.
2. `file:(size:>100MB)` (165–220 ms): 100 MB is not a bucket floor, so its edge bucket (96–128 MiB) is checked file by file.
3. `file:mkv` (166 ms): a word on the file index.

## The runner's design call (§2.26)
**The gate's query mix and its numbers don't change.** The runner never loosens a gate. Close each clause as follows, in this order.

1. **A negated word inside a file clause that has positive extension atoms** (`file:(ext:… -word)`), answered exactly without walking every matching file:
   - Candidates: the torrents the `file_ext_bucket` terms give for the extensions (as #505 answers `ext:` alone).
   - Suspects: the torrents that have at least one file matching the extensions **and** the negated word. These come from the file index, driven by the negated word's postings (`sample` is small), not by the extension's.
   - Answer: candidates minus suspects, plus those suspects that also have a matching file *without* the word. That last check runs on the file index, restricted to the suspects' ids.
   - It must equal the file-index answer exactly: same torrents, order and scores. Extend #505's differential test (`bucket_terms_answer_as_the_file_index_does`) with negated words, including a torrent whose only matching file is a sample, one with both kinds, and words that are common.
   - If a negated word's postings are large (a common word), fall back to the file index. State the threshold and measure it.
   - Other shapes stay as they are.
2. **Finer size codes**, so a decimal-looking bound such as `size:>100MB` (100 × 2^20) falls on or near a code boundary:
   - Choose the split (for example 8 sub-codes per power of two, or split points at 1, 2 and 5 × 10^n × unit) by measuring the mix and the bytes.
   - Re-coding `file_bucket` and `file_ext_bucket` is a **population generation bump** of those fields under SB-6's contract (`population::GENERATIONS`, the resumable backfill, truthful availability while it runs). It is not a `SCHEMA_VERSION` bump. While the new generation backfills, bucket clauses fall back to the file index, as #505 does.
3. **`file:mkv` (a word that is also an extension):**
   - First check exactly what the path tokenizer emits for the extension. If every file with extension X carries the word X, then `file:X` = the ext-X torrents (from the ext terms) ∪ the torrents with the word X in a file whose extension isn't X. Answer the second part from the file index only if its postings can be reached without walking all ext-X files, for example through a field that leaves out the extension token.
   - If closing it needs a **new field or a `SCHEMA_VERSION` bump (v8)**, don't build it. Report the design, its bytes per torrent and its measured gain, and the runner decides. A v8 is only acceptable if it lands before the v0.5.2 cut, so a v0.5.1 user still rebuilds once.

**Byte budget (runner, §2.26):** the torrent index may grow from 391.0 to at most **420 B per torrent** at 10M; §3's ceiling is 512. The file index (757 B) may not grow.

## Do
1. Implement items 1 and 2, with tests as above, plus property tests for the code boundaries (every size maps to one code; codes are ordered; a range's whole codes plus edges cover exactly the range).
2. Investigate item 3, and implement it only within the rules above.
3. **Measure on a rebuilt 10M corpus, by #505's method:** `search-bench --torrents 10000000 --only "broad file"`, cold and warm, `main` against the branch on the same machine. Report the mix and each clause (best / median of three), the cold p95 with 4 at once, and the index bytes per torrent per field. If the disk won't hold the corpus, measure at the largest size that fits and state the gap.
4. `relevance --gate` stays at 0.9186 or better.
5. **Decisions file:** `docs/decisions/2026-11-05-X-search-files-6.md`. It covers what was built, the bytes (flagged to the owner), whether the gate is met, and, if it isn't, what's left with numbers.
6. Add your entry to `runner/slices.yaml` and its `dashboard.yaml` placement only, then run `cd tools/status-connector && npm test`.

## Own
- `crates/search/src/files.rs`, `crates/search/src/compile.rs`, `crates/search/src/population.rs` (the generation entries);
- `crates/search/src/schema.rs`, only if a field's options must change;
- `crates/search/tests/files.rs`, `crates/search/tests/query.rs`;
- the decisions file.

**Shared, minimal:** `crates/search/src/index.rs`, for the plan's dispatch and the snapshot checks, as #505 did.

**Don't edit `crates/search/src/doc.rs` until B13b (#509) merges.** It's about to; the runner will tell you. Bring `main` in then if you need it. Don't touch `crates/pipeline/**` or `crates/api/**`. If you need a hook there, message the runner.

## Done when
These pass: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (per crate if the disk is short; say so) and `relevance --gate`.

Open the PR against `main`, titled "X-search-files-6: 10M file-search gate: negated words, finer size codes". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the 10M table, using the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round. No fixed sleeps. Times are UTC. Work frugally: the weekly usage limit was at its warning level until 2026-11-06T00:00Z.
