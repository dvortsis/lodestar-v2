# X-watch-stall-retry decisions

Brief: runner, 2026-11-13 (`briefs/X-watch-stall-retry.md`), Track Q (code review 2026-11-12). The 60 s span and the 10 min pending limit are the runner's §2.26 calls on this PR (2026-11-13), which the runner flags to the owner under §2.19. Refines UP-1's "read once it's complete" rule (`docs/decisions/2026-11-12-UP-1.md`, Polling and Afterwards); everything else there stands. Spec: architecture §3, §18.7.

## Why
- **A stalled copy looked complete.** The watch reads a file once its size and modification time match on two polls 10 s apart. A copy over SMB or NFS that stalls for 10 s or more passes that test half-written, reads as malformed, and went to `failed/` for good. The user then had to find it and copy it in again.
- **Stalls of 30–60 s are common on home NAS setups** (a disk spinning up, Wi-Fi roaming), so one more read wasn't enough. Losing a copy to `failed/` costs the user more than a broken file waiting a minute longer before it fails (runner).

## The rule (`crates/lodestar/src/watch.rs`)
- **Only a Malformed verdict is held.** Valid, duplicate (also Accepted), held, refused, judging failed, too large, link, linked and unreadable are put away exactly as before. Those verdicts don't come from a file being incomplete.
- **A file read as malformed stays where it is,** remembered with the size and modification time it was read at and the time of that read. It's counted in the poll's `waiting`, and in a new `rechecking` count (named so it isn't confused with the filter's `held` verdict), not in `failed`.
- **`MALFORMED_SPAN` = 60 s of wall-clock time.** A file goes to `failed/` only when it reads as malformed with the same size and modification time it had at a malformed read at least 60 s earlier. The span is measured with a monotonic clock (`Instant`), not in polls, so it holds whatever the poll interval is.
  - **While the span runs, an unchanged held file isn't read again.** It costs nothing per poll and takes no `MAX_SUBMIT` slot. Once the span is over it's read once more: malformed, and it goes to `failed/` as `<stem>.malformed.torrent`, as before; valid, and it goes to `added/`. So a truly malformed file costs two reads and two judgements, not one. Malformed input is never stored, so the second judgement writes nothing. The pipeline's own upload counters (`received`, `malformed`) count reads, so they go up twice for such a file; nothing reports them outside the pipeline today.
  - **Any change to size or modification time resets the span.** The file is still being written: it isn't read while it changes (UP-1's stability rule, unchanged), and its next read starts a new span from that read.
  - **What a single stall can last:** the first still poll sees a stalled file up to 10 s after the stall starts, the next poll reads it and starts the span, and the read that fails it is the first poll 60 s or more after that. So a single stall under about 70 s always survives, and one over about 90 s is failed. On a steady 10 s poll the failing read is usually the poll 70 s after the read that started the span, since the check comes just under a whole number of polls after it. However a file stalls and resumes, it's pending at most 10 min (below). With a 10 s poll, a malformed file now reaches `failed/` about 60–70 s later than before.
  - In a folder with more than `MAX_SUBMIT` files ready, the read after the span waits for its turn in the rotation.

## The bounds (§2.26 numbers, the runner's call)
- **`PENDING_FOR` = 10 min of wall-clock time per file,** measured on `Instant` from its first malformed read, however many times it changes. After that the file goes to `failed/` as `<stem>.malformed.torrent`, without another read, on the next poll that sees it, whether it's still changing or not.
  - **It replaces a count of holds** (an earlier round of this PR had 3). A count punished a copy that's slow but still making progress, such as a slow Wi-Fi SMB link or a large multi-file `.torrent` that reads malformed each time it's briefly still: it failed after three such reads though it never stalled for long (runner). No per-file count is kept now; memory is bounded by `MAX_HELD` alone.
  - **How the two limits interact:** the span decides for a file that stops changing. It fails 60 s after its last malformed read, unless the read after the span is valid. The 10 min limit decides for one that doesn't stop, however its changes fall. A change starts a new span but never a new pending time. So a file that stops changing late is failed at whichever comes first: its span ending, or 10 min from its first malformed read.
  - A file that keeps changing and was never read isn't pending at all. As under UP-1's rule it simply isn't read; the limit applies only once a malformed read has made it pending.
