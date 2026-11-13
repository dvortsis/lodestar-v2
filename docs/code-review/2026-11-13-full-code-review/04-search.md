# Search review
Reviewed at 55e155a9.

Nothing was built or run (brief): every finding is from reading the code. "Confirmed" means the code path was traced end to end; "likely" means traced, with a trigger or a library behaviour I could not execute.

## Coverage
- Read in full (non-test source): `crates/search/src/` `query.rs`, `compile.rs`, `tokenizer.rs`, `filter.rs`, `cancel.rs`, `lease.rs`, `sorted.rs`, `cache.rs`, `index.rs` (its tests too), `live.rs`, `rebuild.rs`, `percolate.rs`, `recovery.rs`, `retry_dir.rs`, `schema.rs`, `doc.rs`, `lib.rs`; `cursor.rs` and `count.rs` up to their tests; `docs/query-language.md`; architecture §8; `runner/standards.md`; decisions `2026-11-12-GS-0.md`, `2026-10-26-soak-search.md`, `2026-10-22-B7.md` (memory part); `crates/api/src/search.rs` (`base_query`, `request_ast`, `run_search`, `index_state`).
- Skimmed: `files.rs` (schema, `file_docs`, buckets, `compile_node`, `IdSet`, `FileSetCache`, `torrent_ids`: lines 130-640 and 1195-1815), `facets.rs` (clause ownership, `facets`, `pass`, `Counts`: lines 236-395 and 596-1245), `regex.rs` (limits and `parse`, first 400 lines), `seek_guard.rs`, `natural.rs` (limits), `suggest/titles.rs` (header, builder loop), `population.rs` (`decode`, `reconcile`, walks), `stem.rs`, `explain.rs` (weights).
- Not read: `files.rs` scorers and scan (`SizeRange`, `SizeChecked`, `EdgeBucket`, `scan_segment`, `bad_torrents`, lines 640-1195 and 1815-2400) and its tests; `suggest/mod.rs`, `suggest/caret.rs`, `suggest/fuzzy.rs`; `catalogue.rs`, `handle.rs`, `log_events.rs`, `testing.rs`; `facets/tests.rs`, `lease/tests.rs`, `cache/tests.rs`; `crates/search/tests/*` (only searched for sleeps, ignores and specific cases). Time ran out; the unread scorers are the largest gap, since they implement tantivy `DocSet`s by hand.

## Findings

### SEARCH-01 Building a torrent's document is quadratic in its distinct file extensions, and they are indexed without a cap
- Severity: blocking
- Kind: security
- Confidence: confirmed for the loop and the missing cap; the stall time is an estimate, not measured
- Where: `crates/search/src/doc.rs:195-220` (`exts: Vec<String>`, `!exts.contains(&ext)` per file), `:272-274` (every distinct extension becomes an `ext` term), `:423-431` (`extension`: any ≤ 10 ASCII alphanumerics); limits in `crates/metainfo/src/info.rs:37-41` (`max_files: 200_000`, 16 MiB info)
- What: `TorrentDoc::new` keeps the distinct extensions in a `Vec` and scans it for every file. Directory names beside it use a `HashSet`. A torrent with 200,000 files, each with its own extension (`a.x0001`, `a.x0002`, …), costs about n²/2 = 2×10¹⁰ string comparisons on the single indexer thread: tens of seconds or more per torrent, during which nothing commits, and again on every re-read (a `reindex_log` replay, a population backfill, a rebuild). Paths are capped (`MAX_PATH_TOKENS`) and years, seasons and episodes are capped; extensions are not. The same torrent also writes up to 200,000 terms into `ext` (inverted index and the fast string column) and as many `file_ext_bucket` pairs, against a 512 B per torrent budget.
- Failure scenario: an attacker serves a few such info dictionaries (about 7 MiB each) to the crawler. Each one stalls indexing, and the alerts that follow commits, for its duration, and adds megabytes of garbage terms. A rebuild pays for all of them again.
- Suggested fix: dedupe with a `HashSet` (or sort and dedup once), and cap the extensions and ext/bucket pairs indexed per torrent, as paths are capped. Add a test with a many-extension tree and a time bound.
- Tracked?: not found

