# X-search-files-5 decisions

§3's "File-level query joined to torrents < 150 ms" row at 10M, continued from X-search-files-4 (#468). This slice answers `file:(size:…)` and `file:(ext:… size:…)` from new torrent-level terms in a v7 search schema, and carries B13's title-identity key in the same bump. **The gate is still missed at 10M: cold p95 228.4 ms, warm p95 87.0 ms (gates 150 / 30), from 314.5 / 285.0 ms on `main` on the same index and machine; cold p50 fell from 167.2 to 22.6 ms.** Three clauses that the new terms don't answer set the p95 now (see "What's left").

## `SCHEMA_VERSION` 7, a second M7 bump (runner's ruling (A), 2026-11-05, flagged to the owner under §2.19)
- **Why a bump:** the new terms are new indexed fields, and Tantivy can't add a field to an existing index. Build-plan §1.3 allowed one bump per milestone, and M7's was SB-6 (v6). The runner allowed a second: the rule exists so a released user rebuilds once per release. v0.5.1 ships search v5 and v6 was never released, so if v7 lands before the v0.5.2 cut, a v0.5.1 user rebuilds once, v5 → v7. Only nightly and dev installs rebuild twice. Build-plan §1.3 and the shared-files row now say so.
- **One pass from any older version.** The rebuild builds this build's schema straight from the store while the newest older index serves; there is no version-by-version path. `tests/rebuild.rs` `a_v5_index_serves_while_v7_is_built_resumed_and_swapped_in` asserts that only `v5-t1` and `v7-t1.building` exist while it builds (no v6 on the way), that v5 answers `file:(size:>1MB)` from its file index meanwhile, and that the v7 fields are Ready after the swap.
- **Rebuild cost for a user:** the blue/green rebuild indexes the whole store in the background while the old index serves. Measured here at 10M (4 cores): about 1 h 55 min to index 10M torrents into both indexes (1,439 torrents a second; the store itself is read, not rebuilt). It needs about twice the index's disk while it runs (the old index stays until the swap): at 10M the old v6 index was 11.2 GB.

## The bucket terms (brief items 1 and 2)
- **Two torrent-index fields, both indexed only (no fast column, no norms):**
  - `file_bucket` (u64, several a torrent): the distinct [`size_bucket`]s of the torrent's covered files.
  - `file_ext_bucket` (keyword, several): the same files' distinct `{ext}:{bucket}` pairs. Extensions are ASCII letters and digits, so the colon can't be part of one.
  - Both come from `files::covered_buckets`, which applies exactly `file_docs`' smart-coverage rule (test `covered_buckets_are_the_file_docs`): a torrent has a term exactly when one of its file documents is in that bucket, with that extension. So results never change by coverage, unlike #468's lone extensions, which were accepted as gaining.
- **Codes, not buckets (`files::size_code`):** each file's term is its bucket twice, plus one when its size is above the bucket's floor. A strict lower bound on a binary unit (`size:>1GB`, where 1 GB is 2^30) falls exactly on a floor, so without the split its whole bucket was an edge, checked file by file in the file index and joined to torrents: the first v7 run measured `size:>1GB` at 163 ms and `ext:mkv size:>1GB` at 280 ms that way, and a whole-bucket range (`size:>=1GB`) at 25 ms. With the split, a bound just above a floor or on one (`size:<=1GB`) is whole codes. Sizes exactly on a floor are rare, so the codes cost almost no more terms than buckets would.
- **How a clause runs (`compile::bucket_clause`):** a `file:(…)` clause that is one size range, alone or beside one extension atom (`file:(size:>1GB)`, `file:(ext:mkv,mp4 size:>1GB)`, `file:((ext:a | ext:b) size:…)`), is exact on these terms because a file has one size, so one bucket, and one extension.
  - **Codes wholly inside the range** are a union of the torrent's terms for them (`files::code_plan`).
  - **What's left of the edge buckets** (at most two, partly covered, and not a whole code) still goes through the file index, restricted to those buckets and the extensions: `compile::bucket_edges` gives the file pass that clause instead of the whole one, and its torrents join the union by id. A range on whole codes (`size:>=1GiB`, `size:>1GiB`) reads no file at all.
  - **Ranking:** the clause is unscored, as #468 made a lone `file:(ext:…)`: nothing beside another clause that drives, 1.0 for the first such clause when none does, 1.0 nested.
  - Other shapes (two ranges, words, negations inside the clause) stay on the file index as before.
