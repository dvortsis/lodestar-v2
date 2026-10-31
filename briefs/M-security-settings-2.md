# M-security-settings-2: API keys, feed tokens, auth mode and the trusted-proxy header
**Model:** Opus: authentication UI. Runner brief, 2026-10-31. Part 2 of the Security screen; part 1 (#285, merged da79ee05) built the screen, the shared password-confirmation dialog, Change password, Sessions and secure cookies. `/security-review` is required.

## Read first
- The part 1 brief, `git show origin/runner-state:briefs/M-security-settings.md` (the full spec, still binding for this part), and `docs/decisions/2026-10-31-M-security-settings.md` (what part 1 built and decided).
- `docs/architecture.md` §10.10.7, §10.10.8 (the Security row), §10.10.11 and §12; `docs/decisions/2026-10-23-B12.md`, `-S-keys.md`, `2026-10-30-F-auth.md`, `-F-auth-lock.md`.
- The API in `web/src/lib/api/schema.d.ts`: `/keys`, `/keys/{id}`, `/keys/{id}/regenerate`, `/keys/{id}/revoke`, `/auth`, `/auth/proxy`. Check whether S-keys already built an API-key UI, and reuse or mount it; don't duplicate it.
- The code in `web/src/lib/features/settings/security/**`.

## Do
1. **API keys and feed tokens:** list, create, reveal once, regenerate and revoke, with each risky action behind part 1's password dialog. A revealed key is shown once, with Copy, and is never stored client-side: not in localStorage, sessionStorage, the leave-guard draft, URLs, the console, or component state after the user closes the reveal.
2. **Auth mode and the trusted-proxy header** (`/auth`, `/auth/proxy`), behind the dialog:
   - explain plainly what trusting a proxy header means, and recommend the safe choice (§2.14 users);
   - an env-set value (`LODESTAR_PROXY_AUTH`) is shown as locked, with its source;
   - turning proxy trust **on** is a security-loosening action for that install: make the warning explicit, and require the password dialog.
3. **Carried from #285's review:**
   - (a) move part 1's restated leave condition into a pure `securityLeave()` in `security/`, with a table test that imports the engine's `leaveDecision` from `../form/leave` and asserts both agree over dirty × bypass × type × from/to;
   - (b) in `ChangePassword.svelte` (~135-143), add `.catch(() => location.assign(to.href))` on the Discard/Keep `ask()`.
4. **States:** loading, error, env-locked, the leave guard, and an expired session mid-flow, as part 1 does.
5. **Sizes:** the Security first view stays ≤ 45 KB with ≥ 0.3 kB of headroom. Load the key manager and the proxy section on demand. New entries go in at measured + 10%. Never raise an existing limit. **Don't add Settings registry entries** (none are needed; this is the same screen). Other Settings first views must not grow.
6. **i18n:** `settings_security_*` keys in en and zh, regenerated with the repo's generator.
7. **Tests:**
   - vitest:
     - each risky action needs the dialog and sends nothing on Cancel;
     - a key is shown once and is absent from storage, drafts and state afterwards;
     - the env lock;
     - the proxy-on warning;
     - the leave table test.
   - Playwright: keys and proxy flows, keyboard-only, with axe.
   - Condition waits only.
   - No baseline changes expected; if any, stop and tell the runner.
8. **Decisions file:** `docs/decisions/<date>-M-security-settings-2.md`.

## Own
`web/src/lib/features/settings/security/**`, its message keys and regenerated maps, the auth and keys mock fixtures, `web/e2e/settings-security.*`, its size-limit entries and your decisions file. **Don't edit** the engine (`form/**`), `shell/**`, `screens2/**`, `lib/state/**`, `lib/ui/**` or `lib/api/**`. S-logs-1 (#284) is merging main in `.size-limit.mjs`; add your entries in their own lines.

## Size
≲ 1,000 changed lines.

## Done when
These pass in `web/`: `npm run check`, `npm run e2e:typecheck`, `npx vitest run`, `npm run build` then `npx size-limit`, and `npx playwright test` for the Settings specs (3× for new ones). Chromium is at /opt/pw-browsers; never run `playwright install`. Run `/security-review`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size deltas. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