### SEARCH-02 Sorted `sig:<info hash>` searches serve stale per-segment results from the browse cache
- Severity: blocking
- Kind: correctness
- Confidence: likely (traced by reading; not executed)
- Where: `crates/search/src/sorted.rs:66` (cache key: query key, order, k, segment id, deleted count), `:130-137`, `:158-173`; `crates/search/src/index.rs:1473-1495` (the compiled key has no part for `sig:<hash>`); `crates/search/src/compile.rs:1141-1186` (`SamePayload` resolves the reference torrent's signature across the whole searcher at `weight()` time); used from `crates/search/src/lease.rs:336-349` and `index.rs:1655-1662`
- What: `SegmentCache` assumes one segment's matches for a query depend only on that segment and its deletes. `sig:<40 hex>` breaks that: which documents of segment A match depends on the reference torrent, which can sit in, arrive in or leave another segment, and on whether it passes the visibility clauses. Hot columns (`@hot{epoch}`) and file clauses (`@files{generation}`) carry a key part for exactly this reason; `sig:<hash>` carries none. `docs/decisions/2026-11-12-GS-0.md` argues freshness only for the lease and the generation-keyed result cache.
- Failure scenario: clones C1..Cn sit in old segments. Someone runs `sig:H` sorted by size before the torrent with hash H is indexed (or while it is unverified or hidden): each segment caches "0 matches". H is then indexed in a new segment (or promoted, or released). The same search sorted by size now returns only H while its count (computed fresh) and the relevance order include every clone. It stays wrong until those segments merge or take a delete, or the 4,096-entry cache is emptied. The reverse (reference deleted or hidden later) keeps listing clones, against GS-0's "a withheld reference looks unknown".
- Suggested fix: when `node.mentions_same_payload()`, put the resolved reference (signature or none) or the searcher generation into `Compiled.key`, or bypass the segment cache. Add a test that runs `sig:<hash>` under a field sort across a commit that adds the reference.
- Tracked?: not found (`tests/payload_sig.rs` only uses `Sort::Relevance`)

### SEARCH-03 One stored torrent that fails to load stalls indexing, alerts and any rebuild indefinitely
- Severity: important
- Kind: correctness
- Confidence: confirmed
- Where: `crates/search/src/index.rs:1883-1888` (`load`), `:2284-2290` (`catch_up_new`), `crates/search/src/live.rs:220-261` (retry from the committed watermark, no skip), `crates/search/src/rebuild.rs:987-997`; `crates/store/src/lib.rs:1014-1016` (unknown codec, decompress or `filetree::decode` failure is an `Err`)
- What: `load` propagates any store error for one id. The live indexer answers every error by reopening its writer and reading again from the watermark, so a row that fails every time (a corrupt `torrent_files` blob, or a file-tree format a newer build wrote before a downgrade: `CodecError::UnsupportedFormat`) is read again every 30 s for ever. Only an unreadable enrichment record is tolerated (`index.rs:1943`).
- Failure scenario: one bad blob at id N. Every torrent after N is stored but never searchable; `errors` climbs with the same `last_error`; percolation, which follows commits, stops alerting. A rebuild never reaches its snapshot and never swaps.
- Suggested fix: after repeated decode-class failures on the same id, index the torrent without its file tree (or skip it), log and count it once, and let the watermark move.
- Tracked?: not found

### SEARCH-04 Decomposed (NFD) Japanese and Korean names never match their composed queries
- Severity: important
- Kind: correctness
- Confidence: confirmed
- Where: `crates/search/src/tokenizer.rs:77-88` (`is_cjk` covers U+3040–30FF, which holds the combining voiced marks U+3099/U+309A, and the conjoining jamo U+1100–11FF), `:146-150` (CJK characters skip normalization)
- What: non-CJK text is NFKD-folded with marks removed, but anything `is_cjk` is pushed as is, with no composition first. A name stored decomposed (file names made on macOS commonly are) keeps `カ` + U+3099 instead of `ガ`, and Hangul as jamo instead of syllables. The mark and each jamo count as CJK characters of their own, so the bigrams differ entirely from those of the composed text an input method types. Half-width `ｶﾞ` takes a third path (NFKD) and loses its voiced mark.
- Failure scenario: a torrent named with the NFD form of ガンダム is not found by the query `ガンダム`; an NFD Korean name is not found by its syllables.
- Suggested fix: compose (NFC) before classifying, so both sides produce the same characters; that changes tokens, so bump `TOKENIZER_VERSION`. Add NFD cases to the tokenizer tests and `tests/tokenizer_props.rs`.
- Tracked?: not found

### SEARCH-05 `/search` has no server-side time budget
- Severity: important
- Kind: resource
- Confidence: confirmed
- Where: `crates/api/src/search.rs:445-449` (a fresh `CancelToken`, cancelled only when the client disconnects), `:484-511`; compare `crates/api/src/facets.rs:213` and `crates/api/src/suggest.rs:95` (`cancel.with_budget(BUDGET)`); `crates/search/src/lease.rs:125-146` (lease creation runs up to two counts)
- What: facets, suggestions, the exact count (5 s) and zero-result recovery (20 ms) all run under a deadline. The search itself, first page and every cursor page, runs until it finishes as long as the connection stays open. The parser bounds the query (64 clauses, 2,048 bytes) but not its cost: each `xx*` prefix unions every posting of every term with that prefix in two fields, per segment, and a first page runs the query up to three times (estimate, exact count below 200,000, page).
- Failure scenario: a signed-in client (or a Torznab key holder) holds open a few dozen connections, each with 64 two-letter prefixes ORed. Each ties up a blocking-pool thread and the four shared search threads for many seconds at 10M torrents; interactive searches queue behind them.
- Suggested fix: give `run_search` a budget (`cancel.with_budget`) and answer a 503-class error when it runs out; consider a small concurrency cap on searches.
- Tracked?: partly. `M8-SEC-2` (security review, "Denial of service through query cost is in scope") is planned; no slice adds a budget.

### SEARCH-06 A rebuild that retries a failed step can drop a torrent from the new index
- Severity: important
- Kind: data-loss
- Confidence: likely (traced by reading; the trigger needs a store read to fail right after another succeeded)
- Where: `crates/search/src/index.rs:2157-2173` (replay: `torrents.reindex_seq = seq` and `delete_term` run before the fallible `torrent_doc(store, …)?`), `crates/search/src/rebuild.rs:966-998` (the `Err` arm keeps the same `Rebuild` and calls `step` again)
- What: the live indexer drops its `Indexer` after any error and reopens from the committed watermarks (`live.rs:220-243`), so half-applied work is discarded. `rebuild_and_swap` does not: on `Err` it pauses and calls `builder.step` again on the same `Indexer`. In the `reindex_log` replay the in-memory `reindex_seq` is advanced and the old document's delete is queued before `torrent_doc` reads the enrichment and source rows. If that read fails (the comment at `rebuild.rs:988` expects "a busy database"), the delete stays queued, the entry counts as applied for the torrent index, and the retry skips it (`seq > torrents.reindex_seq` is false).
- Failure scenario: during a multi-hour rebuild, a replayed entry for torrent T hits a transient store error inside `torrent_doc`. The next commit persists the delete and a `reindex:` watermark past the entry. After the swap T is stored but not searchable until something logs it for reindexing again, or the next rebuild.
- Suggested fix: do every fallible read before `delete_term` and before advancing `reindex_seq`; or on a step error drop and reopen the `Rebuild` from its commit, as the live indexer does. Add a fault-injection test.
- Tracked?: not found

### SEARCH-07 A rebuild never checks free disk space
- Severity: important
- Kind: gap
- Confidence: confirmed (no free-space call exists in `crates/search`)
- Where: `crates/search/src/rebuild.rs:493-496` (`note_disk` only records a high-water mark), `:966-998` (step errors retried without limit), `:982-985` (a failed commit ends the rebuild as `Failed`, leaving `.building`)
- What: a blue/green rebuild needs roughly the index's size again beside the serving index. Nothing estimates or checks that before starting or while building. When the volume fills, step errors are retried every 30 s for ever, or a commit error marks the rebuild `Failed`; either way the partial `.building` directory stays and keeps the disk full, so the serving index's live commits (and the store, if it shares the volume) fail too.
- Failure scenario: an upgrade bumps `SCHEMA_VERSION` on an Unraid box whose appdata share has less free space than the index size. Mid-rebuild, searching goes stale and the crawler's writes start failing, with only `REBUILD_RETRY` in the log.
- Suggested fix: before building, compare free space with the serving index's size plus headroom; refuse with a clear state for the banner, and delete (or offer to delete) `.building` when a build fails for space.
- Tracked?: not found (`G-rebuild-10M` and `G-v07-scale` measure the high-water mark; nothing guards it)

### SEARCH-08 `verified:`, `source:` and `network:` are parsed but absent from `docs/query-language.md`
- Severity: important
- Kind: docs
- Confidence: confirmed
- Where: `crates/search/src/query.rs:477-480` (FIELDS), `:1529-1546` (`without_unverified`); `docs/query-language.md` (no mention of any of the three)
- What: the page calls itself "the precise reference: every field, operator and error", but three fields the parser accepts are missing, and so is the default that every search adds `verified:yes` unless the query names `verified:`.
- Failure scenario: a user can't discover how to see unverified items (`verified:yes,no`) or filter by source, and the doc-example test doesn't pin these fields.
- Suggested fix: add a provenance section with `query` examples.
- Tracked?: partly. `M8-DOC-1` ("docs/query-language.md at 1.0") owns the page; it doesn't name these fields.

### SEARCH-09 `docs/query-language.md` promises more than paging and ranking deliver
- Severity: polish
- Kind: docs
- Confidence: confirmed (first two points); suspected (third, tantivy source not on disk)
- Where: `docs/query-language.md:463` ("Scrolling down never repeats or skips a row") against `crates/search/src/lease.rs:122-124` ("a torrent scraped between two pages can move across the cursor": priors and hot sorts read live swarm values); `docs/query-language.md:453` ("a larger one for words found in the resolved title") against `crates/search/src/schema.rs:50-55` (name 3.0, title 2.0); `docs/query-language.md:96` (NEAR "in either order") against `crates/search/src/compile.rs:1326-1331` (a tantivy slop phrase, which as far as I know charges 2 for a transposition, so `a NEAR/1 b` would not match "b a")
- What: three sentences of the user reference don't match the code. No test pins NEAR in reversed order.
- Suggested fix: reword the first two; add a reversed-order NEAR test and either adjust the slop or the sentence.
- Tracked?: `M8-DOC-1` owns the page

### SEARCH-10 Zero-width and other format characters split words instead of being dropped
- Severity: polish
- Kind: correctness
- Confidence: likely
- Where: `crates/search/src/tokenizer.rs:90-106` (`classify`: anything not numeric, cased or alphabetic is `Sep`), `:151-158` (only combining marks are removed)
- What: U+200B, U+200C/D, U+00AD, U+2060, U+FEFF and the bidi marks are category Cf, not combining marks, so they become separators. `Bat\u{200B}man` indexes as `bat`, `man`.
- Failure scenario: a name with an invisible character inside a word isn't found by that word (`batman`).
- Suggested fix: drop Cf characters in `normalize` (keeps offsets, since each `NChar` carries its source range); needs a `TOKENIZER_VERSION` bump, so do it with SEARCH-04.
- Tracked?: not found

### SEARCH-11 A file-index error makes `-file:(…)` match everything
- Severity: polish
- Kind: correctness
- Confidence: confirmed
- Where: `crates/search/src/index.rs:1437-1456` (any error but cancel/timeout becomes `IdSet::default()`), `crates/search/src/percolate.rs:213` (saved searches compile the same way)
- What: the empty-set fallback is right for a positive clause (no results) but inverts a negated one: `-file:(ext:exe)` then excludes nothing. It is logged (`FILE_SEARCH_FAILED`), not surfaced, and a saved search's watermark moves past the range.
- Suggested fix: fail the search (or mark the response incomplete) when a file clause under a negation can't be answered.
- Tracked?: the fallback is recorded as existing behaviour in `docs/decisions/2026-10-26-soak-search.md`; the negated case isn't discussed

### SEARCH-12 Small parser edges
- Severity: polish
- Kind: correctness
- Confidence: confirmed (overflow, units); suspected (stack depth)
- Where: `crates/search/src/query.rs:776-788` (`unary` recurses once per `-`/`!`; `MAX_DEPTH` counts only parentheses, so 2,047 minus signs recurse 2,047 deep on the async worker's stack); `:1746-1757` (`t - f`, `f - 86_400`, `t - 86_399` overflow when a client AST carries `At(i64::MIN/MAX)`: a panic in a debug build, wrapped and then refused as `invalid_ast` in release, reached from `crates/api/src/search.rs:976-990`); `:1289-1294` (`size:1.12345678TB`: eight or more fraction digits on TB overflow `checked_mul` and the value is refused); `:895-898` (`ab**` reports "prefixes need at least 2 letters")
- Suggested fix: count negations in a loop; use checked arithmetic in the render arms; scale the fraction before multiplying.
- Tracked?: not found

## Checked and found sound
- Client ASTs are validated by render → parse → equality (`crates/api/src/search.rs:976-990`), so every parser limit (2,048 bytes, 64 clauses, depth 16, NEAR 1–50 and 16 words, tag and extension lengths) applies to POSTed ASTs too.
- Adult visibility is ANDed on outside the user's query (`api::search::base_query`), so no query shape (`-(-type:adult)`, OR) reaches Adult while it is hidden; `sig:<hash>` resolves its reference through the same clauses (`compile.rs:1126-1166`).
- Fuzzy and prefix terms are bounded by `MAX_TOKEN_BYTES` = 64 (`tokenizer.rs:25`, `compile.rs:1348-1355`), so the Levenshtein automaton and the prefix regex stay small; `tag_value` caps at 64 bytes before calling the release parser.
- `compile`'s `expect("liftable")` (`compile.rs:437`) holds for every clause `liftable`/`always_lifted` accepts; unit positions are non-decreasing, so `t.position - first` (`compile.rs:1328`) can't underflow.
- Offset and limit are clamped before any arithmetic (`MAX_LIMIT` 200, `MAX_WINDOW` 10,000, `saturating_add`); cursors decode strictly (`cursor.rs:181-234`) and a lease refuses a position of another kind (`lease.rs:286-293`) or another client (`lease.rs:1059`).
- Counts: estimates are flagged (`Total.approx`), anything within twice the threshold is counted exactly (`count.rs:99-114`), sampling is stratified with fixed jitter and scaled by covered documents.
- Two-phase relevance pages don't overlap within a snapshot: windows are bounded by the previous window's last phase-1 candidate and ties break by document address (`lease.rs:393-480`, `index.rs:662`).
- Commit and crash consistency of the live path: each index's watermark, `reindex_log` position and population state ride in its own tantivy commit payload; the file index commits first; a side that is behind catches up alone (`index.rs:2284-2313`); after any error the live indexer discards its writer and resumes from the commit (`live.rs:220-243`).
- Rebuild naming and swap: only a finished build gets the complete name (marker, then rename; a crash between the two is finished at the next start, `rebuild.rs:629-633`); the old live indexer stops before the new index's last catch-up; a retired index is deleted only after its leases are gone or aged out; newer versions are left alone.
- A schema mismatch serves read-only by field name, with absent fields matching nothing and reported (`index.rs:732-779`); an index lacking a required field is passed over, and one that fails to open fails the start instead of being discarded.
- The result cache key covers query, scope, sort, page size, generation, `now` for relative dates, zone for `time:` and the hot epoch; single flight resolves on leader failure or panic (`cache.rs:612-628`).
- The mmap cache is pruned after every commit (`index.rs:197-204`, `:2367`), with a regression test.
- Tests in the crate wait with polling loops under a deadline; the sleeps found only pace a writer. One `#[ignore]` (`tests/cache.rs:376`, a timing test).
- The regex planner (`regex.rs`) checks length, nesting, node count, repetition, alternation, fold cost and compiled weight before translating; it is not wired to queries yet (the parser answers "regex search arrives with the trigram index"; `RX-1` is the planned slice).

## Area-level concerns
- The browse cache (`sorted.rs`) is bounded by entries only (4,096, up to 1,000 rows each plus the query's debug text as key) and is emptied wholesale when full. A query with a relative date (`discovered:<7d`, the reference's first example) carries `@{now}` in seconds, so it never hits and only fills the cache; the result cache has the same miss pattern.
- Leases hold their `file:(…)` id sets outside every budget (up to 1,024 leases, 64 per client). `docs/decisions/2026-10-26-soak-search.md` records this as deferred; nothing since bounds it.
- The title FST build is recorded as too large at 10M (`docs/decisions/2026-10-22-B7.md`: about +0.9 GB at its peak); it still rebuilds from the whole store at most every 60 s while commits arrive, and opens the store with `StoreConfig::default()` rather than the configured one (`suggest/titles.rs:555`).
- Percolation evaluates each torrent once, when it is first indexed. Clauses on data that arrives later (swarm counts, as `percolate.rs:223-225` says; enrichment that lands through `reindex_log`) can't alert for it. I did not check how often enrichment follows the first index.
- Writers use tantivy's default merge policy; documents replaced by `reindex_log` replays and backfills are reclaimed only when their segment merges. No gate measures index size after churn (the size gate builds a fresh index).
- After a downgrade to a build with a lower schema version, nothing "older" exists to serve, so the build opens an empty index and fills it live with no rebuild banner, even though a complete newer index sits beside it (`rebuild.rs:637-649`, `:889-899`).
- `files.rs` is 4,700 lines with several hand-written `DocSet`/`Scorer` implementations and bit-level set code; I did not read the scorers, and they deserve a dedicated pass.
