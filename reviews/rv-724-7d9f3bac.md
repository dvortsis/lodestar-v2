VERDICT: CHANGES

# rv-724 at 7d9f3bac (X-spool-damage-log), fresh Opus review plus a security pass

Reviewed against the brief (`runner-state:briefs/X-spool-damage-log.md`), rv-699 f0eaa328 (r3 I1, I2, N1), rv-699 I3, sec-699, `runner/standards.md` §5 and `docs/decisions/2026-11-11-X-spool-damage-log.md`.

The spool code is correct and the security posture is better than on main. The changes asked for are test-only and small. Two of the three mutations you asked for survive, fully or in part. Under standards §5 ("a missing test for new behaviour"), that blocks.

## Blocking

**B1. The per-frame dedupe has no test that can fail. The test's comment claims a case the test never reaches.**
- **Where:** `crates/pipeline/tests/spool.rs:635` (`damage_is_logged_once_per_segment`, the comment "Two bad frames part-way: still one line").
- **Scenario:** flipping a byte in a frame body breaks that frame's sum. `decode_segment` then stops at the first bad frame (`Tail::Corrupt`), so `seg.damage` holds a single `frame` entry and the second flipped frame is never seen. The "one line however many frames are bad" logic (`spool.rs:1273-1280`) is therefore never exercised with more than one damage entry.
- **Mutation:** I replaced the one-line block with one `log_damage` per `seg.damage` entry. The whole suite still passes.
- **The code itself is right.** A probe that writes four records under key A and reads them under key B gives 4 `seal` damages and `damage_logged == 1` at the head. Under the mutation, the same probe gives 4 and fails.
- **Fix:** replace or extend the first block with damage that doesn't stop the segment:
  - Several `seal` failures: write with `SecretStore::from_key(&[9; 32])`, reopen with `secrets()`, and read 3 times. Assert `damage.len() == 4` and `damage_logged == 1`.
  - Or several `record` failures.
  - Fix the comment either way.

**B2. Mutation "reintroduce the reopen-by-name" survives. The removal the decisions file credits with "closes the gap on both platforms" is untested.**
- **Where:** `crates/pipeline/src/spool.rs:1121-1141` (`start_segment`); test `appends_never_follow_the_segments_name` (`spool.rs:1519-1535`).
- **Scenario:** that test plants the symlink after the first append, while the segment is already open. Main's code passes it too: it reopened once in `start_segment` and appended through that handle. So the test can't tell "appends use the creating handle" from "appends use a handle reopened by name".
- **Mutation:** after `place_new` returns, drop the handle and reopen `<seq>.lsp` with `OpenOptions::new().append(true)`. That is a plain reopen, without even `no_follow` or the identity check. All tests pass.
- The window the change closes is between placement and the first append's handle. No test reaches it.
- **Fix:** a `#[cfg(test)]` hook in `place_new`, like `FAIL_NEXT_DIR_SYNC`, run after `hard_link` succeeds. The hook swaps `<seq>.lsp` for a symlink to an outside victim file (Unix). Assert that the append still succeeds, the victim is byte-identical, and the record reads back from the anonymous or renamed file, or at least never reaches the victim. Reintroducing any reopen-by-name then fails the test.
- Alternatively, have the hook swap in a different regular file and assert it stays unchanged. That also covers Windows.

## Important

**I1. No test checks that the event is emitted, or what it carries.** The "never a path or a byte" guarantee rests on the field types alone.
- **Mutation:** replacing the `log_event!` call in `log_damage` (`spool.rs:1457-1466`) with a no-op passes every test. All the tests read `SpoolStats::damage_logged`, which is bumped before the emit.
- The field kinds (`segment: Label` fed with `Count(seq)`, `part: Label` fed with `&'static str`) are fixed by the catalogue at compile time, so a leak would need a code change. Even so, the event the brief asks for (rv-699 I3) is never observed.
- **Fix:** install a capturing sink, as `crates/companions/tests/manager.rs:1828` does (`capture()`). Assert one `pipeline.spool_damaged` line with `segment` = the decimal number and `part` = the expected value, and nothing else: no path, and no field beyond the two.
- Cheap to do here. Otherwise, put it in the next brief that touches the spool.

**I2. The PR body says P3 is "noted in the decisions file". It isn't.**
- P3 is two cases that are counted but not logged: a bad-header segment that `open()` can't remove (it bumps `leftovers`, `spool.rs:978-980`, with no log), and records `reseal()` drops (`spool.rs:1381`).
- `docs/decisions/2026-11-11-X-spool-damage-log.md` doesn't mention either. P2 (no closing line after the cap) is covered, implicitly, by "Past the cap nothing more is logged until the worker restarts".
- **Fix:** add one bullet under "It can't flood the log" (or a "Not logged" bullet), or correct the PR body.

## Nits

