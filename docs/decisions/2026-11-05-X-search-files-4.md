# X-search-files-4 decisions

§3's "File-level query joined to torrents < 150 ms" row at 10M, after X-search-files-3 (#429) left it at cold p95 264 ms. This slice answers one-atom extension clauses from the torrent index (option (a), runner's ruling 2026-11-05, flagged to the owner under §2.19), cuts the file scan's per-piece and per-file costs in `files.rs`, and measures option (b). **The gate is still missed at 10M: cold p95 199.5 ms, warm 193.9 ms (gates 150 / 30).** No schema, index format or API change.

## `file:(ext:…)` alone is the torrent's extensions (option (a), extensions)
- **A `file:(…)` clause whose only atom is `ext:` (`file:(ext:mkv)`, `file:(ext:flac,mp3)`, and alternatives of them, `file:(ext:mkv | ext:mp4)`, which is `ext:mkv,mp4`) is the torrent index's own `ext` terms, as a filter** (`compile::file_exts`). It scores as the clause it replaces did: nothing at the top level, where the id set was lifted into the collector's filters (a constant there would squeeze the re-ranker's text differences against the priors; the test checks the scores are a word's alone), and a constant 1.0 nested, as `IdsQuery` does. The torrent's `ext` holds every file's extension, so no file-index scan or join runs. At 10M these clauses went from 52–211 ms to 2.6–15 ms.
- **It changes results only by coverage, and only ever adds torrents** (removes them under a negation, `-file:(ext:x)`, its complement). Smart coverage (§8) leaves small images, documents and other files out of the file documents of a pack over `SMART_ALL_FILES`, and BEP 47 hidden files out of every torrent's; the torrent's extensions keep them. Examples, pinned by `one_atom_extension_clauses_cover_every_file_and_words_stay_on_the_file_index` (`crates/search/tests/files.rs`):
  - a 1,200-photo pack of 3 MiB JPEGs: `file:(ext:jpg)` used to miss it and now finds it, as `ext:jpg` does;
  - a torrent whose only `.mkv` is hidden: `file:(ext:mkv)` now finds it.
  - Media, software and files of 5 MiB or more were always covered (mkv, flac, srt, mp3), so those clauses are unchanged.