- **`MAX_HELD` = 1,024 files pending at once** (4 in tests), the same size as the stuck set's `MAX_STUCK`. Filling it takes at least 32 polls of nothing but new malformed files, since `MAX_SUBMIT` is 32 reads per poll.
  - **When a new file would pass it, the oldest pending file goes to `failed/`** to make room: the one whose first malformed read is earliest. It's moved as `malformed` without another read, **and only if a regular file is still there by that name** (looked at without following). A name that's now a folder, a link or nothing is only dropped from the set, never moved; a later poll judges what's there as usual (a link goes to `failed/` as `link`). A held name that turns into a folder or other entry also leaves the set when a poll sees it (tested).
  - That poll's `watch.took` line counts it in `failed` and again in a new field, `pending_full`, so the log says why. No file name appears, as for every watch line. (This adds a field to the list in UP-1's decisions file, which this PR doesn't edit.)
  - **A name an eviction dealt with is skipped for the rest of that poll,** whatever the eviction did. The poll's listing of it is stale, and the next poll judges what's there. Without this, a held name swapped for a folder and reached later in the poll was opened from the stale listing; on Windows that open fails, and the folder would have gone to `failed/` as `unreadable` (CI on #837). Tested on both: the poll's `waiting` count shows the name wasn't visited again.
  - The new field is the only change to the event, and the log catalogue lists event ids, not fields, so its golden file is unchanged.
  - A file evicted this way may have finished copying since its last read. It's then in `failed/` with a cause that's out of date; the user can move it back. This needs over 1,024 files pending at once.
- **Forgotten when settled or gone:**
  - A file put away by any path leaves the set.
  - A whole listing that no longer shows a held name drops it, as for the stuck set. In a folder whose listing is never whole (over 10,000 `.torrent` names or 200,000 entries), held files removed by hand aren't pruned; they're the oldest, so new pending files push them out. A file pushed out that's no longer there is neither counted nor remembered: a move that finds nothing by that name is "gone", not stuck. That applies to every move: a file removed between its read and its move no longer leaves a stuck entry either. **"Not found" means gone only when the file itself is absent,** checked again through the folder's handle: if `added/` or `failed/` went missing mid-move, the move is an ordinary failure and the file is stuck, never read again, as before (tested).
  - A change of setting that keeps the same folder keeps the set, spans and pending times included.

## Restart
- **The set lives in memory only.** After a restart a held file is judged from scratch. It waits for two still polls and is read; if it's malformed, it's held for a fresh span. So a restart can add up to one more span and one more read to an unchanged file, and a fresh 10 min to one that keeps changing, since its pending time starts again. It never sends a file to `failed/` sooner than without one.

## Tests
- **Real-binary tests** in `crates/lodestar/tests/watch.rs`:
  - **`a_stalled_copy_that_finishes_is_taken_not_failed`:**
    - Half a `.torrent` is written, then a valid file that sorts after it.
    - Once the valid one is in `added/`, both were read on the same poll, so the half file was read as malformed after more than 10 s unchanged. It's still in the folder.
    - The stall goes on until 50 s after the write, and every check on the way finds the file still in the folder and `failed/` empty.
    - The copy then finishes, and the file reaches `added/` and search.
    - That wait is the stall being tested, not a wait for the watch; every other wait is on a condition with a deadline of at least 60 s, and there's no fixed sleep.
    - It fails with a 10 s span (checked), and so without the hold.
    - **One narrow race is left:** a poll that lists the folder in the microseconds between the two writes sees the half file a poll early. The read that starts its span still comes at least 10 s after the write, so the test still passes for the right reason. It never passes falsely.
  - **The served test:** its broken file reaches `failed/`, and the test checks that it took more than 60 s from the write, within a 180 s deadline.
- **Unit tests in `watch.rs`:**
  - An unchanged malformed file isn't read while the span runs, however many polls, then fails once the span is over and is never read again.
  - A change starts the span again.
  - A stalled copy that finishes is taken.
  - A file that changes between each malformed read, eight times, more than the old 3-hold cap allowed, is taken once complete. The polls are driven by hand, so this is the short-poll test.
  - A file pending past `PENDING_FOR` is failed unread, though it changed and started new spans in between.
  - The held set stops at `MAX_HELD` by failing the oldest pending file, counted in `pending_full`; other verdicts are never held.
  - Evicting a file that was removed meanwhile counts nothing and leaves no stuck entry (it fails without the "gone" result); an evicted file later in the same poll isn't read again.
  - A held file removed by hand is forgotten.
  - A change of setting that keeps the folder keeps the set and its span.
  - The tests move the watcher's own clock forward (`age`, `age_by`: a test-only skew on `Watcher::now`), so the span stays 60 s and the limit 10 min, and the tests control the clock they're measured on. Moving the clock forward can't underflow, as moving instants back could on a Windows machine just booted. Nothing here uses tokio `test-util`.
- **Added CI time:** the served test now waits for the broken file's 60 s span, about 70–90 s more than before.
- **Tests that only needed some failure:** tests about the stuck set and `failed/` swaps used a malformed body only as "some failure". They now use the floor's refusal, so they still put each file away on its first read.
