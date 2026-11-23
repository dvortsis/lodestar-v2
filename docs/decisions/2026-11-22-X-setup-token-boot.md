# X-setup-token-boot decisions

From the UI review of 2026-11-13 (I13, U3, I14), placed by `docs/decisions/2026-11-14-runner-ui-review-placement.md`. Architecture §10.10.10 (the setup screens) and §10.3 (navigation), §12 (the setup token), §10.1 (first-load budget).

## The setup token is checked on Continue (I13, U3)

- **A new open route, `POST /setup/token`,** takes `{ "token": … }` and answers `200 { "ok": true }` when it's the pending setup token. Otherwise it answers as `POST /setup` does for the token: `401 bad_token`, `409 setup_done`, `429 rate_limited`. It sets no cookie, claims nothing and spends nothing; `/setup` checks the token again.
  - **Why a route, not a flag on `/setup`:** a flag would make the request that claims the instance also the one that only asks, and `SetupRequest` requires a password. A separate request with its own type keeps both simple.
- **It shares setup's attempt limit, never a second counter** (the slice's `/security-review` note). `Auth::check_setup_token` and `Auth::setup_with_notice` both start with one private check, `match_setup_token`: take an attempt from the per-address login bucket (the one `/setup`, `/login` and the fresh-password check use: a burst of 5, then one every 12 s), then compare the trimmed, upper-cased token with the pending one in constant time. A check costs an attempt whether it passes or not, so checks add no guesses: five checks, five setups, or any mix, empty the same bucket.
- **It reveals nothing `/setup` didn't already.** `/setup` already answered `bad_token` for a wrong token before it looked at the password, under the same limit. The check runs no Argon2 and no disk write, as `/setup`'s token refusal doesn't, and uses the same constant-time compare. The error text is the same fixed string; the web UI shows only its own messages.
- **Logging:** a refused check logs `auth.sign_in_failed` with `action: setup` and its reason, and an exhausted limit `auth.rate_limited`, as a refused setup does. A check that passes logs nothing, since nothing changed. No new events, so the logs catalogue is unchanged.
- **The form asks once per token.** The setup form remembers the token the server last said is right, so Back and Continue don't spend another attempt. A happy first run now spends two attempts (the check, then setup) of the five.
- **The form's state machine ends in `success`,** so after a check that passes the setup form starts a fresh one for the password step. The Continue button shows the same busy and locked states as the submit; a 429 locks it with the countdown on the token step; `setup_done` goes to sign-in, as the submit does.
- **`checkSetupToken` uses a plain `fetch`, not the API client.** The client counts any success as proof of a live session (`sessionOk`), which would mark the browser signed in. It reads only the code and `Retry-After`, never the server's text, and builds the `ApiError` itself: importing the client's `errorFromResponse` would add an export to the shared client chunk, for a few lines.

## One session check at boot (I14)

- **The root layout's `load` asks `GET /auth` once, on the first load of any page that isn't bare** (sign-in, setup, welcome). Setup required goes to `/setup`. Signed out, or signed in with the Legal Notice pending, goes to `/login?next=<the path and query>`; the sign-in page shows the notice when it's pending. Signed in goes on. Later navigations don't ask again; the client's 401 handler covers a session that ends later, as before.
- **Nothing renders meanwhile.** The static boot screen from `app.html` stays up for that one round trip. Only an explicit answer moves the browser: an error status, a body that isn't an auth status, a network failure, or no answer within 5 s leaves the page to load as before, and its own requests still meet the 401 handler.
- **It leaves with `location.replace`, not kit's `redirect`** (a refinement for the runner to flag, §2.19). The redirect is a full load of the sign-in or setup page in place of the current entry, so Back doesn't return to a page that would send it on again (as X-first-run-search-gate decided). Importing kit's `redirect` into the root layout grew the shared runtime chunk and pushed seven budgeted entries over; a full load costs a signed-out visitor one more document request, from cache.
- **It replaces X-first-run-search-gate's check on the search page.** `features/auth/first-check.ts` and `first-run.ts`, and the search page's call, are removed: the boot check covers every route, including `/search`, before it renders, so the search page's own check after mount only repeated the request. Its tests move to `web/src/routes/layout.test.ts`. `loginUrl` is inlined for the same reason as above: importing `next.ts` adds a chunk to every first load.
- **Size (§10.1):** the Search route's first load measures 44.98 kB against its 45 kB budget (44.95 kB before). No budget changed.

## Files outside the slice's owns

- `crates/api/src/auth/http.rs` (the route), `crates/api/src/dto/auth.rs` (`SetupTokenRequest`, with a redacting `Debug`), `crates/api/src/openapi.rs` (the open-path list), and their tests in `crates/api/tests/api.rs` and `openapi.rs`.
- The generated `web/src/lib/api/openapi.json` and `schema.d.ts`.
- `web/src/routes/search/+page.svelte` and the removed `first-check.ts`, `first-run.ts`, `first-run.test.ts` (above).
- Tests and comments for this flow: `web/src/lib/features/auth/forms.test.ts`, `token.test.ts`, the new `web/e2e/setup-token-boot.routes.test.ts`, comments in `web/e2e/first-run.routes.test.ts`, `harness.test.ts` and `mock/openapi-mock.ts`, and a line in `web/src/lib/features/auth/README.md`.
