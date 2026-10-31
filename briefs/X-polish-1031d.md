# X-polish-1031d: re-queue list cap, soak report column
**Model:** Opus. Item 1 touches the classification re-queue (content safety, §20.9). Runner brief, 2026-10-31. Optional findings from the #290 and #292 reviews. `/security-review` is required. **Item 1 must not loosen anything.**

## Read first
- CLAUDE.md
- `runner/standards.md`
- `docs/decisions/2026-10-31-X-requeue-2.md` and `-X-polish-1031c.md`
- `docs/architecture.md` §20.9
- `crates/api/src/filters.rs`: `push_pending_requeue`, `drop_pending_requeue`, `requeue`, `resume_requeues`
- `docs/soak/tools/analyze_cr0.py` (the report table, ~595–600)

## Do
1. **Bound `classify.requeue_pending` overall** (#290 review, O1).
   - **The problem:** today the list is bounded per distinct query text only. While search is off, an admin who adds and removes many different class rules grows it without limit. Each entry has to stay until an index marks its matches.
   - **The fix:** cap the list at a small number. Pick it and justify it in the decisions file; something like 64 is in range.
   - **Past the cap,** collapse the list into a single "full re-mark" entry. When an index is available, that entry marks every stored item for re-classification. Use whatever `requeue` already does for "everything unenriched", or the nearest existing full re-mark path. Find it; don't invent a new one if one exists.
   - **The full re-mark is cleared on the same terms as any entry:** only after an index has done all of it, never with search off, never after a shutdown mid-way. It is never cleared by a later re-queue of a single text.
   - **Tests:**
     - pushing past the cap yields the full re-mark entry;
     - with search off it survives restarts;
     - turning search on marks every item;
     - a shutdown mid-way keeps it;
     - dedupe still holds below the cap.
     - Condition waits only.
2. **The soak report column** (#292 review). In `analyze_cr0.py`, the report column "Peak RSS (MiB)" shows `working_set_bytes`. In Docker mode that's cgroup working set including page cache.
   - Rename the column to "Peak working set (MiB)".
   - Add "Peak private (MiB)" from `private_bytes`, if present in the rows.
   - Update its test or golden output.
   - Keep the JSON key `rss_max_mib` for compatibility with older consumers, and add `private_max_mib`.
3. **Decisions file:** `docs/decisions/<date>-X-polish-1031d.md`, one line per item.

## Own
- `crates/api/src/filters.rs` (the re-queue functions only) and its tests.
- `docs/soak/tools/analyze_cr0.py` (the report table and summary keys only) and its test.
- Your decisions file.

**Don't touch** `crates/filter/**`, `crates/pipeline/**` or `web/`.

## Size
≲ 300 changed lines.

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and the api tests pass, with `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`.
- The openapi drift check passes.
- The soak tools tests pass: `python3 -I -m unittest discover -s docs/soak/tools -p 'test_*.py'`.
- Run `/security-review`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally: the seven-day usage warning is escalating. Push once per round of fixes.
