# X-watch-stall-retry decisions

Brief: runner, 2026-11-13 (`briefs/X-watch-stall-retry.md`), Track Q (code review 2026-11-12). The span is the runner's §2.26 call on this PR (2026-11-13), which the runner flags to the owner under §2.19. Refines UP-1's "read once it's complete" rule (`docs/decisions/2026-11-12-UP-1.md`, Polling and Afterwards); everything else there stands. Spec: architecture §3, §18.7.

## Why
- **A stalled copy looked complete.** The watch reads a file once its size and modification time match on two polls 10 s apart. A copy over SMB or NFS that stalls for 10 s or more passes that test half-written, reads as malformed, and went to `failed/` for good. The user then had to find it and copy it in again.
- **Stalls of 30–60 s are common on home NAS setups** (a disk spinning up, Wi-Fi roaming), so one more read wasn't enough. Losing a copy to `failed/` costs the user more than a broken file waiting a minute longer before it fails (runner).

## The rule (`crates/lodestar/src/watch.rs`)
- **Only a Malformed verdict is held.** Valid, duplicate (also Accepted), held, refused, judging failed, too large, link, linked and unreadable are put away exactly as before. Those verdicts don't come from a file being incomplete.
- **A file read as malformed stays where it is,** remembered with the size and modification time it was read at and the time of that read. It's counted in the poll's `waiting`, and in a new `rechecking` count (named so it isn't confused with the filter's `held` verdict), not in `failed`.
- **`MALFORMED_SPAN` = 60 s of wall-clock time.** A file goes to `failed/` only when it reads as malformed with the same size and modification time it had at a malformed read at least 60 s earlier. The span is measured with a monotonic clock (`Instant`), not in polls, so it holds whatever the poll interval is.
  - **While the span runs, an unchanged held file isn't read again.** It costs nothing per poll and takes no `MAX_SUBMIT` slot. Once the span is over it's read once more: malformed, and it goes to `failed/` as `<stem>.malformed.torrent`, as before; valid, and it goes to `added/`. So a truly malformed file costs two reads and two judgements, not one. Malformed input is never stored, so the second judgement writes nothing. The pipeline's own upload counters (`received`, `malformed`) count reads, so they go up twice for such a file; nothing reports them outside the pipeline today.
  - **Any change to size or modification time resets the span.** The file is still being written: it isn't read while it changes (UP-1's stability rule, unchanged), and its next read starts a new span from that read.
  - **What a single stall can last:** the first still poll sees a stalled file up to 10 s after the stall starts, the next poll reads it and starts the span, and the read that fails it is the first poll 60 s or more after that. So a single stall under about 70 s always survives, and one over about 90 s is failed. With a 10 s poll, a malformed file now reaches `failed/` about 60–70 s later than before.
  - In a folder with more than `MAX_SUBMIT` files ready, the read after the span waits for its turn in the rotation.

## The bounds (§2.26 numbers, the runner's call)
- **`MAX_HOLDS` = 3 holds per file, for a file that keeps changing.** Each malformed read with a new size or modification time starts a new hold and a new span, up to three. The fourth malformed read with a new shape sends the file to `failed/` at once, without waiting out a span.
  - **How the two limits interact:** the span decides for a file that stops changing (it fails 60 s after its last malformed read, unless that read was valid). `MAX_HOLDS` decides for one that doesn't stop. So a copy may stall and resume three times, each stall up to about 70 s, before it's failed.
  - A file held three times then left unchanged still gets its full span on the third hold.
  - At worst a file is pending for three spans plus the still polls between them, about four minutes, then fails.
  - A file that keeps changing and is never still isn't read at all, as under UP-1's rule. These bounds are on malformed verdicts, so a writer that rewrites a broken file between polls can't keep it pending.
- **`MAX_HELD` = 1,024 files held at once** (4 in tests). When the set is full, a malformed file goes to `failed/` at once, as before this slice. A folder of malformed files can't grow memory without limit; at worst it gets the old behaviour.
- **Forgotten when settled or gone:**
  - A file put away by any path leaves the set.
  - A whole listing that no longer shows a held name drops it, as for the stuck set. In a folder whose listing is never whole (over 10,000 `.torrent` names or 200,000 entries), held files removed by hand aren't pruned; once 1,024 build up, holds stop until a restart, which is the old behaviour.
  - A change of setting that keeps the same folder keeps the set, spans included.

## Restart
- **The set lives in memory only.** After a restart a held file is judged from scratch. It waits for two still polls and is read; if it's malformed, it's held for a fresh span. So a restart can add up to one more span and one more read to an unchanged file (and up to `MAX_HOLDS` more holds to one that keeps changing, since its count starts again). It never sends a file to `failed/` sooner than without one.

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
  - A file that changes between each malformed read is failed after `MAX_HOLDS`.
  - The held set stops at `MAX_HELD`, and other verdicts are never held.
  - A held file removed by hand is forgotten.
  - A change of setting that keeps the folder keeps the set and its span.
  - The tests move a held file's read time back by the span (`age`), so the span stays 60 s and the tests control the clock it's measured on. Nothing here uses tokio `test-util`.
- **Tests that only needed some failure:** tests about the stuck set and `failed/` swaps used a malformed body only as "some failure". They now use the floor's refusal, so they still put each file away on its first read.
