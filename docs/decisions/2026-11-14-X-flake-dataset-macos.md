# X-flake-dataset-macos decisions (#896)

The 10M bench dataset's determinism tests (`bench/tests/dataset.rs`, M8-PERF-1a) failed on macOS on every merge since they were added. Linux and Windows passed. This records the cause and what the tests compare now. It is the runner's call, as a test-correctness question.

## What macOS showed
Each finding below comes from a diagnostic CI run started by hand on the branch.
- **Where the content differed:** `torrent_files` only, the stored file-tree blobs. Same codec (zstd), same dictionary id, different bytes, first at torrent 56 in every run. All the other digested tables matched, the dictionary's bytes included.
- **The trees were the same:** decompressed with each store's own dictionary, the differing blobs held identical trees. Torrent 56's blob was 28,505 bytes in some builds and 28,510 in others, the same two sizes in every run.
- **A fresh compressor varied too:** four builds ran on four threads at once, each compressing torrent 56's tree again on its own thread. In 1 of the 4 builds, a new compressor gave 28,505 bytes. The store and the other three builds gave 28,510.
- **It's rare:** 160 compressions of that tree, fresh and reused, on four threads, all gave 28,510.
- **Where:** zstd 1.5.7 (zstd-sys 2.1.2) on macOS arm64, at the store's settings: level 19, a trained dictionary, no checksum, no dictionary id in the frame.

## What Linux showed
zstd gave one output for one input in every test:
- whatever glibc's heap held (`MALLOC_PERTURB_` with four values, every allocation on the heap);
- with inputs placed back to back in memory, in one shared buffer, or at overlapping offsets;
- from a fresh compressor or one reused across every tree.

valgrind's memcheck found no read of uninitialised memory. The file-tree encoder and the dictionary training are deterministic.

## Why the store isn't changed
- **The store's compressor settings don't vary by platform.** zstd-sys is built with `std`, `legacy` and `zdict_builder` only, without `zstdmt`. `BlobCodec` copies its dictionary and sets only the dictionary-id and checksum flags. zstd's automatic choices (long-distance matching, the row match finder, the block splitter) follow from the level and the input size alone.
- **A store fix can't help.** A prepared dictionary, or a fresh context for each blob, wouldn't make zstd's output fixed, because a fresh context is what varied.
- **Nothing in the product depends on blob bytes being identical.** `torrent_files` is read only to decompress it, to train the dictionary and to recompress. The update journal's hash covers a copy of one store file, checked against itself. Snapshot deletions are keyed by info hash. A worker's blob keeps the worker's own bytes, and nothing compares it. Backup and the import digest hash no blob bytes.

## What the tests compare now
- **The content digest** (`dataset::digest`) reads a `torrent_files` row as its torrent, codec, dictionary id and the tree the blob decodes to, each blob decoded with that store's own dictionaries. A blob that won't decode counts as its bytes, marked apart. Every other column of every digested table still counts as stored. That includes `zstd_dicts`, so the dictionaries count by their bytes.
- **The byte-for-byte checks** (`same_bytes` in the tests) still compare every byte of the store file, apart from SQLite's two checkpoint counters in the header.
  - When the bytes differ, both stores are copied with each file-tree blob replaced by the tree it holds, then rebuilt with `VACUUM INTO`, and those must be the same bytes.
  - The rebuild is needed because a blob of another length moves the pages after it. Nothing else is excused: every other byte of content still has to match.
- **Tested on Linux** (`a_file_tree_counts_by_its_tree_not_its_encoding`): one blob encoded another way (another level, the same dictionary) leaves the digest and the store's bytes the same, and a blob holding a different tree changes both. Each half fails if the digest goes back to the compressed bytes, or stops reading the tree.
- **Diagnostics kept:** on a mismatch, the tests name each differing table, its first differing row and column, and whether differing file-tree blobs hold different trees.

## Not done
- **No report to zstd upstream.** Filing with an outside project is the owner's call; the runner puts it to the owner as an optional item.
