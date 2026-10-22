# M-auth: Setup-token and login flows (web)
**Model:** Opus (security-sensitive UI state machines; owner policy, CLAUDE.md). **Branch:** `slice/M-auth`.

**Spec:**
- `runner/standards.md`.
- `docs/architecture.md`:
  - §12 (auth: the one-time setup token printed to the logs, an argon2 admin password of 10+ characters, session cookies, rate limiting);
  - §10.10 and §10.10.10 (first-run wizard step 1, setup token);
  - §10.3 (routes);
  - §10.13 (accessibility);
  - §10.14 (strings);
  - §10.19 (brand on the sign-in and setup screen);
  - §16's B0/B1/B3 entries and the auth notes (search §16 for "setup token").
- `docs/decisions/`, `docs/ui-glossary.md`, `docs/design-tokens.md`.
- The `user-docs` rules (`.claude/skills/user-docs/SKILL.md`) for any help text longer than a sentence.
- The code:
  - `crates/api/src/auth/{mod.rs,http.rs}` (the endpoints and their errors: `/auth`, `/setup`, `/login`, `/logout`, `/password`; rate limits; `setup_required` on 401);
  - `web/src/lib/api/` (W5's typed client: `onLoginRequired(next, {setupRequired})`, `onSessionOk`, `ApiError`);
  - `web/src/lib/ui/**` (W7ab primitives: fields, buttons, alerts, toasts);
  - `web/src/routes/{setup,login,welcome}`.

## Own
- `web/src/lib/features/auth/**` (the state machines, components and the feature's `messages/{en,zh}.json`).
- `web/src/routes/setup/**`, `web/src/routes/login/**`.
- Tests next to them, plus e2e specs `web/e2e/auth*.test.ts` against the W6 mock.
- **Don't edit** the app shell (`+layout.svelte`, `lib/shell/**`: W9's), the API, or `package.json`.
- Wiring the 401 handler into the shell is a one-line hand-off. Export a function W9 calls, e.g. `installAuthHandlers()`, and say so in the PR.

## Build
1. **The state machines** are explicit, finite and unit-tested: idle → submitting → success or error, with the error kinds the API returns.
   - **Setup:**
     - the token, with a format hint (`XXXX-XXXX-XXXX-XXXX`), a paste-tolerant parser, and where to find it (the logs: Docker `docker logs`, the Unraid log button, the Windows service log), plainly;
     - a new password (10+ characters, a strength hint without blocking a long passphrase), with confirmation;
     - submit, then sign in, then on to `/welcome`.
     - A spent or wrong token gives a clear error. Setup already done redirects to `/login`.
   - **Login:**
     - the password, then submit, then return to `next` (from W5's handler). Validate `next` as a same-origin relative path, or an open redirect is possible.
     - Wrong password and 429 rate limiting show `Retry-After` as a countdown, with no enumeration hints.
     - Logout.
   - **Session expiry** mid-use goes to `/login?next=…` through W5's handler, preserving the current URL. `setup_required` goes to `/setup` instead.
2. **Security:**
   - nothing sensitive in URLs, logs, localStorage or sessionStorage;
   - password fields use `autocomplete` correctly (`new-password` for setup, `current-password` for login);
   - no autofocus traps;
   - the token is never echoed back in errors;
   - the submit button is disabled while submitting, so double-submit can't trigger rate limits;
   - every write goes through W5's client (Content-Type JSON, same-origin).
3. **UI:** W7ab primitives and tokens only (no hex, no arbitrary values); brand per §10.19; full-page layouts usable at 320 px; keyboard-only flows; axe-clean in Riverstone light and dark. Strings live in the feature's message files (en + zh; zh may be a TODO marker per W4's pipeline).
4. **Hand-off for W9:** document the exports the shell needs (`installAuthHandlers`, a `currentSession` rune if useful).

## Tests
- Unit tests for every state-machine transition.
- Component tests for errors and 429 countdowns.
- Playwright (W6 mock): first-run setup happy path; wrong token; login, return to `next`; the open-redirect attempt is refused; expiry mid-session; keyboard-only; axe.
- Run Playwright on a private port (a copied config with `--strictPort`, never port 4173 shared).

## Done when
- In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit` and the auth Playwright specs pass after merging `origin/main`.
- A `docs/decisions/<date>-M-auth.md` file if you made decisions.
- **A PR** titled "M-auth: Setup-token and login flows". Its description gives:
  - the state diagrams (text is fine);
  - the security properties;
  - the W9 hand-off API;
  - deviations, and why.