- **Population (SB-6's contract):** both fields, and `title_identity`, are `schema::V7_FIELDS`, at generation 1 in `population::GENERATIONS`, filled by the rebuild. The search uses the bucket terms only when both are `Availability::Ready` (`SearchIndex::bucket_terms_ready`); otherwise (an older index serving during the rebuild, or a backfill of them still running) the whole clause runs on the file index, so it never returns empty.
- **Proof the results are the same (brief item 5):** `tests/files.rs` `bucket_terms_answer_as_the_file_index_does` runs over 1,000 query shapes against the same query forced onto the file index (a never-matching exclusion inside each clause). Same torrents, same order, same scores, for bare ranges and with one or several extensions or `ext:a | ext:b`; at, just below and just above every bucket boundary; open at either end; single buckets, parts of one, wide spans; beside a word, a top-level range or another file clause; in an alternative; and negated (the accepted rule stands); over a large pack whose small images coverage leaves out and a torrent whose only video is hidden. It fails if the edge buckets are dropped. `relevance --gate` stays at 0.9186.

## B13's title-identity key (the runner's rider)
- **`title_identity`** (u64, indexed only, one a torrent with a parsed title): a stable FNV-1a hash of exactly what B13's sibling check compares (`api::compare::identity`): the effective title's words as search's tokenizer folds them, and the sorted, distinct `year`, `season`, `episode` and `absolute` spans, an absent set a value of its own. Test `title_identity_follows_b13s_rule` pins the folding, the separations and the hash's value. A collision only over-matches; the stored records still decide. Nothing reads it yet: B13b (the runner's follow-up) uses it for "other versions of this".

## P2 and the `rest` skip (brief items 3 and 4)
- **A size check builds only the scorer its plan runs** (`SizeCheckedWeight::plan`). When the other clauses are plain terms (`ext:mkv,mp4`), their files are counted from the term dictionary, so a segment that runs the plain conjunction opens no postings for the check it didn't take (#468's review P2), and each piece builds one scorer instead of two. Test `the_plain_conjunction_builds_no_scorer_for_the_check`.

## Measured at 10M (brief item 6)
- **Method:** as #468. A 10M corpus built in this container with `main` at dfc91d2 (4 cores, 15 GiB; 8 torrent-index segments, about 300M file documents), benched with `search-bench --torrents 10000000 --only "broad file"`; then the v6 index deleted for disk and v7 built from the same store and benched the same way. Cold means the file-set cache emptied before each query; the OS page cache warm. **This machine ran slower than #468's**: `main` here (which includes #468) measured cold p95 314.5 ms, where #468 measured the same code at 199.5 ms, and an unchanged clause (`file:(ext:mkv)`) moved between 16 and 28 ms from run to run, so compare within the table, not across PRs.
- **The mix (ms):**

  | | cold p50 | cold p95 | warm p50 | warm p95 | cold, 4 at once p95 |
  |---|---:|---:|---:|---:|---:|
  | `main` (v6) | 167.23 | 314.47 | 156.95 | 284.99 | 1620.92 |
  | v7 | 22.63 | 228.35 | 17.67 | 87.04 | 809.83 |

  The warm cache now holds every set the mix needs (100% of segment lookups hit, 6.3 MiB): the bucket clauses' sets are only their edges, so they're small.
- **Per clause, cold, alone (ms, best / median of three):**

  | clause | `main` (v6) | v7 |
  |---|---:|---:|
  | `file:mkv` | 198.09 / 201.89 | 166.37 / 174.99 |
  | `file:(ext:mkv)` | 17.88 / 19.87 | 15.85 / 16.26 |
  | `file:(ext:srt)` | 3.40 / 3.77 | 3.48 / 4.83 |
  | `file:(ext:flac,mp3)` | 7.21 / 7.61 | 6.40 / 7.69 |
  | `file:(ext:jpg)` | 8.08 / 8.43 | 5.25 / 5.28 |
  | `file:(size:>100MB)` | 270.01 / 275.06 | 165.34 / 220.08 |
  | `file:(ext:mkv,mp4,avi -path:sample)` | 264.64 / 271.52 | 273.45 / 287.88 |
  | `file:(size:>1GB)` | 204.22 / 209.35 | 24.60 / 25.71 |
  | `file:(ext:mkv size:>1GB)` | 288.77 / 290.29 | 24.42 / 28.39 |

  Torrent counts are identical in every row. Rows whose path didn't change (`file:mkv`, the lone extensions, `-path:sample`) differ by this machine's run-to-run noise.
- **Index bytes per torrent (§2.26, flagged to the owner):** the torrent index went from 3,642,023,947 to 3,909,739,928 bytes, 364.2 → 391.0 B a torrent, against the §3 budget of 512 (the file index is unchanged, 757 B). Per field, from Tantivy's space usage on the v7 index:

  | field | bytes | B per torrent |
  |---|---:|---:|
  | `file_bucket` | 39,782,670 | 3.98 |
  | `file_ext_bucket` | 67,209,433 | 6.72 |
  | `title_identity` (rider) | 159,602,755 | 15.96 |

  The bucket terms are 10.7 B a torrent together; the first v7 build, with buckets instead of codes, measured within a byte of the same. `title_identity` is at the runner's ~16 B ceiling: one term a torrent, and the synthetic corpus's titles are mostly distinct, so its term dictionary is most of it.

## What's left (the gate)
- **`file:(ext:mkv,mp4,avi -path:sample)`** (273 ms): a negation inside one file needs the file index, whatever torrent terms exist (runner, #468).
- **`file:(size:>100MB)`** (165–220 ms): 100 MB is 100 × 2^20, not a bucket floor, so its edge bucket (96–128 MiB) is still checked file by file and joined. Finer codes would close it at more bytes a torrent; or the gate's mix could use a binary-unit edge, as most of its sizes are.
- **`file:mkv`** (166 ms): a word on the file index, which #468 left there (no per-torrent marker of truncated paths).
- Each is a ninth of the mix, so p95 stays above 150 ms until all three are under it. The warm gate (30 ms) is missed for the same three.

## Shared files outside the brief's owns (accepted by the runner)
- `crates/search/src/index.rs`: the new fields in `resolve_fields`, `bucket_terms_ready`, and the file pass calling `compile::search_file_clauses`.
- `crates/search/src/population.rs`: the three entries and the tracked-fields test.
- `crates/search/src/facets/tests.rs`: the new fields in a `TorrentDoc` literal.
- `crates/search/tests/rebuild.rs`: the v5 test now goes to v7.
- `crates/search/tests/index.rs`, `crates/search/src/lease/tests.rs`, `crates/search/src/percolate.rs`, `crates/search/src/suggest/mod.rs`: their tests forced a file-index scan with a bare range (`file:(size:>0)`, `file:(ext:flac size:>1)`), which the bucket terms now answer without one; each adds a never-matching exclusion, which keeps the clause on the file index with the same matches.
- `docs/build-plan.md`: §1.3 and the shared-files row (the runner's condition 3).
