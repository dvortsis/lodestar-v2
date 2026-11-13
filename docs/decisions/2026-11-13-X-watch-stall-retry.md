# X-watch-stall-retry decisions

Brief: runner, 2026-11-13 (`briefs/X-watch-stall-retry.md`), Track Q (code review 2026-11-12). Refines UP-1's "read once it's complete" rule (`docs/decisions/2026-11-12-UP-1.md`, Polling and Afterwards); everything else there stands. Spec: architecture §3, §18.7.

## Why
- **A stalled copy looked complete.** The watch reads a file once its size and modification time match on two polls 10 s apart. A copy over SMB or NFS that stalls for 10 s or more passes that test half-written, reads as malformed, and went to `failed/` for good. The user then had to find it and copy it in again.

## The rule (`crates/lodestar/src/watch.rs`)
- **Only a Malformed verdict gets a second look.** Valid, duplicate (also Accepted), held, refused, judging failed, too large, link, linked and unreadable are put away exactly as before. Those verdicts don't come from a file being incomplete.
- **A file read as malformed stays where it is,** remembered with the size and modification time it was read at. It's counted in the poll's `waiting`, and in a new `held` count, not in `failed`.
- **The next read decides.**
  - Still malformed, with the same size and modification time: the file isn't changing, so it really is malformed. It goes to `failed/` as `<stem>.malformed.torrent`, as before. With a 10 s poll that's one poll later, so a malformed file now reaches `failed/` about 10 s later than before.
  - Changed in between: it's still being written. It isn't read while it changes (UP-1's stability rule, unchanged), and is judged afresh once it's still again. If that read is valid, it goes to `added/`.
- **A file is read again only on a later poll, never twice in one.** A truly malformed file costs two reads and two judgements instead of one. Both are bounded by `MAX_SUBMIT` per poll, and malformed input is never stored, so the second judgement writes nothing.

## The bounds (§2.26 numbers, the runner's call)
- **`MAX_HOLDS` = 3 holds per file.** Each malformed read with a new size or modification time is held again, up to three times. The fourth malformed read sends the file to `failed/`, whatever changed. So a copy may stall and resume three times, 30 s or more of stalls in all, before it's failed. A file that keeps changing forever and is never still isn't read at all, as before UP-1's rule; this bound is on malformed verdicts, so a writer that rewrites a broken file between polls can't keep it pending.
- **`MAX_HELD` = 1,024 files held at once** (4 in tests). When the set is full, a malformed file goes to `failed/` at once, as before this slice. A folder of malformed files can't grow memory without limit; at worst it gets the old behaviour.
- **Forgotten when settled or gone:** a file put away by any path leaves the set, and a whole listing that no longer shows a held name drops it, as for the stuck set. A change of setting that keeps the same folder keeps the set.

## Restart
- **The set lives in memory only.** After a restart a held file is judged from scratch: it waits for two still polls, is read, and if malformed is held once more. So a restart can give a file at most one extra read, and never sends a file to `failed/` sooner than without one.

## Tests
- `crates/lodestar/tests/watch.rs`, `a_stalled_copy_that_finishes_is_taken_not_failed`: the real binary. Half a `.torrent` is written, then a valid file that sorts after it. Once the valid one is in `added/` (so both were read on the same poll, the half file after more than 10 s unchanged), the half file is still in the folder and `failed/` is empty. The copy then finishes, and the file reaches `added/` and search. Every wait is on a condition with a 60 s deadline; no fixed sleep. The test fails without the hold.
- The existing served test still sees its broken file in `failed/`, now on the second read.
- Unit tests in `watch.rs`: an unchanged malformed file fails on its second read and is never read again; a stalled copy that finishes is taken; a file that changes between each malformed read is failed after `MAX_HOLDS`; the held set stops at `MAX_HELD`, and other verdicts are never held; a held file removed by hand is forgotten.
- Tests about the stuck set and `failed/` swaps used a malformed body only as "some failure". They now use the floor's refusal, so they still exercise one put-away per file.
