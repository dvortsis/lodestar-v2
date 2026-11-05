# X-search-files-5 decisions

§3's "File-level query joined to torrents < 150 ms" row at 10M, continued from X-search-files-4 (#468). This slice answers `file:(size:…)` and `file:(ext:… size:…)` from new torrent-level terms in a v7 search schema, and carries B13's title-identity key in the same bump. @@GATE@@

## `SCHEMA_VERSION` 7, a second M7 bump (runner's ruling (A), 2026-11-05, flagged to the owner under §2.19)
- **Why a bump:** the new terms are new indexed fields, and Tantivy can't add a field to an existing index. Build-plan §1.3 allowed one bump per milestone, and M7's was SB-6 (v6). The runner allowed a second: the rule exists so a released user rebuilds once per release. v0.5.1 ships search v5 and v6 was never released, so if v7 lands before the v0.5.2 cut, a v0.5.1 user rebuilds once, v5 → v7. Only nightly and dev installs rebuild twice. Build-plan §1.3 and the shared-files row now say so.
- **One pass from any older version.** The rebuild builds this build's schema straight from the store while the newest older index serves; there is no version-by-version path. `tests/rebuild.rs` `a_v5_index_serves_while_v7_is_built_resumed_and_swapped_in` asserts that only `v5-t1` and `v7-t1.building` exist while it builds (no v6 on the way), that v5 answers `file:(size:>1MB)` from its file index meanwhile, and that the v7 fields are Ready after the swap.
- **Rebuild cost for a user:** the blue/green rebuild indexes the whole store in the background while the old index serves. Measured here at 10M (4 cores): @@REBUILD@@. It needs about twice the index's disk while it runs (the old index stays until the swap): at 10M the old v6 index was 11.2 GB.

## The bucket terms (brief items 1 and 2)
- **Two torrent-index fields, both indexed only (no fast column, no norms):**
  - `file_bucket` (u64, several a torrent): the distinct [`size_bucket`]s of the torrent's covered files.
  - `file_ext_bucket` (keyword, several): the same files' distinct `{ext}:{bucket}` pairs. Extensions are ASCII letters and digits, so the colon can't be part of one.
  - Both come from `files::covered_buckets`, which applies exactly `file_docs`' smart-coverage rule (test `covered_buckets_are_the_file_docs`): a torrent has a term exactly when one of its file documents is in that bucket, with that extension. So results never change by coverage, unlike #468's lone extensions, which were accepted as gaining.
- **How a clause runs (`compile::bucket_clause`):** a `file:(…)` clause that is one size range, alone or beside one extension atom (`file:(size:>1GB)`, `file:(ext:mkv,mp4 size:>1GB)`, `file:((ext:a | ext:b) size:…)`), is exact on these terms because a file has one size, so one bucket, and one extension.
  - **Buckets wholly inside the range** are a union of the torrent's terms for them.
  - **The edge buckets** (at most two, the ones the range only partly covers) still go through the file index, restricted to those buckets and the extensions: `compile::bucket_edges` gives the file pass that clause instead of the whole one, and its torrents join the union by id. A range on whole buckets (`size:>=1GiB`) reads no file at all.
  - **Ranking:** the clause is unscored, as #468 made a lone `file:(ext:…)`: nothing beside another clause that drives, 1.0 for the first such clause when none does, 1.0 nested.
  - Other shapes (two ranges, words, negations inside the clause) stay on the file index as before.
- **Population (SB-6's contract):** both fields, and `title_identity`, are `schema::V7_FIELDS`, at generation 1 in `population::GENERATIONS`, filled by the rebuild. The search uses the bucket terms only when both are `Availability::Ready` (`SearchIndex::bucket_terms_ready`); otherwise (an older index serving during the rebuild, or a backfill of them still running) the whole clause runs on the file index, so it never returns empty.
- **Proof the results are the same (brief item 5):** `tests/files.rs` `bucket_terms_answer_as_the_file_index_does` runs over 1,000 query shapes against the same query forced onto the file index (a never-matching exclusion inside each clause). Same torrents, same order, same scores, for bare ranges and with one or several extensions or `ext:a | ext:b`; at, just below and just above every bucket boundary; open at either end; single buckets, parts of one, wide spans; beside a word, a top-level range or another file clause; in an alternative; and negated (the accepted rule stands); over a large pack whose small images coverage leaves out and a torrent whose only video is hidden. It fails if the edge buckets are dropped. `relevance --gate` stays at 0.9186.

## B13's title-identity key (the runner's rider)
- **`title_identity`** (u64, indexed only, one a torrent with a parsed title): a stable FNV-1a hash of exactly what B13's sibling check compares (`api::compare::identity`): the effective title's words as search's tokenizer folds them, and the sorted, distinct `year`, `season`, `episode` and `absolute` spans, an absent set a value of its own. Test `title_identity_follows_b13s_rule` pins the folding, the separations and the hash's value. A collision only over-matches; the stored records still decide. Nothing reads it yet: B13b (the runner's follow-up) uses it for "other versions of this".

## P2 and the `rest` skip (brief items 3 and 4)
- **A size check builds only the scorer its plan runs** (`SizeCheckedWeight::plan`). When the other clauses are plain terms (`ext:mkv,mp4`), their files are counted from the term dictionary, so a segment that runs the plain conjunction opens no postings for the check it didn't take (#468's review P2), and each piece builds one scorer instead of two. Test `the_plain_conjunction_builds_no_scorer_for_the_check`.

## Measured at 10M (brief item 6)
@@NUMBERS@@

## Shared files outside the brief's owns (accepted by the runner)
- `crates/search/src/index.rs`: the new fields in `resolve_fields`, `bucket_terms_ready`, and the file pass calling `compile::search_file_clauses`.
- `crates/search/src/population.rs`: the three entries and the tracked-fields test.
- `crates/search/src/facets/tests.rs`: the new fields in a `TorrentDoc` literal.
- `crates/search/tests/rebuild.rs`: the v5 test now goes to v7.
- `crates/search/tests/index.rs`, `crates/search/src/lease/tests.rs`, `crates/search/src/percolate.rs`, `crates/search/src/suggest/mod.rs`: their tests forced a file-index scan with a bare range (`file:(size:>0)`, `file:(ext:flac size:>1)`), which the bucket terms now answer without one; each adds a never-matching exclusion, which keeps the clause on the file index with the same matches.
- `docs/build-plan.md`: §1.3 and the shared-files row (the runner's condition 3).
