# X-win-restart-followups-2 (Opus): the follow-ups from #649's review

Assigned by the runner, session_01EYhgh4JX3AmHzKXPjYCoUg, on 2026-11-09. It continues in the session that built X-lease-test-polish and X-win-restart-followups (#649 merged as da15ccab). Follow `.claude/skills/steward/SKILL.md`. Branch `slice/X-win-restart-followups-2` from main da15ccab or later. PR title `X-win-restart-followups-2: <title>`. A fresh Opus review follows.

## Read first
- The #649 review: runner-state `reviews/rv-649-8d4ac5f6.md` (MERGE; findings 1, 2, 4, 5 are this slice).
- `docs/decisions/` for X-win-restart-followups and PK-win-restart; `runner/standards.md`.

## Owns
- `crates/lodestar/src/system/restart.rs` and the portable `Start Lodestar.cmd`;
- `crates/companions/src/manager.rs` (the lock open at ~991) and its tests;
- `crates/search/src/rebuild.rs` (doc text only);
- `docs/decisions/2026-11-09-X-win-restart-followups-2.md`.
- **Not** `crates/lodestar/src/update/**`: PK-win-upd-3 (#644) is open there. The same pattern in `update/layout.rs:239` goes in a later slice once #644 merges; name it in the decisions file.

## Build
1. **A relaunched copy that fails at start must not vanish** (review finding 1). After exit 80 the launcher ends without `pause`, so a failure at start in the relaunched copy (a held index lock, a held port, a bad config) closes the window before anyone reads it. Make it visible: preferably, a relaunched portable copy that fails at start waits for a key on `CONIN$` (only when it was relaunched, only on a failure at start, never under the service), with the error and the log path printed. Test what can be tested on Linux; the Windows path needs a Windows test in `tests/windows_service.rs` style if feasible, else say so in the decisions file.
2. **The companions lock** (finding 2): `Manager::open` answering Busy after a quick restart on Windows silently turns companions off for the run. Reuse the bounded busy-lock retry pattern from `rebuild.rs` (10 s at 100 ms on Windows, one attempt elsewhere, I/O errors never retried, a fake clock in tests). Don't add a dependency from companions on search: a tiny local helper, or a shared one in an existing common crate if one fits without a new dependency edge. No fixed sleeps.
3. **Nits:** `EXIT_RELAUNCHED` refers to `update::control::EXIT_RESTART` (a read-only reference) or a test pins them equal; reword `rebuild.rs:553`'s doc (tantivy 0.27 maps every try-lock error to busy, so only the open is "never retried").

## Checks
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test` on the touched crates; Windows CI green before "ready".
