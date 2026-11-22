# X-search-files-3 decisions

`file:(size:…)` reads `size_bucket` postings (SB-6, #415) instead of every file's size, for §3's "File-level query joined to torrents < 150 ms" row at 10M (the I2/I4 gate X-search-files-2 found). No schema, index format or API change. Applies `2026-10-31-X-search-files-2.md`'s proposal as `2026-11-04-SB-6.md` refined it.

## How a size clause runs now (`crates/search/src/files.rs`)
- **Buckets wholly inside the range are read from their postings; only the edge buckets check the `size` column.** A range `[lo, hi]` touches buckets `size_bucket(lo)..=size_bucket(hi)`. A bucket is whole when its floor is at least `lo` and its ceiling (the next floor less one, `u64::MAX` for bucket 127) at most `hi`; only the first and last can be partial (`SizeRange::buckets`, test `only_edge_buckets_are_checked`). An edge bucket's postings are read and each file's size checked, so the column is read only for files in at most two buckets, each spanning at most half its lowest size.
- **Both size-clause paths use them:**
  - **A bare `size:` range** (the scan in pieces, `Source::Size`): each bucket's postings are read from the piece's first document to its last, 64 at a time, edge buckets filtered through `first_vals` (`scan_buckets`). The gather's form follows the buckets' postings lengths, which count the files the range can match, instead of the guessed tenth of the segment.
  - **A range under a boolean** (`ext:mkv size:>1GB`, `-size:…`, `a | size:…`, and `matched_files`): `SizeWeight` gives Tantivy's union of the whole buckets' term postings and one `EdgeBucket` scorer per edge (its postings, skipping files outside the range). It's lazy, so as the follower of an intersection it seeks through skip lists rather than building a set.
- **No path reads every file's size** where the segment's buckets are complete. The column path stays only as the fallback below.

## When the buckets can't answer: per segment, from the segment itself
- **A segment answers from buckets only when every document in it has one:** the bucket terms' document frequencies sum to `max_doc` (deleted documents count in both). Every document written since v6 carries exactly one bucket (`FileDoc::to_document`, which says so), so the sum falls short exactly when a document lacks one: an older index's documents (the field is `ABSENT` there, so the check is skipped), or ones a backfill hasn't rewritten. Such a segment uses the column, as before. The check streams at most 128 terms, once per piece and per scorer (each `matched_files` call builds one); not memoised, since it's small beside the postings it gates.
- **This bypasses `SearchIndex::prefilter`, a refinement of SB-6's population contract for the runner to flag (owner §2.19).** SB-6 says every prefilter on a tracked field goes through it; `prefilter` is in `index.rs`, which this slice may not edit (SRC-3 and X-merge-policy are working there), and `files::compile` sees only the fields. The per-segment check is exact for the snapshot being searched, so a lease reading a snapshot from mid-backfill is right by construction, and no document is ever treated as "no match" for lacking the field (such a segment is scanned whole).
- **It detects missing buckets, not stale ones.** A later change to the bucket definition (`size_bucket`, `bucket_floor`) would leave old documents with one bucket each, which this check accepts. **Such a change must use a new field name** (so old segments lack it and fall back), or this check must also consult the field's population generation through `SearchIndex::prefilter`. Noted at `files::size_bucket`.

## A bug fixed in the column fallback
- **The column's range search was called with unclamped bounds.** Tantivy's `Column::get_docids_for_value_range` assumes a range within the column's own minimum and maximum (its `RangeQuery` clamps first). Called with `size:<3` on a segment whose smallest file is 5 bytes, or on a segment of one distinct size, it returned files outside the range. The new property test found it (a single file of `u64::MAX` bytes matched `size:0`). The bounds are now clamped, and a range outside the segment's values reads nothing. Before this slice every bare size clause went through this path; it would have shown as wrong results on small or uniform segments.

## Measured (`search-bench --only "broad file"`, release build, 4 cores, 15 GiB)
- **Method.** The bench's broad-file section, with one change: its per-clause table gains cold columns, each clause alone with the file-set cache emptied, three runs: the best and (from the 10M runs on) the median. "Cold" here, as in X-search-files-2, means the file-set cache emptied before each query; the OS page cache is warm. Each scale's corpus was built once (`--dir`), then `before` (main at fc32466, with the same bench patch) and `after` were run against the same index. Cold and warm p50 and p95 are over the bench's mix of nine clauses, 300 queries, half under a common word.
- **200,000 torrents (the CI size):**

  | | cold p50 | cold p95 | warm p50 | warm p95 | cold, 4 at once p95 |
  |---|---:|---:|---:|---:|---:|
  | before | 8.76 | 19.08 | 2.28 | 4.77 | 63.65 |
  | after | 6.90 | 12.45 | 2.34 | 5.62 | 28.97 |

- **1,000,000 torrents:**

  | | cold p50 | cold p95 | warm p50 | warm p95 | cold, 4 at once p95 |
  |---|---:|---:|---:|---:|---:|
  | before | 26.10 | 58.13 | 4.30 | 9.55 | 261.12 |
  | after | 21.91 | 37.14 | 4.53 | 10.09 | 132.99 |

- **Per clause, cold, alone (ms, before → after):**

  | clause | 200k | 1M |
  |---|---:|---:|
  | `file:(size:>100MB)` | 14.94 → 7.47 | 58.33 → 27.41 |
  | `file:(size:>1GB)` | 19.36 → 6.52 | 50.77 → 23.05 |
  | `file:(ext:mkv size:>1GB)` | 12.85 → 9.56 | 45.79 → 37.25 |
  | `file:(ext:mkv)` (no size, for scale) | 8.20 → 6.31 | 20.94 → 20.38 |

  The size clauses now cost about what an extension clause of similar breadth does: they read postings for the files they match, not every file. Warm times are unchanged (the sets are the same; noise only). The matched torrent counts are identical before and after at every scale.
- **10M:** see the next section.

## 10M: the gate is missed (cold p95 264 ms against 150 ms)
- **Gate missed at 10M: cold p95 263.7 ms, warm p95 259.8 ms against 150 / 30; tracked by X-search-files-4.** The runner merges this PR as the size-path improvement (2026-11-04). §3's file-level gate stays open, not loosened: nothing here claims it met.
- **Corpus:** `search-bench --torrents 10000000 --dir …`, built once in this container: 10,000,010 torrents, 9 torrent-index segments, and by the 1M ratio about 295M file documents. Storing took about 70 min and indexing about 80, interrupted once by a container restart and resumed from the index's watermark. The store and index (about 19 GB) were read through once after the restart, so the OS page cache was warm again for the timed runs, as at 1M. `before` (main at fc32466 with the bench patch), `after` (this PR without the join change below) and `after + join` ran against the same index.
- **The mix (ms):**

  | | cold p50 | cold p95 | warm p50 | warm p95 | cold, 4 at once p95 |
  |---|---:|---:|---:|---:|---:|
  | before | 192.78 | 423.07 | 189.52 | 417.37 | 2120.33 |
  | after | 174.81 | 302.56 | 157.56 | 273.85 | 1103.64 |
  | after + join | 156.56 | 263.65 | 153.28 | 259.77 | 1006.60 |

  Both gates fail at 10M: cold (150 ms) and warm (30 ms). The warm pass is no better than cold because the file-set cache's 8 MiB holds 94 sets and hits 8.8% of segment lookups; `2026-11-04-L4-followups.md` predicted this from the charge's shape (about 12 MiB wanted).
- **Per clause, cold, alone (ms; after + join shows best / median of three):**

  | clause | before | after | after + join |
  |---|---:|---:|---:|
  | `file:mkv` | 153.28 | 161.70 | 140.62 / 143.73 |
  | `file:(ext:mkv)` | 150.80 | 160.13 | 130.40 / 130.42 |
  | `file:(ext:srt)` | 63.64 | 65.91 | 59.92 / 60.20 |
  | `file:(ext:flac,mp3)` | 160.79 | 182.13 | 161.42 / 163.12 |
  | `file:(ext:jpg)` | 200.00 | 200.26 | 194.88 / 197.14 |
  | `file:(size:>100MB)` | 420.02 | 233.24 | 187.11 / 188.12 |
  | `file:(ext:mkv,mp4,avi -path:sample)` | 251.96 | 276.28 | 222.17 / 223.01 |
  | `file:(size:>1GB)` | 388.97 | 182.89 | 151.64 / 154.90 |
  | `file:(ext:mkv size:>1GB)` | 383.99 | 309.62 | 266.81 / 267.10 |

  The size clauses fell by half to two-fifths, as at 1M. Match counts are identical in every run. Non-size rows differ between `before` and `after` only by run-to-run noise, since this PR doesn't change their path.
- **Where the time goes (measured, timers in a throwaway build, not committed):** for `file:(ext:mkv)`, about 85 ms scanning the file index (the postings of tens of millions of matching files, on 4 threads) and about 55–65 ms in the join to torrents (each of 9 segments reading 1.6M ids, 22–32 ms each, 4 at a time, plus about 10 ms building the union bitset). For `size:>100MB` the scan is about 155 ms, since more files match, plus the same join. **Every broad clause is now linear in its matching files plus the corpus, and the size path is no longer the floor:** `ext:jpg` and `ext:mkv,mp4,avi -path:sample` cost as much as the size clauses do.
- **The join reads ids a block at a time (`IdsWeight::matches`, this PR):** the torrent index's `id` column is read 1,024 values at a time when it's full (every torrent has an id), instead of one lookup a document. It took 10–30 ms off each clause at 10M (the "after + join" column). Test `the_join_marks_each_document_whose_id_is_in_the_set` (blocks, a partial last block, ids out of order).
- **What would meet 150 ms at 10M (not in this slice's scope or ownership), with the runner's ruling on each (2026-11-04):**
  1. **Answer one-atom file clauses from the torrent index.** `file:(ext:mkv)` and `file:mkv` ask nothing a single file's co-occurrence adds, and the torrent index already holds every file's extension and path words. That skips both the file scan and the join for four of the nine clauses. **It changes results:** smart coverage leaves some pack files out of the file index, and the torrent index covers them all. So it's a semantics decision for the runner and owner, and it needs `compile.rs`. **Goes to X-search-files-4**, which measures it, and its result-semantics change is decided on evidence before any code.
  2. **Torrent-level `size_bucket` terms** (each torrent's distinct file buckets, multi-valued, in the torrent index): a bare size range becomes torrent postings for its inner buckets, with the file index needed only for the two edge buckets. That's a schema change (M8's bump) with a budget cost to measure, owned by `schema.rs` / `index.rs` / `doc.rs`. **Goes to X-search-files-4**, which measures it.
  3. **The file-set cache at 10M.** At 8 MiB the warm path is a cold path. Sizing it with the corpus is the owner's memory guarantee (§3, `2026-10-30-L3-L5-landing.md`), so it's an owner question, not a runner one. **Off the table** (runner): it's the owner's memory guarantee.
  4. **More cores.** The scan is parallel over segments and pieces; the gate's 4 cores are the floor of the target hardware, so this only lowers what the owner host sees. **Not a fix** (runner).
- **The runner's item 1, the lazy edge settle, is not done:** it doesn't move these numbers. In this mix each edge bucket's first file is already in range (`>100MB` and `>1GB` exclude only files of exactly 100 MiB or 1 GiB), so settling stops at once. Nor can it be made lazy under a boolean: Tantivy's union reads every child's `doc()` when it's built. See "Not done".

## Not done
- **The 10M gate:** missed, tracked by X-search-files-4 (see the options above).
- **The lazy edge settle (runner item 1):** a follow-up. It doesn't move this mix's numbers, and under Tantivy's union it can't be lazy without a different scorer shape (the union reads `doc()` when built).
- **An edge scorer under a boolean in pieces starts from document 0.** `EdgeScorer` settles on its first in-range file when made, before the piece's seek, so each piece of a nested range walks its edge bucket's head. That's a bucket's files, not the segment's (Tantivy's range doc set did the same over every document), and only for `And`/`Or`/`-` ranges; a bare range seeks first.

## Tests
- **`crates/search/tests/files.rs` (new), through the whole index:** a corpus with a file at every bucket boundary a store can hold (each floor, one below and one above, 0, 1 and the largest signed length), checked against a scan of every file's size for open ranges both ways, exact sizes and closed ranges inside one bucket, across one edge and across many (over 1,000 ranges); zero, huge and past-the-top ranges; the parsed forms (`size:>1GiB`, `size:>=1GiB`, `size:<1GiB`, `size:0`); and the same ranges under a boolean (with `ext:`, negated, in an `OR`).
- **`files::bucket_props` (property test):** random segments of random sizes (bias to bucket floors ±2, 0 and `u64::MAX`), with deletions and, in some segments, every other file written without a bucket: the bucketed range, the column scan, both under a boolean, and the negation agree with a scan oracle, whole and in pieces of 1 to 20 documents. `inner_buckets_come_from_postings` shows a whole bucket is answered from postings without reading the column, bare and under a boolean (it fails with `SizeWeight` forced onto the column). Treating edge buckets as whole fails the property test and `only_edge_buckets_are_checked`.
- **`a_bucket_scan_stops_when_cancelled`:** the bucket scan checks its stop at least every `CHECK_EVERY` files, counted across buckets, and gives up when told.
- **`the_column_fallback_matches_nothing_outside_the_segments_sizes`:** the clamp's regressions, deterministically (a range below every size, a segment of one size, above and below it).
- **`an_edge_bucket_explains_without_seeking_back`:** `EdgeBucket::explain` for a file before the scorer's first match answers "no match" instead of seeking backwards, which `SegmentPostings` forbids (it panics on the debug assertion without the fix; runner review item 2).
- **`the_join_marks_each_document_whose_id_is_in_the_set`:** the block-read join (see the 10M section).
- **Existing tests unchanged,** except `a_scan_in_pieces_finds_what_a_whole_scan_does`, whose index has no bucket field, now says so (`bucket: ABSENT`, the column path).

## Shared file
- **`bench/src/bin/search-bench.rs`:** the per-clause cold columns (best and median) only (reporting, as the brief allows). No gate or class changed.
