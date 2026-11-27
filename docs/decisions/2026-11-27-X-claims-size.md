# X-claims-size decisions

Slice X-claims-size (runner brief, 2026-11-27T12:51Z; runner's choice of levers, 2026-11-27T13:11Z): identification answers' claims blobs made smaller with a trained zstd dictionary. Spec: architecture §3, `docs/decisions/2026-11-27-X-enr11-size-measure.md`. No budget, schema or `ci.yml` change.

## What was measured (step 2)
At CI's size, 50,000 torrents, seed 1. Each lever was applied to a copy of `size-report`'s store and vacuumed; the claims tables fill in id order, so their deltas carry over to the unvacuumed gate figure.

| Lever, store bytes per torrent | 60 % | 100 % | Needs |
|---|---:|---:|---|
| main | 1114.4 | 1224.5 | |
| (a) dictionary 16 / 32 / 64 KiB | −43.9 / −51.4 / −57.7 | | no migration |
| (a) dictionary 112 KiB, magicless frame | −64.6 | −108.1 | no migration |
| (b) 8 alternative titles | about −22 without a dictionary, −1 with | | |
| (c1) drop `torrent_claims_source` | −6.6 | −11.1 | a schema slot |
| (c2) `torrent_claims` `WITHOUT ROWID` | −9.2 | −16.4 | a table rebuild |

- **The runner chose (a) alone** (§2.26). (c1) and (c2) save 7–14 B for a schema slot, and (c1) slows `mark_claims_unenriched`, a long transaction already flagged in #1138's review. (c2) suits rows under about 200 B, and claims rows can reach 1.18 MB.
- **(b) isn't available:** the drawer's API shows every stored title, up to 32 (`identification()` in `crates/api/src/torrents.rs`), and the content-safety check judges each one (`details_alone` in `crates/pipeline/src/unverified.rs`). With a dictionary it would save under 2 B anyway.
- **No lever brings the 60 % share within 976 B.** The store already holds 949.3 B at 0 %, which leaves about 49 B per answer, page and index overhead included. A dictionary-compressed answer is still about 124 B.

## The format: codec 2 (`crates/store/src/claims.rs`)
- **A codec, not a layout:** `version · 2 · dict_id:uvar · the zstd frame without its 4-byte magic`. The version byte stays first, so the SQL that filters on it (`served_identifications`, the identify sweep) is unchanged. The magic is put back before decompressing, so no zstd experimental feature is needed. The frame carries no dictionary id and no checksum; the blob's own id is the only one.
- **Old blobs read as before:** codecs 0 and 1 are untouched, and every blob `main` writes decodes (a test writes `main`'s encoding into a store, opens it, trains, rewrites and reads both).
- **Fails closed:** `decode` knows no dictionary and refuses every codec 2 blob with `ClaimsError::Dictionary(id)`. `decode_with` asks its `Inflate` for that id and never tries another. The store's reads treat such a row as unreadable, as they treat a corrupt one: left out of `torrent_claims`, never served, logged for reindexing when dropped (the safe side), and passed over by the sweeps.
- **Written only when smaller:** a blob the dictionary doesn't shrink is stored raw (codec 0), and never shorter than the tables' 7-byte minimum, so the schema's `CHECK` holds.

## The dictionary (`crates/store/src/claims_dict.rs`)
- **Stored as the file trees' are:** a `zstd_dicts` row of kind `claims`, and the settings row `claims_dict` naming the active one. Both are written in one short transaction, so the setting never names a dictionary that isn't there. A store whose setting does is refused at open, as for the file trees.
- **Trained once, by the writer:** at each flush, while no dictionary is active (an in-memory check, so it costs nothing afterwards), the writer asks the store whether at least `MIN_ROWS` (1,000) answers are stored. Answers are version 3 rows of an identify source, one with a terms record. When they are, the store reads at most `MAX_SAMPLES` (4,000) of them and `MAX_SAMPLE_BYTES` (4 MiB) in all, through the `(source_id, …)` indexes. It skips any over 16 KiB, trains a dictionary of up to `DICT_SIZE` (112 KiB, the file trees' size) outside any transaction, and installs it. A second connection installing first wins; the other stores nothing. Once the setting exists nothing trains again, restarts included. There is no retraining; one would take a new id.
- **One Info event,** `pipeline.claims_dict_trained`, with the sample count and the dictionary's size (log-catalogue owns granted by the runner, 2026-11-27T13:15Z).
- **Used for answers only:** claims with a details section are written with it; feed claims and unverified items keep codecs 0 and 1.
- **Readers find a new dictionary themselves:** a connection opened before training loads the dictionary from `zstd_dicts` the first time a blob names it, then keeps its decompressor.
- **Never deleted or replaced in place.** Nothing in this slice deletes a `zstd_dicts` row.

## No background rewrite
- **Rows written before the dictionary keep their codec 1 blobs** (runner, 2026-11-27T13:11Z). Training happens at the 1,000th answer, so at most about 1,000 rows (plus any stored while a check was pending) stay larger: about 112 B each, about 0.1 MB in all, whatever the store's size.
- **They shrink anyway when refreshed:** ENR-10 refreshes an answer before its retention runs out (150 of 180 days by default), and the refresh is written with the dictionary.
- A rewrite pass would cost a long write path and its own tests for that.

## Result
`size-report --torrents 50000 --identified 0,60,100` with this slice:

| Part, bytes per torrent | 0 % | 60 % (54.7 % of the corpus) | 100 % (90.8 %) | Budget |
|---|---:|---:|---:|---:|
| Canonical store | 949.3 | 1055.5 (main 1114.4) | 1123.0 (main 1224.5) | 976 |
| Claims blob mean | — | 128 B (main 237) | 126 B (main 238) | |
| Torrent index | 550.5 | 613.1 | 642.6 | 560 |

- **The dictionary is trained from the first 1,000 answers.** 26,353 of 27,353 blobs use it at 124 B each; the 1,000 written before it average 236 B. The measured saving uses the dictionary as production builds it: trained at the threshold, with no rewrite.
- **The saving may be smaller on real stores.** Synthetic titles draw on a fixed 600-word vocabulary per script, so a real catalogue's answers share less than the corpus's do.
- **Costs:**
  - Writing an answer is no slower: 31.5 µs per blob with the dictionary against 38.5 µs, at zstd 19.
  - Reading is faster, since the decompressor is kept: 0.7 µs against 16 µs.
  - Storing 27,353 answers, training included, takes 5.5 s against 5.1 s.
  - The 112 KiB dictionary is 2.3 B per torrent at 50,000 torrents and nothing at 10M.
- **The size gate stays at 0 %** (`ci.yml` untouched): the 60 % share is still 79.5 B over 976. Any budget change is the runner's, decided later.
- **`size-report` checks for the dictionary every 50 answers,** about what one plugin stores between two of the writer's flushes at a full day's budget. It prints the dictionary's size and how many rows use it.

## Also in this slice
- **`synth::answer` goes through `Identification::into_claims`** (the plugin API's own conversion and clamps), and the terms record comes from `Terms { redistribute: true, .. }.record()` (#1136's review nits). `lodestar-sources` moves from the bench's dev-dependencies to its dependencies for that; the pipeline already depends on it, so nothing new is compiled.
- **`docs/decisions/2026-11-27-X-enr11-size-measure.md`:** the "default share" heading now says "realistic share" (brief).

## Found, for later (runner, 2026-11-27T13:11Z)
- **Production never trains the file-tree dictionary.** Only tests and `size-report` call `train_file_dict`, so the §3 file-tree figure (346 B blobs) assumes a dictionary real stores don't have.
- **`size-report`'s train-then-recompress rewrite leaves about 60 B per torrent of partly filled pages** (VACUUM takes the 60 % store from 1113.8 to 1047.6 B: 46 B in `torrent_files`, 12.5 B in `torrents`). The slack is there at 0 % too, so the 0 % figure carries it.
