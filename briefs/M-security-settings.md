# M-security-settings: the Settings → Security screen
**Model:** Opus: authentication UI (passwords, sessions, API keys, proxy auth). Runner brief, 2026-10-31. M5 backlog (`runner/slices.yaml`). `/security-review` is required.

## Read first
- CLAUDE.md's UI rules, `runner/standards.md`, `docs/ui-glossary.md`.
- `docs/architecture.md` §10.10.1–10.10.5, **§10.10.7** (risky changes: "Re-enter your password in the dialog (B12's confirmation). Changing the password needs the current one."), **§10.10.8's Security row** ("password, sessions, API keys and feed tokens, trusted-proxy auth, secure cookies (read-only)"), §10.10.11 (accessibility, performance, tests) and §12 (security).
- `docs/decisions/2026-10-22-M-auth.md`, `2026-10-23-B12.md`, `-S-keys.md`, `2026-10-30-F-auth.md`, `-F-auth-lock.md`, plus `-S-settings-2b.md` (how hand-built screens use the engine: `GroupedForm`, `onReveal`, `href`, lazy parts) and the engine README (`web/src/lib/features/settings/form/**`).
- The API in `web/src/lib/api/schema.d.ts`: `/auth`, `/auth/proxy`, `/password`, `/sessions/revoke-others`, `/keys`, `/keys/{id}`, `/keys/{id}/regenerate`, `/keys/{id}/revoke`. S-keys may already have built an API-key UI elsewhere; reuse it, don't duplicate it.

## Do
1. **A Security screen** in `web/src/lib/features/settings/security/**`, registered through the screen registry like the screens2 screens. Generate whatever the schema covers. Hand-build only:
   - **Change password:** current password, new password and confirm, with the server's strength rules shown before submit. On success, say whether other sessions were signed out.
   - **Sessions:** the current session and **Sign out other sessions** (`/sessions/revoke-others`) behind B12's password confirmation.
   - **API keys and feed tokens:** list, create, reveal-once, regenerate and revoke, each risky action behind the password confirmation. A revealed key is shown once, with Copy, and never stored in the client: not in localStorage, sessionStorage, the leave-guard draft, URLs, or the console. If S-keys already has this UI, mount or link it.
   - **Auth mode and trusted-proxy header** (`/auth`, `/auth/proxy`), behind the password confirmation. Explain plainly what trusting a proxy header means, and recommend the safe choice (§2.14 users).
   - **Secure cookies:** read-only status.
2. **The password confirmation** is one shared dialog using B12's flow:
   - focus starts in the password field;
   - a wrong password shows the server's error as text;
   - the lock-out state (F-auth-lock) is shown truthfully;
   - Cancel sends nothing;
   - the password is never kept after the request.
3. **States:** loading, error, env-locked or read-only (for example, auth set by env), and the leave guard, all through the engine. A signed-out or expired session mid-flow goes to sign-in without losing the user's place where the engine already supports that.
4. **Sizes:** the Security first view stays within the 45 KB Settings route budget (group map plus its own map, as #277 set up), with **≥ 0.3 kB of headroom**. Load the dialogs and the key manager on demand. Add new entries at measured + 10%; never raise an existing limit. If it can't fit, stop and report the numbers.
5. **i18n:** `settings_security_*` keys in en and zh, regenerated with the repo's generator.
6. **Tests:**
   - vitest: each risky action needs the password confirmation and sends nothing on Cancel; a wrong password shows the error; a key is shown once and is absent from storage and drafts afterwards; the lock-out state is shown; and the password is cleared after submit.
   - Playwright: the screen renders, keyboard-only, with axe, against the mock; and a password change plus sign-out-others flow.
   - Condition waits only, no fixed sleeps.
   - If Settings baselines change for the new nav entry, use the `web/e2e/.update-snapshots` marker and check that only Settings images changed.
7. **Decisions file:** `docs/decisions/<date>-M-security-settings.md`.

## Own
`web/src/lib/features/settings/security/**`, its registry entry (one line, as the screens2 screens did), its message keys and regenerated maps, the Settings mock fixtures for the auth endpoints, `web/e2e/settings-security.*`, its size-limit entries and your decisions file. **Don't edit** the engine (`form/**`), `shell/**`, `screens2/**`, `lib/state/**`, `lib/ui/**` or `lib/api/**`. If the engine lacks something, stop and tell the runner.

## Size
≲ 1,200 changed lines. If it grows past that, split it: password and sessions first, then keys and proxy.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build`, then `npx size-limit`
- `npx playwright test` for the Settings specs, 3× for anything new. Chromium is at /opt/pw-browsers; never run `playwright install`. If the bundled headless shell doesn't match, use an untracked config with `executablePath: '/opt/pw-browsers/chromium'` and never commit it.

Run `/security-review` on your diff. Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size deltas. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
