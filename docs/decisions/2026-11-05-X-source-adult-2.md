# X-source-adult-2 decisions (always-adult mask on corrections and imports)

Slice X-source-adult-2 (runner brief, 2026-11-05T10:20Z). It closes the first two of the gaps that `2026-11-05-X-source-adult.md` says must close before Sukebei is registered: **`crates/api` corrections** and **imports**. This file supersedes those two bullets. That file is left as it is, as `docs/decisions/README.md` asks. The other two gaps there are still open: stopped and purged sources (the runner's call), and the plugin's registration.

- **`/classify/correct` applies the same mask as the pipeline.** The handler (`FiltersApi::correct`, `crates/api/src/filters.rs`) classifies with `lodestar_filter::reclassify::source_adult(own, row.sources, adult_sources)`. That's the rule `source_adult::correction` applies in the backfill, moved into `lodestar-filter` so both crates share one copy. For a torrent whose `sources` holds an always-adult bit:
  - clearing a correction, setting only Not junk, or setting Is adult all keep it Adult;
  - your explicit Not adult makes it detection's type (the accepted rule from X-source-adult);
  - clearing that again makes it Adult once more.

  Only the classification uses the mask. The stored correction is still exactly what you sent, so the classification never pretends you said Is adult.
- **How the API learns the mask.** `FiltersApi` gains `adult_sources: u64` (the bitset form of `AdultSources`), set in `crates/lodestar/src/services.rs` from a new `Pipeline::adult_sources()` getter. It's empty while no always-adult source is registered, so nothing changes today. The runner approved both hunks outside the brief's list.
- **Imports read `unverified_items` in the commit's own transaction.** `commit_import_batch` (`crates/store/src/import.rs`) uses the connection's `AdultMark` from #479. For each torrent record, new or merged into a stored row, it reads the finders of any unverified item under the record's swarm hashes. When one is an always-adult source:
  - the row gains that source's bit (only the always-adult bits, never the other finders');
  - `mark_adult_found` marks it for the backfill, unless it's stored Adult already or you said Not adult.

  So the derived signal is stored with the row in one transaction, and no crash can leave it un-adult with no retry. With an empty mark nothing is read.
- **The driver judges a claimed record as adult before the commit.** `driver::run` (`crates/sources-importers`) asks `Store::adult_finders` for each record's hashes:
  - A claimed record is judged by `lodestar_filter::reclassify::judge_source_adult`: detection's reading and the adult reading, the stricter verdict standing. That's the pipeline's `source_adult::judge`, moved to `lodestar-filter` with `stricter`, and the pipeline now delegates to it.
  - The record is stored Adult (nearest adult kind, as an Is adult correction makes it), so the commit writes it adult and the mark isn't needed.
  - A hash on the floor's or a dropping list is still checked first.
  - A Not adult classification rule never un-adults a claimed record.
  - A read error counts as adult (stricter), as in `Writer::source_adult`.
  - The mark in the commit covers a claim that lands after this read: the record is stored as the driver judged it, with the bit and the mark, and the backfill classifies it Adult.
- **The CLI sets the mark** (`crates/lodestar/src/cli/import.rs`). `lodestar import` runs without the pipeline, so it sets the store's `AdultMark` from the sources the store has registered whose plugin's compiled-in manifest says `always_adult` (Sukebei), whatever their state. That's stricter than the pipeline's mask, which counts only plugins registered at start: a stopped source's claims still mark what it found. The CLI is the only caller of `driver::run`. `AdultSources::store_mark` builds the mark.
- **Other paths checked (brief step 3).** None writes a stored torrent's classification or `sources` without the mask:
  - **Classification rules** (added or deleted): their matches are queued for the background enrichment, which applies the mask.
  - **The writer, promotion, claims and the backfill:** masked by X-source-adult and #479.
  - **Rollback** (`lodestar rollback`, a pre-migration snapshot): it restores the whole store, `sources` included, so each row comes back with the classification and bits it had together.
  - **Search rebuild and exports:** read the stored class only.
  - **Test-only:** the `set_enrichments` calls in `alerts.rs`, `compare.rs` and `search_live.rs`.
  - **Not changed: import candidates** (a record with no info dict). They're judged by name without the adult reading. They aren't stored torrents and aren't served. When one is fetched, `Writer::source_adult` finds the unverified item under its hash and stores it adult.
- **Tests.**
  - `crates/api/tests/source_adult.rs`: clear, Not junk only, Is adult, Not adult (with and without Not junk) and clear again on a source-adult torrent, through `FiltersApi::correct`. A plain source's torrent isn't affected. The test fails with an empty mask.
  - `crates/sources-importers/src/driver/tests.rs`:
    - a Magnetico dump holding a hash a Sukebei item claims is stored Adult with the bit, and the backfill keeps it adult;
    - a claimed name only the adult reading's CS-5a meets is quarantined, and the same name unclaimed is stored;
    - a claim that another connection adds just before the commit leaves the row with the bit and the mark, and the backfill then makes it Adult (the crash between the two steps).

    Each half of the fix was switched off in turn to check that these tests fail without it. `lodestar-sources-importers` gains `lodestar-pipeline` as a dev-dependency for the backfill.
  - `crates/store/src/import.rs`: a crash inside the transaction leaves nothing; the commit adds only the always-adult bit and marks the row; a merge into a row lacking the bit gains it and is marked; a row stored Adult gains the bit but isn't marked.
- **Paths outside the brief's list:**
  - `crates/filter/src/reclassify.rs`: the shared `source_adult`, `judge_source_adult` and `stricter`;
  - `crates/pipeline/src/source_adult.rs`: delegates to them, plus `store_mark`;
  - `crates/pipeline/src/lib.rs`: the getter;
  - `crates/lodestar/src/services.rs`: one line;
  - `crates/lodestar/src/cli/import.rs`: sets the mark;
  - `crates/sources-importers/Cargo.toml`: the dev-dependency;
  - `crates/api/tests/{filters,filters_packs,golden,pack_overrides,standin_undo}.rs`: one `adult_sources: 0` line per `FiltersApi` literal.

  `crates/pipeline/src/writer.rs` is untouched.
