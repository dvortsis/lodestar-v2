# X-spool-damage-log decisions

Follow-ups to WK-11's spool (`docs/decisions/2026-11-11-WK-11.md`, #699) from its reviews: damage logged by segment, and three fixes in `crates/pipeline/src/spool.rs`.

## Damage is logged (rv-699 I3)
- **One event, `pipeline.spool_damaged`** (Warn, component Workers), in the pipeline's catalogue. It fires when a segment's header doesn't read or names another worker (`header`), its last write was torn (`torn`), a frame part-way is bad (`frame`), a record doesn't open (`seal`) or doesn't decode (`record`).
- **It names the segment by its number, never a path or a byte.** The file name is that number in 16 hex digits, inside the spool folder. A label field takes only fixed text, so the number goes in a `Size` field.
- **It can't flood the log.** It's written under the same once-per-segment gate as the drop counts, so a segment read again after a lost session logs nothing more. It's also capped at `MAX_DAMAGE_LOGS` (64) a run, all segments together. The counts (`worker_spool_dropped_total`) still count everything.
- **Shared edits (approved in the brief):** the event's names and en/zh rows in the logging settings (`web/src/lib/features/settings/logging/event-names/pipeline.ts`, `messages/{en,zh}.json`), and the regenerated catalogue golden (`crates/lodestar/src/settings/golden/logs_catalog.txt`). The words are plain; `docs/ui-glossary.md` has no term for the spool yet.

## A segment's number is spent before its write (rv-699 r3 I1)
`start_segment` moves `next_seq` on before writing the new segment. Before, a write that failed part-way left the number where it was. That happened if the folder sync failed (EMFILE, EIO), or a file was planted at that name after the spool opened. Every later append then tried the same name and failed until a restart. Now the failure costs one append, and the next takes a fresh number (`a_planted_name_at_the_next_number_doesnt_wedge_the_spool`). A header-only segment that was placed but couldn't be reopened stays in the counts, so they match the disk.

## A reopened segment with a second link is refused (rv-699 r3 N1)
The reopen for appending also refuses a file with another hard link (Unix), as `secure_remove` does. Nothing is appended through a link planted between the placement and the reopen. It's safe to refuse, since the number has already moved on.

## What secure removal does on each platform (rv-699 r3 I2; corrects WK-11's note)
WK-11's decisions file said a planted link's target is never zeroed "on either platform". That holds on Unix only:
- **Unix:** a file with another hard link (a link count above 1) is unlinked without being overwritten, so nothing planted in the folder can have another file zeroed. A symbolic link is never followed.
- **Windows:** a symbolic link or other reparse point is never followed, as on Unix. But a file's link count isn't on stable Rust, so a hard link isn't detected: a regular file planted in the folder as a hard link to another file has that file's contents zeroed. Making a hard link on Windows needs write access to its target, and the data folder's ACL check refuses other owners at start. The spool's own leftover second link, a temp name beside its segment, is never zeroed, because `Spool::open` unlinks it without overwriting it.
