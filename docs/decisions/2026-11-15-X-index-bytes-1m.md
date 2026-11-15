# X-index-bytes-1m decisions

X-test-vm's 1M Magnetico import reported the torrent index at 652 B/torrent, against `INDEX_BUDGET` = 512 B (`bench/src/bin/size-report.rs`, §3/§10.1). The budget is unchanged.

## What the measurement shows
- **`size-report`'s own corpus is within budget at 1M.** Seed 1, main@c080f5fd, after the final commit, merge and GC:

  | Torrents | Torrent index (B/torrent) | Segments | Deleted docs | Stale bytes |
  |---|---|---|---|---|
  | 50,000 | 496 | 3 | 0 | 0 |
  | 1,000,000 | 446 | 4 | 0 | 0 |

  The figure falls with scale, because term dictionaries and postings amortise. CI gates at 50k (`--torrents 50000 --gate`), the stricter of the two.
- **Where the bytes go at 1M (B/torrent):**
  - postings: `text` 94.5, `path` 91.4;
  - terms: `hash` 38.1, `payload_sig` 17.0, `group_minhash` 15.4, `title_identity` 15.3;
  - docstore: 36.6;
  - everything else: under 15 each.
- **A recursive `du` isn't the budget figure.** At 1M, `du` of the same folder gives 1,205 B/torrent, because it adds the file index (759 B).
  - The pilot script (`docs/operations/imports/pilots/smoke/smoke.py`) subtracts the `files` subfolders, but otherwise counts everything under `data/index`: another `v*-t*` folder left by a rebuild, segment files a merge replaced that garbage collection hasn't removed yet, and deleted documents in live segments.
  - The same script measured the Magnetico pilots at 360 B (10k) and 306 B (100k).
- **The 652 B figure isn't reproduced here.** The 1M Magnetico corpus and the X-test-vm host's index aren't available to this slice.

## How to measure an index (new tooling, dev only)
- **`lodestar_search::DiskStats`** reads an index folder read-only (a reader, no writer, no lock). It reports:
  - live segments, docs and deleted docs;
  - the bytes the live segments use, by Tantivy's accounting;
  - top-level and recursive bytes on disk;
  - stale segment files: files named for a segment that isn't live.

  `space_by_field_at` gives the per-part table for any folder.
- **`index-stats DIR [--torrents N]`** (bench bin) prints both for a data folder's `index` or a Tantivy folder. Stop the server first, or read a copy.
- **The budget figure is "live segments" per torrent.** Stale bytes and deleted docs are temporary, and the file index has its own budget.
- **`size-report`** now prints the same line under its own torrent-index figure.
- **Nothing ships in the product binary.** `DiskStats` is a small read-only API in `lodestar-search`; only the bench calls it.

## The pilot script now reports live segments (runner, 2026-11-15)
- **`smoke.py`'s `torrent_index` and `file_index` are now the budget figures.** They count the live segment files of the newest `v{schema}-t{tokenizer}` folder and of its `files/`, read from `meta.json` (`index_live`). That's the same count `DiskStats` makes: on a 25,000-torrent index both give 505 B/torrent.
- **The old figures stay, labelled as on disk.** `torrent_index_on_disk` and `file_index_on_disk` are the old `du` figures, so earlier pilot reports stay comparable: in reports from before this change, `torrent_index` and `file_index` are those on-disk figures.
- **The extra bytes are shown beside them, not counted:** `index_stale` (segment files no live segment owns) and `index_other_versions` (other `v*-t*` folders), both per record. `sizes.index_live` holds the raw bytes, plus deleted docs and the segment count.

## Schema, and what reopens this
- **No schema change** (brief step 5). On the evidence above, the 652 B figure is suspected to be a measurement artefact: the 1M synthetic index is 446 B/torrent, the Magnetico pilots measured 360 B (10k) and 306 B (100k), and `du` counts more than the live index.
- **The host re-measure is pending.** The runner asks X-test-vm to run `index-stats` on its next 1M import after this merges.
- **This slice reopens if that host's live segments come out over 512 B/torrent.** The per-part table then says where to cut, by the brief's step 3.
