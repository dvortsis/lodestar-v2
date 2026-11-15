# X-first-run-search-gate decisions

From the tester's run on `main` (2026-11-15): U1, A1 and a lead on two saves. This file says only what the code now does.

## The search page checks sign-in after mount (U1)

- **Why it was missed.** The bare search page makes no request until the first interaction or 30 s (§10.1's first-load budget). Settings and Insights fetch at once, so their 401 or 409 reaches the client's handler and redirects. Search had nothing to answer: a fresh instance showed no setup, and a pending Legal Notice showed no prompt.
- **One `GET /auth` after mount** (`web/src/lib/features/auth/first-check.ts`). The check has no imports: a signed-in visitor's load gains one small JSON response and about 0.15 kB of route JS (size-limit passes). The module that navigates (`first-run.ts`) loads only when the browser has to leave.
  - Setup required: go to `/setup`, as the client's 401 path does.
  - Signed out, or the notice pending: go to `/login?next=<this page>`. The sign-in page shows the notice when it's pending, and returns to the page afterwards.
  - Signed in with the notice accepted, or the check fails: stay put. A failed check isn't treated as signed out; the page's own requests still meet the client's handler.
- **The redirect replaces the history entry** (a UX refinement for the runner to flag, §2.19). Back from setup or sign-in doesn't return to a search page that would redirect again. The client's mid-use 401 path still pushes, as before.
- **Signed-out visitors are redirected too,** not only setup and the notice. It's the same check, and a signed-out search page could do nothing anyway.
- **`vite preview` answers `/api/…` with a short JSON 404** (`web/vite.config.ts`), as the real server does for an unknown route, instead of the SPA page with a 404 status. Lighthouse runs against preview and was counting that 4 kB page as part of the search page's load, which the real server never sends. Lighthouse's budgets are unchanged; locally, total byte weight is 100.3 kB against the 102.4 kB limit. Playwright mocks `/api` in the browser, so its requests never reach preview.
- The e2e mock now answers `GET /auth` as signed in by default, since every spec that opens `/search` now makes the check. Specs that need another state override it, as before. Sign-in specs that land on /search now answer signed in once the password is taken, as the server does.

## Settings → Notifications labels (A1): not reproduced

- Checked on a real server built from `main` (and nothing under `web/src/lib/features/settings` or `web/src/lib/ui/fields` changed since the tester's commit). Signed in, every disclosure open, with axe (WCAG 2.2 AA) and Chromium's computed names.
- The screen has 5 switches, matching the tester's count: ntfy priority's and Mail port's Automatic, and Allow private ntfy, mail server and webhook. It also has 12 route checkboxes. Each is an `<input>` inside its own `<label>`, with no `aria-labelledby` and no dangling reference. axe reported nothing.
- No code change. If the tester's tool reports it again, the runner asks which tool and state.
- **Seen while checking:** an Automatic switch that is off is named only "Automatic". The field's name comes from its header label's `for`, which points at the switch only while it's on. This isn't an empty name. It's left for a follow-up if the runner wants it.

## Language and Notifications saves: not a defect

- Checked on the same real server, reloading after each change:
  - Language is a preference: it applies at once with one `PATCH`, with no Save button (§10.10.1), and survives a reload.
  - A Notifications route is kept by Save changes and survives a reload.
  - Leaving with an unsaved change asks first ("Leave without saving?").
- **What can look like a lost save:** turning Language's Automatic off doesn't change the value until a language is picked (the select still reads Automatic), so nothing is saved and no save bar appears. That is the form's documented behaviour for `x-auto` fields.
- The e2e spec covers both saves.
