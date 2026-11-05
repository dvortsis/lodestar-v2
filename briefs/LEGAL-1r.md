# LEGAL-1r: the Legal Notice on release/v0.5.1's setup screen, for rc.4
**Model:** Opus (auth and setup path; never helper work). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05.

## Why
The owner decided (in chat, 2026-11-05 ~01:15Z; recorded as `docs/owner-decisions.md` §2.40 in the runner's next PR) that rc.4 = the memory fixes plus a small Legal Notice built for the release branch's existing setup screen, so v0.5.1, the first public release, opens with the notice. The fuller LEGAL-1 (main's setup wizard step) and rule packs on by default (RP-DEF, RP-UC) ship in v0.5.2. `release/v0.5.1` split from `main` on 2026-10-30: it has the older setup screen (`crates/api/tests/golden/setup_*.txt`, `docs/user/images/setup-screen.png`), no wizard, and 5 fixed packs.

## Read first
- On `main` (after #447 merges): `docs/owner-decisions.md` §2.38 (the Legal Notice; counsel review is advisory, never a gate), `docs/decisions/2026-11-04-X-legal-docs.md` ("Slices": LEGAL-1's behaviour and tests), `docs/legal/legal-notice.md`.
- On `release/v0.5.1`: `crates/api/src/auth.rs` and `crates/api/src/auth/**` (setup and login), the setup screen in `web/src/lib/features/auth/**`, the settings store, About in `web/src/lib/features/settings/about/`.

## Do (on the release branch's code)
1. **Branch** `release/rc4-legal` from `origin/release/v0.5.1`.
2. **Server:**
   - `legal::NOTICE_VERSION` (a const, starting at 1) and the notice text embedded in the binary from a release copy of `docs/legal/legal-notice.md`.
   - `POST /setup` takes the accepted notice version; missing or stale → `409 notice_required` (a typed API error with a golden). On success, store `legal.notice_version` in settings in the same transaction as the admin password.
   - **Login gate:** when stored `legal.notice_version` is missing or below `NOTICE_VERSION`, login succeeds into a state that shows the notice and requires acceptance before anything else (an install that finished setup on rc.1–rc.3 has none, so it sees the notice once). Accepting records the version. An editorial change that leaves `NOTICE_VERSION` alone never re-shows it. Design the narrowest server-enforced gate on the release code (e.g. a `notice_required` error from authenticated routes until accepted, plus one accept endpoint); say what you chose in the decisions file. It must not weaken any auth rule, rate limit or lock-out.
   - Regenerate `openapi.json` and `schema.d.ts` with the repo's tooling.
3. **UI:** the setup screen shows the notice (rendered Markdown, scrollable) and a required checkbox before the password can be submitted; the login gate screen shows the same and an Accept button; About shows "Legal" with the notice. en and zh message keys for the labels only; the notice body isn't translated. Tokens only, no hex; keep the size budget.
4. **Notice text accuracy for v0.5.1:** the notice was written against `main`. Check every sentence against the release build (no packs on by default, no unauthorised-copy pack, no source plugins registered, crawler starts at program start). List any untrue sentence in your PR and message the runner; don't rewrite legal text yourself beyond deleting a clause that describes a feature v0.5.1 lacks, and say which.
5. **Tests:** 409 on a missing or stale version; setup succeeds with the current version and stores it; the notice is re-shown after a version bump; not re-shown after an editorial-only change; an install set up before LEGAL-1r (no stored version) sees the notice at login and can accept it; the gate blocks authenticated routes until accepted; auth lock-out and rate limits unchanged. Web: vitest for the checkbox and gate; Playwright for the setup flow if the release branch has the e2e harness. No fixed sleeps.
6. **Docs:** the user guide's setup chapter (screenshots per the `user-docs` skill if the harness exists on release), the release notes draft on the release branch if one exists, and `docs/decisions/2026-11-05-LEGAL-1r.md`.

## Own
The `release/rc4-legal` branch: `crates/api/src/auth.rs`, `crates/api/src/auth/**`, a new `crates/api/src/legal.rs` (or equivalent), the setup/login screens under `web/src/lib/features/auth/**`, About's legal entry, their message keys, goldens, `openapi.json`/`schema.d.ts`, the setup docs and the decisions file. **Don't touch** what R-rc4-cut owns (`crates/lodestar/src/thp.rs`, memstats, `crates/log/src/url.rs`, `crates/lodestar/Cargo.toml`, `.github/workflows/release.yml`), and never edit `main`.

## Done when
On the branch: `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; in `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, and Playwright for any spec you touch (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium-1243/chrome-linux64/chrome`). Then `/security-review` on the diff against `origin/release/v0.5.1`.

Open the PR **against `release/v0.5.1`**, titled "LEGAL-1r: Legal Notice at setup and login for v0.5.1 (rc.4)", label `full-ci`. Follow `.claude/skills/steward/SKILL.md` and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head, the gate design you chose and any notice sentences untrue for v0.5.1, using the `send_message` tool of the Claude Code Remote MCP server. Never merge; never tag. Push once per round of fixes.
