# lib/api

Typed API access. `openapi.json` is written by the Rust generator; `npm run api:types` turns it into `schema.d.ts` (types only). Use only the generated types; a missing field is a request to the main session, never a local type. Generated files are exempt from the lint bans.

## client.ts

`api.get|post|put|patch|delete(path, { query, path, body, signal })` is typed from `schema.d.ts`'s `paths`. It returns the JSON body of the 200, 201 or 202 response (`void` when there is none). Base `/api/v1`, same-origin cookies, writes always `Content-Type: application/json`. `path` params, `query` and `body` are required exactly where the operation requires them (and `options` with them); GET takes no body.

- Errors are `ApiError { status, code, message, field?, start?, end?, retryAfter?, setupRequired?, assessment? }`. `setupRequired` (a 401) and `assessment` (a 428 from `/filters/apply`) come from the envelope's top level. A non-envelope body is `http_<status>`, a network failure `network`. `isAbort(e)` recognises a cancelled or superseded request, including an abort while reading the body: ignore it.
- `retryAfter` is in seconds, from `Retry-After` as delta-seconds or an HTTP date.
- A 401 `login_required` calls the handler registered with `onLoginRequired((next, { setupRequired }) => …)` once. Concurrent 401s are de-duplicated until a request succeeds. It never fires for `/login`, `/setup`, `/auth` or `/logout`, nor while on `/login` or `/setup`. `setupRequired` says to go to `/setup`.
- `onSessionOk(f)` runs `f` after every successful request except `/auth` and `/logout` (the events client uses it to resume after a login). `sessionOk(path)` is the internal hook that calls it.
- `latest()` is a request slot: `slot.run(signal => api.get(…, { signal }))` aborts the previous run and drops a response that resolves after a newer run started.
- Cursors pass through untouched; `mustRestartPaging(e)` is true for `cursor_expired` / `bad_cursor`.
