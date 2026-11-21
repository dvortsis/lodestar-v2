# X-auth-early-401

The runner's call (UX, §2.26; flagged under §2.19). Opus, web auth plus a one-line server header. Needs /security-review.

## Problem
1. **A 401 that lands before the auth handlers are installed is dropped.**
   - `web/src/lib/api/client.ts` `loginRequired()` returns early while `loginHandler` is unset.
   - The Shell installs the handlers only after a lazy `import()` in `onMount` (`web/src/lib/shell/Shell.svelte`).
   - So a signed-out visitor, or an expired session whose first request fails fast, sees a broken page instead of going to sign-in or setup.
   - Unsaved-draft keeping (X-session-draft-keep) never runs for that 401 either.
2. **GET /auth sends no `Cache-Control: no-store`** (`crates/api/src/auth/http.rs`).
   - The draft-block probe (#999) and checkAuth read it.
   - A cached `logged_in: true` must never be possible.

## Owns
- web/src/lib/api/client.ts and its unit tests (client.test.ts or the existing api test file)
- crates/api/src/auth/http.rs, plus the api auth tests that cover GET /auth
- runner/slices.yaml and runner/dashboard.yaml: this slice's own entry and placement line only
- docs/decisions/<date>-X-auth-early-401.md

Never runs with X-ui-guards, which owns web/src/lib/**.

## Do
1. In client.ts, keep the first unhandled login-required call (path and opts) while no handler is set. Replay it once when `onLoginRequired(handler)` installs a handler.
   - At replay time, apply the same filters: `AUTH_PATHS` and the auth-page check against the location at replay.
   - `sessionOk()` before install clears the pending entry, because a later success proves the session.
   - A notice_required 409 is kept the same way.
   - Bounded: one pending entry, and the first one wins.
   - No change to `loginPending` semantics after install.
2. GET /auth responds with `Cache-Control: no-store`. No other header changes, and OpenAPI is unchanged unless headers are documented there.
3. Tests:
   - a 401 before install redirects once after install;
   - a success before install cancels it;
   - a 401 on an auth path or auth page is not replayed;
   - two early 401s replay once;
   - a 409 notice is replayed with noticeRequired;
   - Rust: GET /auth carries `no-store`, signed in and signed out.
   - No fixed sleeps; waits have deadlines of at least 10 s.
   - Mutations must die: no replay, replay ignoring the filters, and sessionOk not clearing the pending entry.
4. Size: the main chunk must stay within its limit. If it goes over, send the runner the numbers first, and never edit limits yourself.

## Gates
- In web/: check, vitest, build, size-limit, and the auth e2e specs.
- Rust: fmt, clippy -D warnings, cargo test -p lodestar-api.
- The status-connector npm test.