- **N1.** `spool.rs:836-843`: `write_atomic`'s new doc comment ("Rewrites `<dir>/<seq>.lsp` … (a reseal)") now sits directly above `struct NotMade`'s doc, so rustdoc attaches both to `NotMade` and `write_atomic` (`:896`) has none. Move the paragraph down to `write_atomic`.
- **N2.** `spool.rs:828-834`: `Place` now has one variant, `Replace`, so the parameter and the `match` are dead weight. Either remove `Place` or keep it on purpose with a comment. Optional.
- **N3.** `docs/decisions/2026-11-11-X-spool-damage-log.md:15`: "The reopen had checked only that the name pointed to a regular, single-link file". Main's reopen checked "regular" and `same_file(looked, opened)`. It never checked the link count, and that missing check was N1's whole point. Say "a regular file, the one the name pointed to at that moment".
- **N4.** The same line says "so the temp name is still unlinked while the handle stays open". With Windows' legacy (non-POSIX) delete semantics, the name may stay delete-pending until the segment's handle closes. That's harmless: the number is spent, `reseal` closes the active segment first, and the kernel completes the delete when a crashed process's handles close. Something like "unlinked, or delete-pending until the segment closes" is exact.
- **N5.** `spool.rs:1273-1276` and the event docs say "the part that failed first". In fact a `frame` (corrupt tail) entry is pushed before the per-frame loop. So with an earlier `seal` failure plus a corrupt tail, the line says `frame`. Say "the first part found", or accept it. Cosmetic only.
- **N6. A residual window, not this PR's to fix.**
  - Between `create_private(<seq>.tmp)` and `hard_link`, someone who can write the spool folder can rename the temp file away and plant another at the temp name. The planted file becomes `<seq>.lsp`, and the spool keeps appending to its own file wherever it was moved.
  - What that party gets is sealed frames (AEAD under the worker's key), which is no more than they could read from the folder anyway.
  - On Unix this needs the 0700 folder's owner. On Windows it needs write rights on the data folder's ACL, the same precondition the decisions file already states.

## Security (/security-review pass on the spool's file handling)

**No vulnerability found at or above Medium.**

**Data to a swapped name.**
- Every append now goes through the handle that `create_new` made (`place_new` → `Active.file`). On both platforms, nothing planted at `<seq>.lsp` after placement receives a byte: no symlink, junction, hard link or replacement file.
- Reseal still writes a fresh `create_new` temp and renames it over the target, so nothing is written through the target name.
- The remaining window is the temp-name one in N6. It yields ciphertext only, to someone who already has write access to the folder.

**Secure removal.**
- The Windows statements in the doc comment (`spool.rs:699-709`), the `shared()` comment and the decisions file are now accurate: symlinks and reparse points are never followed on either platform, and on Windows a planted hard link's target is zeroed.
- The precondition is stated correctly: create rights in the spool folder, inherited from the data folder's ACL, which the spool doesn't check itself.
- WK-11's file gets a one-line pointer, not a rewrite, as the brief asked.
- One overclaim survives in WK-11's file: "(Unix: …) so a planted hard link … can't have its file zeroed". It is scoped to Unix by its parenthesis, so it's acceptable.

**Counts after a failed sync.**
- `placed: true` happens only after `hard_link` succeeds and the temp name is unlinked, so a header-only `<seq>.lsp` is on disk, and it goes into `closed` with `HEADER_LEN` bytes and 0 frames. That matches the disk.
- `placed: false` covers create, write, sync and link failures, and the temp name is removed then.
- The number is spent in every case, so there's no `AlreadyExists` wedge. Mutation tests confirm both points.
- If the temp unlink fails on Windows, the bytes are under-counted, as on main. The next open fixes it.

**Hostile or damaged segments.**
- The decoders and `read_capped` are unchanged from WK-11 (sec-699 §1 audited them). Allocation is capped at `SEGMENT_BYTES + MAX_FRAME + FRAME_HEAD`.
- The new code adds no loops over untrusted lengths. The `fuzz (worker_spool)` CI job is green at this head.
- The log path cannot panic: `Count(seq)` takes a `u64`, and `part` is a `&'static str` from a fixed set.

**Log content.**
- `segment = Count(seq)` (the decimal number) and `part` from {header, frame, seal, record, torn}. Neither is a path, a byte, an infohash or a peer address. The catalogue fixes both fields as `Label`.
- `Damage.segment` (the 16-hex file name) predates this PR and isn't logged by it.

**Flood cap.**
- Per spool (a new `SpoolStats` per `Spool::open`), 64 lines. The check-then-bump isn't atomic, but every caller holds `&mut Spool` or runs inside `open()`, so there's one writer. Drops are still counted past the cap (`damage_logging_is_capped` checks `dropped_torn == 67`).
- Re-reads after a lost session don't log again (`counted`). `forget()` clears `counted`, but a forgotten segment isn't read again in that run.

**Pre-existing (WK-11, not this PR; worth a later slice).**
- `Spool::open`'s header read (`spool.rs:967`), `count_frames`, `is_expired` and `read_capped` use a plain `File::open`, which follows symlinks and, on Unix, blocks on a FIFO.
- A FIFO planted as `<seq>.lsp` hangs `Spool::open` at worker start, on its blocking thread. A symlink makes the read follow its target, but only "header" damage results, and no bytes are logged or sent.
- Both need write access to the 0700 folder. Low. sec-699 didn't raise it.

## Other checks

**Flake fix** (`crates/lodestar/src/worker/spool.rs:576-584`).
- It now waits on the actual condition: `until("drained", …)` polls `rt.reconcile()`, which takes the `compare` flag via `swap(false, AcqRel)`, under the shared 20 s deadline with a 10 ms poll interval (`runtime.rs:2277`). No fixed sleep.
- The polling consumes the flag only when it is set, so `assert_eq!(d, Some(0))` and the following `assert_eq!(rt.reconcile(), None)` keep their old meaning.
- Every other assertion in the test is unchanged. Test code only.

**Test helper change** (`tests/spool.rs:225`, `fs::metadata(path)` instead of `DirEntry::metadata`). Test-only and in an owned file. The reason (a Windows listing reports stale sizes) is plausible and is explained in the PR body.

**Brief coverage.**
- I3: done.
- r3 I1: done.
- r3 I2: done.
- r3 N1: done another way, with the runner's approval stated in the PR. Its test is weak (B2).
- The optional `claim()` test was left out, as the brief asked, and `runtime.rs` is untouched.

**Hygiene.**
- **Paths.** Changed paths are the owned files plus the approved shared edits: `log_events.rs`, `event-names/workers.ts`, `messages/{en,zh}.json`, the golden, the WK-11 pointer, and the test-only `worker/spool.rs` (runner-approved, listed in the body).
- **Event names.** `pipeline.spool_damaged` is in the Workers chunk because its component is Workers, and the golden agrees: `component: workers`, `level: warn`.
- **Messages.** The en and zh key sets are identical (checked with jq), and the zh wording matches the en meaning.
- **Text.** No model identifiers in the diff or the PR body. The only ones are the harness-supplied Co-Authored-By trailers, which CLAUDE.md allows. No 12-digit numbers outside the golden JSON, and no local times.

## Verified

The worktree was at 7d9f3bac, detached, with `CARGO_TARGET_DIR` in the scratchpad, `DEBUG=0` and `INCREMENTAL=0`.

**Commands:**
- `cargo fmt --check`: clean.
- `cargo +1.99.0 clippy -p lodestar-pipeline -p lodestar --all-targets -- -D warnings`: clean. 1.99.0 was present, so no fallback.
- `cargo test -p lodestar-pipeline`, 3 runs: all pass, including the lib's 182 tests (2 ignored) and the `tests/spool.rs` suite.
- `cargo test -p lodestar --bin lodestar the_catalog_lists_every_event_and_kind` (logs-catalog golden): pass.
- `cargo test -p lodestar --bin lodestar worker::spool`, 5 runs: 10/10 each time, including the flake-fixed replay test.
- `web/`: `npx vitest run src/lib/features/settings/logging`, 4 runs. The first run had 2 failures; it ran while a cargo build loaded the machine and its output wasn't kept, so I couldn't see which tests failed. The next 3 runs passed 57/57. I've treated it as load or a cold cache, not this PR, but it's unconfirmed.
- CI at this head: `ci-ok` is green, including `test (windows-latest)` and `fuzz (worker_spool)`.

**Mutations** (each applied to `crates/pipeline/src/spool.rs`, run with the pipeline lib tests and `--test spool`, then reverted; the tree was clean afterwards):

| Mutation | Result |
| --- | --- |
| Remove spend-before-write (`next_seq` set only after `place_new` succeeds) | Killed by `a_failed_folder_sync_keeps_the_counts_matching_the_disk` (lib) and `a_planted_name_at_the_next_number_doesnt_wedge_the_spool` (integration; seen with `--no-fail-fast`) |
| Reintroduce the reopen-by-name (drop the handle, `append(true).open(<seq>.lsp)`) | **Survived** (B2) |
| Drop the per-segment dedupe: log on every read (`if count` becomes `if true`) | Killed by `damage_is_logged_once_per_segment` |
| Drop the per-segment dedupe: one line per damage entry | **Survived** (B1). Killed by my scratch probe (4 seal failures expect 1 line; the mutation gave 4) |
| Drop the `placed` count branch | Killed by `a_failed_folder_sync_keeps_the_counts_matching_the_disk` |
| Drop the 64-line cap | Killed by `damage_logging_is_capped` |
| Remove the `log_event!` emit and keep the counter | **Survived** (I1) |

**Waits.** None of the new tests sleeps. The one wait (the flake fix) is a condition with a 20 s deadline.