- **The relevance fixture and the synthetic bench are unchanged.** No fixture torrent has over 1,000 files or a hidden file; `relevance --gate` gives NDCG@10 0.9186, the baseline. At 1M and 10M every clause's torrent count is identical before and after.
- **Two or more atoms stay on the file index** (`file:(ext:jpg size:>1MB)` asks about one file), and so do words.
- **Matched files still come from the file index.** `compile::file_clauses` still lists the clause, so `SearchIndex::matched_files` is unchanged; `files::compile` wraps it as `TorrentLevel`, which `files::torrent_ids` gives no set for, so the search's file pass scans nothing for it (test `a_torrent_level_clause_is_not_scanned`). A hit gained through an uncovered or hidden file shows no matched files, as the file index has none.
- **`file:word` stays on the file index** (runner: only with a fallback that keeps every result it finds today). `path:` stops at `MAX_PATH_TOKENS`, so it misses a file deep in a huge pack that the file index finds (the test's `zebra` case), and the index keeps no per-torrent marker of truncation. A fallback would still scan the word's file postings, so it would save nothing. Its 10M number: 125 ms best, 147 ms median.
- **Docs:** `docs/query-language.md` ("Files inside torrents") says an extension on its own checks every file and may show no matched files, and the summary in `crates/search/src/query.rs` gains the line. §8's description of `file:(…)` (architecture.md 230, 2208) is superseded by this file.

## The file scan (`crates/search/src/files.rs`)
Profiled at 1M and 10M (perf, release build). Each item was measured on its own; the 10M table below is all of them.
- **Columns are opened once per query and segment, not once per piece or scorer.** Opening a blockwise-linear column reads its whole block table (a few bytes per 512 documents); at 10M's ~300M file documents, with four pieces a large segment, that was about a tenth of a size clause. The `torrent` column is shared by a segment's pieces; `size` columns go through `SegColumns`, shared by pieces and by the size check's scorers.
- **Full columns are read without optional slots** (`ColumnValues::get_vals` into a plain array), and a run of one torrent's files pushes its id once.
- **`ext:x size:…` checks each file's size in the column behind the other clauses** (`SizeChecked`, compiled for a conjunction with a required range and another required clause). At the top of a clause the scan reads the other clauses a block at a time within its piece, with its usual cancellation checks (`Source::Checked`); nested (under `-` or `|`, or beside a torrent's term for matched files) it's a scorer that keeps Tantivy's `fill_buffer` contract. Intersected with a bucketed range, Tantivy decoded and merged every bucket's postings (about 30 for `>1GB`) through a union only to seek through it: a third of the clause. Where the ranges' buckets hold fewer than a quarter of the files the other clauses match (`CHECK_RATIO`), the plain conjunction runs instead. Both give the same files; the property test checks both paths against a scan oracle, forced either way.
- **A union of terms is read term by term** (`TermUnion`: one term, `Should`-only alternatives of terms, optionally beside `MustNot` clauses). Tantivy's union sorts documents the gather doesn't need sorted; each term's postings are read in turn, and a piece's excluded documents are read into a bitset first. `ext:mkv,mp4,avi -path:sample` went from 243 to 168 ms against the build before it (272 to 172 ms from `main`, the table below). Each piece's exclusion bitset takes a bit per document of the piece, transient: at 10M's largest segments about 9 MiB a piece, up to four at once a query, released when the piece ends. Test `a_term_union_scan_finds_what_its_scorer_does` compares it with the scorer, whole and in pieces, and checks the shapes it mustn't take.
- **The join's id union is built a word at a time** from the parts kept as bitsets, not an insert per id, and tested as a plain word array (`Union`).
- **Pieces of one segment can take different gather forms** (a short last piece can fall under the bitset's threshold): joining them puts the list into the bitset rather than the bitset into a list to sort.
- **#429's review nits:** the join test asserts the column is `ColumnIndex::Full`, and two new tests cover ids past `u32::MAX` (no union; per-part checks on the block path) and a column with gaps (the per-document path). The column-fallback test's comment now says it's `bucket: ABSENT` that forces the column. The `column_opt` guard in the bare range's column path is marked belt and braces: every file document carries a size; the clamp is the guard results depend on. A piece's gather form now scales the scorer's or buckets' whole-segment count to the piece's share of the documents.

## Measured at 10M (`search-bench --torrents 10000000 --only "broad file"`, release, 4 cores, 15 GiB)
- **Method:** as #429. The corpus was rebuilt in this container (10,000,000 torrents; 8 torrent-index segments, about 300M file documents; about 19 GB, more than RAM, so some reads come from disk; the runs were CPU-bound, 3.4 cores busy and few major faults). `before` is `main` at b08e161, `after` this branch, against the same index, one after the other. Cold means the file-set cache emptied before each query; the OS page cache is warm. The mix is the bench's nine clauses, 300 queries, half under a common word.
- **The mix (ms):**

  | | cold p50 | cold p95 | warm p50 | warm p95 | cold, 4 at once p95 |
  |---|---:|---:|---:|---:|---:|
  | before | 170.36 | 284.53 | 174.48 | 288.14 | 1097.06 |
  | after | 106.77 | 199.52 | 108.38 | 193.94 | 998.85 |

  Both gates fail: cold (150 ms) and warm (30 ms). The warm cache hits 4.6% of segment lookups (8.8% before: the clauses left on the file index are the largest sets, which crowd each other out of 8 MiB).
- **Per clause, cold, alone (ms, best / median of three):**

  | clause | before | after |
  |---|---:|---:|
  | `file:mkv` | 145.62 / 147.71 | 125.28 / 146.76 |
  | `file:(ext:mkv)` | 146.37 / 146.99 | 15.10 / 15.19 |
  | `file:(ext:srt)` | 52.32 / 52.91 | 2.62 / 2.64 |
  | `file:(ext:flac,mp3)` | 171.38 / 171.73 | 5.72 / 6.02 |
  | `file:(ext:jpg)` | 202.20 / 211.32 | 4.71 / 4.77 |
  | `file:(size:>100MB)` | 207.54 / 215.75 | 192.25 / 198.58 |
  | `file:(ext:mkv,mp4,avi -path:sample)` | 272.01 / 278.21 | 171.60 / 172.36 |
  | `file:(size:>1GB)` | 170.57 / 179.34 | 152.41 / 152.77 |
  | `file:(ext:mkv size:>1GB)` | 279.14 / 285.57 | 201.24 / 214.70 |

  Matched torrent counts are identical in every row. These runs predate the self-review's fixes (the top-level score of a lone `file:(ext:…)`, the size check read within each piece instead of through its scorer, the gather join); none of them adds work to these clauses' paths, and the corpus had to be deleted for disk space before they could be re-run.
- **Where the rest goes:** in every clause left on the file index, reading each matching file's `torrent` (and, for size, `size`) from its blockwise-linear column is 30–45% of the time, and the join to torrents 6–8%. That is per matching file, and at 10M the broad clauses match tens of millions of files: no further constant factor in `files.rs` takes them under 150 ms on four cores.

## What would meet 150 ms (for the runner)
- **Option (b), torrent-level `size_bucket` terms** (each torrent's distinct file buckets): measured on the synthetic generator at 200,000 torrents, 5.13 distinct buckets a torrent (44% have one), **4.0 B a torrent** of postings and terms, so the torrent index would go from 413 B to about 417 B of its 512 B budget, about 40 MB at 10M. It turns `size:>100MB` and `size:>1GB` into torrent postings plus the two edge buckets' files. The runner deferred it to its own slice (it owns `schema.rs`, `doc.rs`, `index.rs`; SB-6's population contract) until this report.
  - **Upgrade path:** a new torrent-index field (say `file_bucket`, multi-valued u64, indexed, no fast field), written by `TorrentDoc` from the file tree it already reads, under SB-6's population contract as a tracked field: new and reindexed documents carry it, a backfill reindexes the rest, and `SearchIndex::prefilter` reports the field complete only once the population record says so. Until then a bare range runs on the file index as now. Per segment, as the file index's `bucketed` check does, a segment whose documents all carry the field (each torrent with any file carries at least one bucket, so the terms' document frequencies can't simply sum to `max_doc`; it needs the population generation or a "has buckets" marker term) answers from it; any other falls back. No rebuild is forced: old indexes keep working, slower, until the backfill finishes.
- **It isn't enough on its own:** `ext:mkv size:>1GB` (201–215 ms) and `ext:mkv,mp4,avi -path:sample` (172 ms) are each a ninth of the mix, above the 5% a p95 allows. **Proposed for the runner: torrent-level `ext × size_bucket` terms** (a torrent has a file of this extension in this bucket, so `ext:mkv size:>1GB` is the union of its inner buckets' terms, exact for one file, plus the edge buckets' files from the file index). Not measured; by the bucket figures it costs a few bytes a torrent more than (b). The `-path:` clause needs the file index whatever the terms.

## For the runner
- **The gate's mix** still counts `file:(ext:mkv)`, `file:(ext:srt)`, `file:(ext:flac,mp3)` and `file:(ext:jpg)`, which no longer join anything. Whether §3's "file-level query joined to torrents" row should still count them (they lower the p50 a lot and the p95 little) is the runner's call; the bench is unchanged.

## Shared files outside the brief's owns
- **`crates/search/src/compile.rs`**: option (a), with the runner's go.
- **`crates/search/src/query.rs`** and **`docs/query-language.md`**: the docs that describe `file:` (the runner asked for them).
- **`crates/search/src/rebuild.rs` and `crates/search/tests/index.rs`**: one test each probed a missing or emptied file index with a lone `file:(ext:…)`, which the torrent index now answers; each now probes with a two-atom clause, and `rebuild.rs` also asserts the lone clause finds its torrents.
