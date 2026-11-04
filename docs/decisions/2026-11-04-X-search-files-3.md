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
- **Method.** The bench's broad-file section, with one change: its per-clause table gains a `cold ms` column, each clause alone with the file-set cache emptied, best of three. "Cold" here, as in X-search-files-2, means the file-set cache emptied before each query; the OS page cache is warm. Each scale's corpus was built once (`--dir`), then `before` (main at fc32466, with the same bench patch) and `after` were run against the same index. Cold and warm p50 and p95 are over the bench's mix of nine clauses, 300 queries, half under a common word.
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
- **10M:** see the section below.

## 10M
TO FILL

## Not done
- **An edge scorer under a boolean in pieces starts from document 0.** `EdgeScorer` settles on its first in-range file when made, before the piece's seek, so each piece of a nested range walks its edge bucket's head. That's a bucket's files, not the segment's (Tantivy's range doc set did the same over every document), and only for `And`/`Or`/`-` ranges; a bare range seeks first.

## Tests
- **`crates/search/tests/files.rs` (new), through the whole index:** a corpus with a file at every bucket boundary a store can hold (each floor, one below and one above, 0, 1 and the largest signed length), checked against a scan of every file's size for open ranges both ways, exact sizes and closed ranges inside one bucket, across one edge and across many (over 1,000 ranges); zero, huge and past-the-top ranges; the parsed forms (`size:>1GiB`, `size:>=1GiB`, `size:<1GiB`, `size:0`); and the same ranges under a boolean (with `ext:`, negated, in an `OR`).
- **`files::bucket_props` (property test):** random segments of random sizes (bias to bucket floors ±2, 0 and `u64::MAX`), with deletions and, in some segments, every other file written without a bucket: the bucketed range, the column scan, both under a boolean, and the negation agree with a scan oracle, whole and in pieces of 1 to 20 documents. `inner_buckets_come_from_postings` shows a whole bucket is answered from postings without reading the column, bare and under a boolean (it fails with `SizeWeight` forced onto the column). Treating edge buckets as whole fails the property test and `only_edge_buckets_are_checked`.
- **`a_bucket_scan_stops_when_cancelled`:** the bucket scan checks its stop at least every `CHECK_EVERY` files, counted across buckets, and gives up when told.
- **`the_column_fallback_matches_nothing_outside_the_segments_sizes`:** the clamp's regressions, deterministically (a range below every size, a segment of one size, above and below it).
- **Existing tests unchanged,** except `a_scan_in_pieces_finds_what_a_whole_scan_does`, whose index has no bucket field, now says so (`bucket: ABSENT`, the column path).

## Shared file
- **`bench/src/bin/search-bench.rs`:** the per-clause `cold ms` column only (reporting, as the brief allows). No gate or class changed.
