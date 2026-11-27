# X-claims-size decisions (#1141)

Slice X-claims-size (runner brief, 2026-11-27T12:51Z): try to shrink identification answers' share of the store before any §3 budget moves (§2.26). Spec: architecture §3, `docs/decisions/2026-11-27-X-enr11-size-measure.md`. The runner's decision (2026-11-27T15:09Z) is that no lever is built. This PR keeps the measurements and the `size-report` fixes. There is no budget, schema or `ci.yml` change, and the gate stays at 0 %.

## What was measured
- **Method:** CI's size, 50,000 torrents, seed 1, at the 0, 60 and 100 % identified shares.
  - The levers (b) and (c) were applied to a copy of `size-report`'s store, which was then vacuumed. The claims tables fill in id order, so their deltas carry over to the unvacuumed gate figure.
  - The dictionary rows are full `size-report` runs, with the dictionary trained in the run as the writer would train it: at the 1,000th answer, without rewriting older rows.
  - At 0 % no lever changes anything, because there are no claims rows.

| Store, bytes per torrent | 60 % | 100 % | Would need |
|---|---:|---:|---|
| main | 1114.4 | 1224.5 | |
| (a) zstd dictionary trained on the stored answers, 112 KiB | 1055.5 (−58.9) | 1123.0 (−101.5) | a new blob codec, a trainer and a migration |
| (a) the same, trained on templates with every source value replaced | 1107.2 (−7.2) | 1210.4 (−14.1) | the same |
| (b) 8 alternative titles instead of 32 | about −22 | | |
| (c1) drop `torrent_claims_source` | −6.6 | −11.1 | a migration |
| (c2) `torrent_claims` `WITHOUT ROWID` | −9.2 | −16.4 | a table rebuild |
| (c1) and (c2) | −14.1 | −24.6 | a table rebuild |

Dictionary sizes probed per blob, before the runs: 16, 32 and 64 KiB saved 44, 51 and 58 B per torrent at 60 %. 112 KiB with the zstd magic left out saved 65 B.

## Why each lever was dropped
- **(a) A dictionary can't keep what it learned from.**
  - The runner's security review of #1141 found answers' titles verbatim in a dictionary trained on them: 743 of 1,000 test titles were still there after every claim was dropped.
  - A dictionary is kept for good, in the store and in every snapshot and backup. That would break the ENR terms retention (TMDB's 180 days, `docs/decisions/2026-11-20-X-snapshot-claims-retention.md`), the source purge (§18.5) and content-safety removals.
  - **Trained on templates instead,** the dictionary learns only the layout: field presence, counts, lengths, the id schemes and their fixed letters, and primary language subtags. Every title, genre and id character, and every number, is replaced by a same-class placeholder.
  - That saves 7.2 B per torrent at 60 %, below the runner's floor of about 25 B. Nearly all of the saving had been title vocabulary shared across answers, which is the source content the retention and purge rules forbid a dictionary to keep.
  - A new codec, a trainer and a schema migration (needed so an older build refuses the store rather than deleting rows it can't read) aren't worth 7 B.
- **(b) Fewer stored titles: the titles are shown and judged.**
  - The drawer's API shows every stored title, up to 32 (`identification()` in `crates/api/src/torrents.rs`).
  - The content-safety check judges each one (`details_alone` in `crates/pipeline/src/unverified.rs`).
- **(c1) and (c2) save 7–14 B for a migration.**
  - (c1) would also slow `mark_claims_unenriched`, a long transaction #1138's review flagged. Without `torrent_claims_source`, its subquery reads each row by its index entry instead of from a covering index.
  - (c2) suits rows under about 200 B, and claims rows can reach 1.18 MB.
  - Every other claims query plans through the primary key or `torrent_claims_stored` either way (checked with `EXPLAIN QUERY PLAN`).

## The budget
- **The store stays over at the realistic share:** about 1114 B per torrent at 60 % against 976, and 1224 B at 100 %. At 0 % it holds 949.3 B, so the claims tables could take only about 27 B per torrent, about 49 B per answer with pages and indexes. An answer's blob alone averages 237 B.
- **The budget decision goes to the runner.** CI's `size-report --gate` stays at the 0 % share, and `ci.yml` is untouched.

## Also found, for later
- **About 60 B per torrent of the 0 % figure is slack from `size-report` itself.**
  - Its train-then-`recompress_files` step rewrites every file-tree blob in place and leaves partly filled pages. VACUUM takes the 60 % store from 1113.8 to 1047.6 B per torrent: 46 B in `torrent_files`, 12.5 B in `torrents`.
  - The slack is there at 0 % too.
- **Production never trains the file-tree dictionary.** Only tests and the bench (`size-report` and the 10M dataset builder, `bench/src/dataset.rs`) call `train_file_dict`. So the §3 file-tree figure (346 B blobs) assumes a dictionary real stores don't have.
- The runner noted both for follow-ups (2026-11-27T13:11Z).

## What this PR changes
- **`synth::answer` goes through `Identification::into_claims`** (#1136's review). The bench's answers now take the plugin API's own conversion and clamps, then the store's model as `store_claims` maps a details-only find.
- **The terms record comes from `Terms { redistribute: true, .. }.record()`** (#1136's review), not hand-written bytes. It's the strict default otherwise, so it also sets the adult-marker flag, as a real plugin's record does.
- **`lodestar-sources` moves to the bench's `[dependencies]`** for those two. The pipeline already depends on it, so nothing new is compiled.
- **`docs/decisions/2026-11-27-X-enr11-size-measure.md`:** its "default share" heading now says "realistic share" (brief).
