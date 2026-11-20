# X-flake-log-history-win decisions

The flake: `logging::source_tests::history_joins_the_ring_and_the_files_without_gap_or_duplicate` failed on Windows CI only (on #973, run 44705991207, first attempt). It's a real bug in `/logs` history, not in the test.

- **What failed.** The paged history was 299..162, then 139..0: lines **161..140 were missing**, a contiguous 22-line run, with nothing repeated (the runner read this from the job log).
  - In that test, rotated file C holds lines 140–209. Page 6 ended at line 162 with its cursor inside C. So page 7 lost exactly the rest of C and resumed at the next older file, B.
- **The cause** is in `files::open_listed`, which history uses to open a listed file.
  - A rotated `.jsonl` that was compressed since the listing is read as its `.gz`, but **only when opening the `.jsonl` fails with "not found"**.
  - Compression renames the finished `.gz` into place, then deletes the `.jsonl`. On Linux that delete is atomic, so a later open gets "not found" and falls back.
  - On Windows, a file being deleted while any handle is open on it (the reader that just listed it, a scanner, the indexer) can stay in the folder, unopenable: the open fails with access denied or delete pending, not "not found".
  - So history counted C unreadable, skipped the rest of it, and went on to B. That's a silent gap in `/logs`, not just in the test.
- **The fix (production code):** `open_listed` falls back to the `.gz` on **any** error opening a rotated `.jsonl`, not only "not found".
  - That's always correct: the `.gz` is renamed into place only once it's whole (written, synced), and it holds the same lines at the same offsets.
  - When the `.gz` can't be opened either, the `.jsonl`'s own error is returned, as before.
  - The current file has no `.gz`, so its errors stand.
  - The `.gz` still goes through `open_regular`, so a link or other non-regular file under either name is still refused.
- **`history::head_of`** (the first-line fingerprint that finds the current file again after a rotation) now opens through `open_listed` too. A rotated `.jsonl` mid-deletion would otherwise read as fingerprint 0, so `locate` wouldn't recognise the file the current one became.
- **Reproduced on Linux.** `a_jsonl_that_cant_be_opened_beside_its_gz_is_read_as_the_gz` (`history.rs`) puts a folder under the `.jsonl` name: opening it fails, and not as "not found". It sits beside the finished `.gz`.
  - Before the fix, `open_listed` returns that error (the file would be skipped). After it, the `.gz` is read. The test also checks that without a `.gz` the `.jsonl`'s error stands, and that `head_of` reads the `.gz`.
  - A delete-pending file can't be made on Linux; the folder stands in for it.
- **Also added:** `compression_finishing_between_any_two_pages_keeps_history_whole` (`source_tests.rs`). It holds every rotated file uncompressed, then finishes all compressions just before each page in turn (16 cases), and checks that the history is exactly 299..0 each time. The race in the original test is inside a page, so this guards the cursor's `.gz` fallback between pages.
- **The flaky test is unchanged.** It isn't skipped, ignored or gated off, and no sleeps were added.
