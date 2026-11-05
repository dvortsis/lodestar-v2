# SRC-10c-2: Internet Archive plugin follow-ups (before its Q2 hold lifts)
**Model:** Sonnet (following SRC-10c's own code). A fresh Opus reviewer follows. Runner brief, 2026-11-05.

## Why
SRC-10c (#411, merged 9571227e) stays unregistered until its Q2 terms check is recorded. Its delta review left five follow-ups.

## Do (in `crates/sources-plugins/src/archive_org.rs` and its tests)
1. A test for the one-second resume advance: a poll of 20 full pages all dated the same second saves the resume point as `from + 1`. (Mutation: replacing `t.max(from + 1)` with `t` must fail it.)
2. The decisions file says plainly that when 10,000 or more items share one second, the rows past the 10,000th in that second are skipped, and that this is inherent (the search API defines no order within a second).
3. After a run of resume polls catches up and clears the key, the next lookback poll must not start earlier than where the resume run ended (so it doesn't re-read a bulk and set the resume point back inside it for up to ~6 h). Keep the 6 h lookback for late arrivals otherwise. Test it.
4. Skipped items (no `btih`, bad hash) should still advance the window: have the plugin track the newest date among skipped rows too (from the parser's page summary if it reports one; if `crates/sources-feeds/src/archive_org.rs` must change to expose it, keep that change minimal and say so). Test a page of all-skipped rows.
5. The decisions file notes that while every poll is full (sustained intake ≥ 10,000/h or a block of future-dated items) the lookback is off, so late-arriving torrents in that window aren't re-read.
Keep the bold Q2 HOLD line. No registration.

## Own
`crates/sources-plugins/src/archive_org.rs`, `crates/sources-plugins/tests/archive_org.rs`, `crates/sources-plugins/fixtures/archive_org/**`, `docs/decisions/2026-11-04-SRC-10c.md` (append a follow-up section), and `crates/sources-feeds/src/archive_org.rs` only for step 4 if needed.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar-sources-plugins -p lodestar-sources-feeds`. PR "SRC-10c-2: …" against `main` from `slice/SRC-10c-2`. Follow `.claude/skills/steward/SKILL.md`; message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) via the claude-code-remote `send_message` tool. Never merge. Push once per round. No fixed sleeps.
