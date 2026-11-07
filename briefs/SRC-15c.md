# SRC-15c: Settings → Sources: Account consent dialog, generated config forms, purge counts
**Model:** Sonnet (UI), with `/security-review`, because it handles credential fields and consent text. A fresh Opus reviewer follows. Runner brief, 2026-11-07T03:25Z. It takes the slot X-sources-api-spec freed (#561 merged), within owner §2.42's four slots.

## Spec
- **Decisions:**
  - `docs/decisions/2026-11-07-X-sources-api-spec.md` (#561): the API you build on;
  - `docs/decisions/2026-11-06-SRC-15b.md` (#552): the dialogs, purge flow and size rules you extend;
  - `docs/decisions/2026-11-06-runner-sources-split.md` (#550).
- **The approved texts:** `docs/owner-decisions.md` §2.49 holds the four consent texts, including the Account text with the owner's bounded change. Show the **Account** text **verbatim**.
- **Architecture:** §18.7, §18.8, §10.10.7 and §10.14. Follow the UI rules in CLAUDE.md: tokens, the spacing ramp, Atkinson fonts, no hex.
- **`runner/slices.yaml`:** the `SRC-15c` entry and its notes.

## Do
All of this is in `web/src/lib/features/sources/**`.
1. **The Account consent dialog.**
   - Show the §2.49 Account text verbatim, and extend SRC-15b's strings test so it covers it.
   - Build the credential fields from `credential_spec` in `GET /sources`: label, kind and required.
     - `secret` fields are write-only password inputs. They're never prefilled and never read back.
     - Submit them through `PATCH /sources/{id}/credentials`. Clear them from component state after the submit, whether it succeeds or fails.
     - Keep them out of logs, error text and stores.
   - **The gate stays closed in v0.5.2.** The server refuses enabling an Account source; X-consent-gate-lift opens it after the cut. So:
     - the dialog may save credentials, but must show the server's refusal plainly when enabling is refused;
     - don't add a client-side bypass;
     - match the wording the API now returns. #561 fixed the `patchSource` description.
2. **Generated config forms** from `config_schema`, which the server has already stripped of secrets.
   - Support the types the plugins actually use: string, integer or number with min and max, boolean, enum, and arrays of strings or objects.
   - Anything else shows a plain "edit through the API" note instead of guessing.
   - Validate on the client the same way the server does, and still show the server's errors.
3. **The purge count before commit (§10.10.7).**
   - Call `GET /sources/{id}/purge/preview`: session-only, no password, no body.
   - Show the counts in the purge dialog before the admin types the password. The password is asked only for `POST …/purge`.
   - Handle loading, error and stale states.
4. **Tests:**
   - unit tests for the form generator, including the unsupported types;
   - the strings test;
   - an e2e spec for the Account dialog and the purge preview, with a keyboard path, against the fixture server;
   - no test may print a secret.
   - Run `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit` and `npx playwright test` for the specs you touch. Set `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`, and never run `playwright install`.
5. **Size:** stay within the budgets. Keep the dialogs lazy, using SRC-15b's on-demand entry. Raising a size limit needs the runner's okay, and a smaller UI is preferred.

## Done when
- **Checks:** everything in step 4 passes, and CI is green.
- **Owns:** only `web/src/lib/features/sources/**`, plus your decisions file `docs/decisions/2026-11-07-SRC-15c.md`. Ask the runner before touching `web/.size-limit.mjs`, e2e files outside sources, or the API.
- **Process:** follow `.claude/skills/steward/SKILL.md`, push once per round, and never merge. Title the PR `SRC-15c: Account consent dialog, generated config forms, purge counts`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA once CI is green.
