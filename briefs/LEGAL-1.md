# LEGAL-1: the Legal Notice on main (server gate ported from LEGAL-1r, plus the wizard step)
**Model:** Opus (auth and setup path; never helper work). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05T04:40Z.

## Why
v0.5.1 opens with the Legal Notice via LEGAL-1r (#457, on `release/v0.5.1` only, merge `5b840813`). `main` has no notice yet, so v0.5.2 would ship without it. This slice brings LEGAL-1r's behaviour to `main` and puts the notice in main's setup wizard as its own step (owner §2.38, §2.40; `docs/decisions/2026-11-04-X-legal-docs.md` "Slices"). LEGAL-2 (crawler paused until setup) and SRC-15b (consent screens) follow it.

## Read first
- LEGAL-1r on the release branch: `git show -m --first-parent 5b840813 --stat`, then the diff. Read `docs/decisions/2026-11-05-LEGAL-1r.md` there: the gate design, NOTICE_VERSION 2 and its reason, `auth.json` storage, 409 `notice_required` on session routes, `POST /legal/accept`, `GET /legal/notice` (open), API keys and feed tokens not gated, the rate limit unchanged, a malformed version failing closed, and the release-copy corrections.
- On `main`: `docs/owner-decisions.md` §2.38 and §2.40, `docs/decisions/2026-11-04-X-legal-docs.md`, `docs/legal/legal-notice.md`, `crates/api/src/auth.rs` and `auth/**`, `web/src/lib/features/wizard/**` (`steps.ts`, `WizardPage.svelte`, `messages/`), and About in `web/src/lib/features/settings/about/`.

## Do (branch `slice/LEGAL-1` from `origin/main`)
1. **Server: port LEGAL-1r's gate to main's auth code.** Port the behaviour, not the commits. Main's auth has moved on since the cut. In particular, SRC-17a (#462) inserts `Access::Session` in `auth/http.rs`, and SRC-11b (#459) adds token-authenticated webhook routes. Bring `main` in once those merge. The webhook and feed-token routes stay ungated, like API keys. Keep `NOTICE_VERSION` at 2 and keep the stored field the same as the release (`auth.json` `notice_version`), so an install upgraded from v0.5.1 to v0.5.2 isn't asked again. The notice text is embedded from main's `docs/legal/legal-notice.md` copy, and a test checks it matches.
   - **Decide, and record:** main's notice describes features v0.5.1 lacks (packs on by default, the unauthorised-copy pack once RP-UC lands). If main's text differs materially from what a v0.5.1 user accepted at version 2, ask the runner whether v0.5.2 bumps to 3. Don't bump on your own.
2. **Wizard:** a `LegalStep.svelte` as the wizard's first step. It shows the rendered Markdown (nodes, never `{@html}`, `https:` links only, as LEGAL-1r does), scrollable, with a required checkbox, and `POST /setup` sends `notice_version`. Register it with the one-line step entry in `steps.ts`. Message keys go in `wizard/messages` (en, zh labels only; the body isn't translated). The login gate and About → Legal come from LEGAL-1r's web code, adapted to main.
3. **openapi.json and schema.d.ts:** regenerate them with the repo's tooling, never by hand.
4. **Tests:** every LEGAL-1r test, ported:
   - 409 on a missing or stale version; setup stores the version;
   - a version bump re-shows the notice; an editorial-only change doesn't;
   - an install set up before the notice sees it once;
   - API keys, feed tokens and webhooks aren't gated;
   - the rate limit and lock-out are unchanged; proxy sessions and streams are gated; a malformed version fails closed.
   - Web: vitest for the step and the gate. Playwright for the wizard flow (main has the e2e harness), with the e2e mock and fixtures updated.
   - No fixed sleeps.
5. **Docs:** the user guide's setup chapter on main, with screenshots per the `user-docs` skill if the shot harness covers the wizard; the decisions file `docs/decisions/2026-11-05-LEGAL-1.md`; and a line in main's `docs/releases/v0.5.2.md` draft if one exists.

## Own
`crates/api/src/auth.rs`, `crates/api/src/auth/**`, `crates/api/src/legal.rs` (+ `legal/`), `crates/api/tests/legal.rs` and its goldens, `web/src/lib/features/wizard/LegalStep.svelte` (+ test), the one-line `steps.ts` registration, the wizard message keys, the login gate under `web/src/lib/features/auth/**`, About's legal entry, `openapi.json` and `schema.d.ts`, the setup-chapter docs, and the decisions file. You may also make mechanical `notice_version` additions to tests that post `/setup`, and changes to the e2e mock and fixtures; list them in the PR.
**Don't touch:** `crates/lodestar/src/services.rs` (LEGAL-2), the pack files, `release/v0.5.1`.

## Done when
On the branch: `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`. In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, and Playwright for the specs you touch (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium-1243/chrome-linux64/chrome`). Then `/security-review` on the diff.

Open the PR against `main` with the title "LEGAL-1: Legal Notice as a setup-wizard step on main" and the label `full-ci`. Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head, and the answer on the version question. Use the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round of fixes.
