# lib/api

Typed API access. `openapi.json` is written by the Rust generator; `npm run api:types` turns it into `schema.d.ts` (types only). Use only the generated types; a missing field is a request to the main session, never a local type. Generated files are exempt from the lint bans.

## client.ts

`api.get|post|put|patch|delete(path, { query, path, body, signal })` is typed from `schema.d.ts`'s `paths` and returns the 200 body. Base `/api/v1`, same-origin cookies, writes always `Content-Type: application/json`.

- Errors are `ApiError { status, code, message, field?, start?, end?, retryAfter? }`; a non-envelope body is `http_<status>`, a network failure `network`. `isAbort(e)` recognises a cancelled or superseded request: ignore it.
- A 401 `login_required` calls the handler registered with `onLoginRequired(next => …)` once (concurrent 401s are de-duplicated until a request succeeds); never from `/login`, `/setup`, `/auth` or `/logout`, nor while on `/login` or `/setup`.
- `latest()` is a request slot: `slot.run(signal => api.get(…, { signal }))` aborts the previous run and drops a response that resolves after a newer run started.
- Cursors pass through untouched; `mustRestartPaging(e)` is true for `cursor_expired` / `bad_cursor`.
