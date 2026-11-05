# LEGAL-2: the crawler stays paused on a fresh data directory until setup completes
**Model:** Opus (startup and setup path, crawler lifecycle). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05T12:35Z. Ships in v0.5.2.

## Why
LEGAL-1 (#472, merged ad921690) put the Legal Notice into main's setup wizard. The crawler still starts at program start, before anyone has set the password or read the notice: `Services::start` builds the pipeline with `paused` from settings, which defaults to off. The runner adopted LEGAL-2 under §2.26 (`docs/decisions/2026-11-04-X-legal-docs.md` line ~50, and `docs/owner-decisions.md` §2.38/§2.40): **on a fresh data directory the crawler starts paused until setup completes, so nobody's address joins the DHT before anyone has read the notice. Existing installs are unaffected.**

## Read first
- `docs/decisions/2026-11-04-X-legal-docs.md` and `docs/decisions/2026-11-05-LEGAL-1.md`.
- `crates/lodestar/src/services.rs` (`Services::start`, the `paused` setting, and how the crawler, the sources and the import workers start).
- `crates/api/src/auth.rs` and `crates/api/src/legal.rs` (what "setup complete" means: the password is set and `notice_version` is accepted).
- The pipeline's pause control (`crates/pipeline/src/control.rs`) and how `PATCH /settings` pause and resume reach it.

## Do (branch `slice/LEGAL-2` from `origin/main`)
1. **What counts as fresh.** Define a "fresh data directory" precisely and fail closed. A store created by this run with setup not complete is fresh. An existing install that completed setup before LEGAL-2, with a password set and with or without `notice_version`, is **not** paused by this, even if the notice is re-shown at login. Record the rule in the decisions file.
2. **Pause until setup.** On a fresh directory, everything that announces or contacts the network on the crawler's behalf starts paused until `POST /setup` succeeds: the DHT crawler, metadata fetch, and the sources that dial out. Name exactly what you pause in the decisions file. A local import with no network is out of scope; say so either way. Then:
   - **when setup succeeds,** the crawler resumes on its own, unless the user has explicitly paused it;
   - **the user's own pause setting** still wins after setup;
   - **after a restart** before setup completes, the crawler is still paused.
3. **Status.** The status page and `GET /status` say why the crawler is paused ("waiting for setup"), with an en and zh key. Don't add any UI beyond that line.
4. **Tests:**
   - a fresh store doesn't bind or announce on the DHT socket before setup. Assert on the crawler's state, not on sockets, if the harness can't see them;
   - it resumes after `POST /setup`;
   - an existing set-up store starts unpaused;
   - a user-paused install stays paused after setup;
   - a restart mid-setup keeps it paused.

   Waits are on conditions with a deadline of at least 10 s, never fixed sleeps.
5. **Docs:** a sentence in the user guide's setup chapter, per the `user-docs` skill: nothing connects to the network until setup is done. Also `docs/decisions/2026-11-05-LEGAL-2.md`.

## Own
`crates/lodestar/src/services.rs`, plus the status line, its keys, tests in `crates/lodestar/tests/`, the guide sentence and the decisions file. **Overlaps:**
- X-source-adult-2 (running) adds one `adult_sources` field in `services.rs`.
- SRC-11c (#490) adds webhook wiring lines there.

Keep your hunks to the start-up path. Whichever PR merges later brings `main` in.

## Done when
- `cargo fmt --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- `cargo test --workspace`;
- web checks if you touch `web/`;
- `/security-review` on the diff.

Then open the PR titled "LEGAL-2: Crawler paused on a fresh data directory until setup completes", with the label `full-ci`. Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Never merge. Push once per round.
