# X-import-pilot-fixes

**Goal:** fix what the cloud tester's real-dump Bitmagnet import lane found on main a553fd1b (2026-11-13). Cloud testing comes first (owner §2.62).

**Owns:**
- `crates/lodestar/src/cli/import.rs`, plus its tests: the existing unit tests, and the import CLI tests under `crates/lodestar/tests/` that already cover it;
- `crates/sources-importers/src/bitmagnet/conn.rs` (the error message only);
- `crates/sources-importers/src/bin/bitmagnet_check.rs`;
- `docs/operations/imports/pilots/smoke/smoke.py`;
- `docs/operations/imports/bitmagnet-restore.md`;
- `docs/decisions/2026-11-13-X-import-pilot-fixes.md`.

**Fixes, in order:**
1. **Blocks pilots.** `import start` sizes its free-space check by the whole source and ignores `--limit` (`space(config, &store, opened.rows)`, about line 604). A 200k or 1M pilot is refused wherever the full import wouldn't fit.
   - Size it by `min(rows, limit)`.
   - Check `resume` (about line 709) and the dry-run or plan path (about line 498) for the same bug. `resume` has to size by what's left under the job's limit.
   - Add tests: a limit smaller than a source that wouldn't fit is accepted, and the full source is still refused.
2. **The estimate is too high.** `DEFAULT_RECORD_BYTES = 4096`, used before anything has been measured, against a real 1,313 B per torrent measured at 1M, including the index.
   - Runner call (§2.26): set it to **2048**, which keeps about a 1.5× margin over what was measured.
   - Cite the measurement in the decisions file. Leave the measured-value path alone.
3. **Error message.** A Unix-socket connection still needs `--allow-plaintext-loopback`, and the `bad_settings` error doesn't name the flag. Make the message name it and say when it's safe: the socket is on the same machine, so nothing is sent over a network. Don't change when the flag is required.
4. **smoke.py.** `login()` gets 409 `notice_required`, because setup now needs `notice_version` from `GET /api/v1/legal/notice`. Fetch it and send it, the way the real UI does.
5. **bitmagnet_check.** It takes only 88 name samples on a 2M-torrent slice, against 1,000 on the full database. Find out why, for example the sampling stride is computed from the full table or the filter is too tight. Fix it so a slice still gets about the target sample count, or the check says plainly that it sampled fewer. Add a test.
6. **The findings counters.** Per-run "findings" counters don't add up across legs, such as a pause and resume. Find out whether this is a bug or how the counters are defined, then fix it or document it in the decisions file.
7. **Runbook** (`bitmagnet-restore.md`), following the `user-docs` skill:
   - Add the expected scale and duration: a full restore of a 45.6M-torrent, 281.6M-file dump needs about 190 GB for the cluster, plus 8 GB of WAL, and takes about 25 h, limited by one PostgreSQL backend on a single core. Say plainly that this is one measured run.
   - Add that the dump checks can be streamed, so the 97 GiB unpacked dump never has to exist on disk, if the existing tools already support it. Don't build new tools here.
   - **Not in this slice:** the proposed "importer tables only" route, and how Lodestar gets the reader password. The runner tracks those separately.

**Rules**
- No fixed sleeps. Waits are on conditions with deadlines of at least 10 s.
- Never add tokio `test-util` or `testing` to `lodestar`.
- No cloud or AWS calls. The tester's numbers are given above, so don't re-run its lane.
- Run the checks CLAUDE.md lists, and `/security-review` on the diff (the importer reads databases users supply).

**PR:** branch `slice/X-import-pilot-fixes`, title `X-import-pilot-fixes: Size import checks by --limit and fix the pilot lane findings`. Follow `.claude/skills/steward/SKILL.md`. Push once per round. When CI is green, message the runner `ready <PR> <full sha>`.
