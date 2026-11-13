# API security surface review
Reviewed at 55e155a9.

## Coverage
- Read in full (non-test source): `crates/api/src/lib.rs`, `auth.rs`, `auth/http.rs`, `auth/guard.rs`, `auth/proxy.rs`, `auth/limit.rs`, `auth/log_events.rs` (event catalogue and `checked`), `extract.rs`, `error.rs`, `request_log.rs`, `keys.rs`, `upload.rs`, `export.rs`, `webhooks.rs`, `torznab.rs`, `torznab/xml.rs`, `openapi.rs`, `settings.rs`, `settings/checks.rs`, `logs.rs`, `system.rs` (routes and handlers), `updates.rs`, `legal.rs`, `crates/lodestar/src/http.rs`, `crates/lodestar/src/web.rs`.
- Read in part: `torznab/query.rs` (parsing and bounds), `settings/send.rs` (to line 430), `logs/stream.rs` (to line 260, session re-check), `jobs.rs` (routes, controls), `ops.rs` (source patch, credentials, purge routes), `alerts.rs` (feed origin), `crates/api/tests/openapi.rs` (route list, guard tests), `crates/lodestar/src/services.rs` (auth load, router layering, serve), `crates/lodestar/src/settings/mod.rs` (secret redaction, `keep_secrets`, `saved_secret`), `crates/lodestar/src/logging/source.rs` and `names.rs` (file-name validation), `crates/lodestar/src/web_build.rs` (`csp`), `crates/store/src/api_keys.rs` (`verify_api_key`), `crates/integrations/src/net.rs` (module docs and function list only).
- Route table: built from every `.route(` in `crates/api/src` and checked against each wrapper (`protected`, `key_or_session`, `feed_token`, `session_only`, none). A script checked that every registered non-GET handler calls `same_origin_json` (directly or through a helper); the two that do not are `upload` (its own stricter check) and `webhooks::receive` (token auth, no cookie).
- Not read: `logs/params.rs` beyond its limits, `settings/usage.rs` beyond its guard, `torznab/categories.rs`, the unit and integration tests other than `tests/openapi.rs`, `dto/**` beyond the `Debug` impls, and the bodies of `crates/integrations` (the SSRF guard and the download-client and notifier adapters: outside this area, examined at design level only). axum's own `serve` source is not on this machine, so the connection-timeout point below is unconfirmed.

### Route table (guards as built)
| Guard | Routes |
|---|---|
| Open | `GET /auth`, `POST /setup` (setup token, login bucket), `POST /login` (login bucket), `POST /logout`, `GET /legal/notice`, `GET /openapi.json`; outside `/api/v1`: `GET /status` (static page), `GET /metrics`, `GET /healthz`, the embedded UI fallback |
| Own token, no session | `POST /webhooks/{integration}` (Basic or Bearer `lsw_` token, per-IP failed-check bucket, per-integration bucket) |
| Session, no notice gate | `POST /legal/accept` |
| API key (scope) or feed token or session | `GET /torznab/api` (TORZNAB), `GET /feeds/{id}` (FEEDS or feed token); keys are GET/HEAD only |
| Session and accepted notice (`auth::protected` or `require_session`) | every other route, including `/export` and `/sources/{id}/purge*` (merged inside `jobs::routes`' `protected`), and `GET /api/status` |
| Session plus fresh password | `PUT /auth/proxy`, `POST /sessions/revoke-others`, `POST /password` (old password), `POST /keys`, `POST /keys/{id}/regenerate`, `POST /webhooks/{integration}/token`, `POST /workers/tokens`, `POST /sources/{id}/purge`, `PATCH /sources/{id}` with `egress`, companion credential reveal |

No route was found reachable without the guard it should have.

## Findings

### APISEC-01 The "Search API" key scope reaches no route
- Severity: important
- Kind: gap
- Confidence: confirmed
- Where: `crates/api/src/keys.rs:50-60`, `crates/api/src/search.rs:191-197`, `crates/api/src/facets.rs:47-48`, `crates/api/src/torrents.rs:49-53`, `web/src/lib/features/settings/security/Keys.svelte:27-32`, `crates/api/tests/openapi.rs:345-451`
- What: `scope::SEARCH` is documented as "the JSON search API, read-only (`/search`, `/parse`, `/facets`, torrent detail), for integrations", and the Security screen offers it ticked by default. No route is wrapped in `key_or_session(.., scope::SEARCH)`: `key_or_session` is used only for `/torznab/api` and `/feeds/{id}`. Search, facets, parse and torrent detail are all behind `auth::protected` (session only). The test `keys_reach_no_existing_route` pins this: a key with every scope gets `401 login_required` on every route but the feed.
- Failure scenario: a user makes an API key with the default "Search API" scope and calls `GET /api/v1/search?q=x` with `X-Api-Key`. The answer is `401 login_required`, the same as no credentials, and nothing says the scope is not served.
- Suggested fix: opt the read-only search routes in (`GET /search`, `/parse`, `GET /facets`, `/torrents/{hash}`, `/torrents/{hash}/files`) with the privacy rules Torznab applies, or remove the scope from the UI and DTO until it does something.
- Tracked?: not found in `runner/slices.yaml` or `docs/build-plan.md`. `docs/decisions/2026-10-23-B12.md` defines the scope; `crates/api/README.md:93` tells authors how to opt a route in.

### APISEC-02 No Host-header check: DNS rebinding reaches the open routes and `/login` from any website
- Severity: important
- Kind: security
- Confidence: likely (the missing check is confirmed; the attack path is reasoned, not run)
- Where: `crates/api/src/extract.rs:194-221`, `crates/lodestar/src/http.rs:68-88`, `crates/api/src/auth/http.rs:301-314`, `crates/lodestar/src/config.rs:131-137`
- What: the only cross-site defence for writes is `Content-Type: application/json` plus "a present `Origin` must equal `Host`". Nothing checks that `Host` names this instance (a grep for `header::HOST` finds only this comparison, the feed-link builder and the worker join URL). A page on `evil.example:8090` whose name is re-pointed at the LAN address of Lodestar is same-origin with it as far as the browser is concerned, so `Origin` and `Host` both read `evil.example:8090` and the check passes. The default bind is `0.0.0.0:8090` on a LAN, which is the usual target of this attack.
- Failure scenario: an admin (or anyone on the LAN) opens a hostile page. After the rebind its script can (1) read `/metrics`, `/healthz`, `/api/v1/auth` and `/api/v1/openapi.json` (version `0.5.0`, enabled source plugins, crawl volumes); (2) `POST /api/v1/login` with JSON and guess passwords at the limiter's rate (5, then one every 12 s, about 7,000 a day) from the victim's address, and on a hit receive a session cookie for `evil.example:8090`, which gives its script the whole admin API; (3) keep the victim address's login bucket empty, so the real admin gets `429` at sign-in and on every password-confirmed change. The admin's own cookie is never sent (different host), so no existing session is taken over, and with a strong password the result is disclosure and lockout only.
- Suggested fix: reject requests whose `Host` is not an IP literal, `localhost`, or a name on a configured allowlist (an `http.allowed_hosts` setting; the trusted-proxy screen is a natural home for the public name) with `421` or `403`, at least on `/api/**`, `/metrics` and `/healthz`.
- Tracked?: not found. `docs/architecture.md:2554` covers rebinding only for outbound requests.

### APISEC-03 Concurrent Argon2 runs are not capped: many client addresses can exhaust memory and the blocking pool
- Severity: important
- Kind: resource
- Confidence: likely (traced in code; the numbers are arithmetic, not a run)
- Where: `crates/api/src/auth.rs:462-478`, `crates/api/src/auth.rs:603-605`, `crates/api/src/auth.rs:728-745`, `crates/api/src/auth/http.rs:308-312`, `crates/lodestar/src/main.rs:227-229`
- What: `/login` (open) takes one token from the caller's per-address bucket and then runs Argon2id (19 MiB, 2 passes) on `spawn_blocking`. Nothing limits how many run at once across addresses; the runtime is built with tokio's default blocking pool (512 threads). The per-address bucket (burst 5) is the only bound, so the ceiling is 512 concurrent hashes, about 9.7 GiB, on the same pool every SQLite read and search uses.
- Failure scenario: a host that can present about 100 or more source addresses (a LAN host claiming unused addresses, any host inside a wide trusted-proxy range, which B12 notes "can set any `X-Forwarded-For`, which gives it a fresh login bucket per request", or a botnet when the port is exposed) sends 5 logins from each at once. The process allocates several GiB for a moment and is OOM-killed on a NAS or a memory-limited container; short of that, the blocking pool is saturated and searches and API reads stall.
- Suggested fix: a small global semaphore (2 to 4 permits) around `hash_password` and `verify_password`, with `429` (or a short wait) when it is full; the `Buckets` map then only has to bound attempts, not memory.
- Tracked?: not found. `docs/decisions/2026-10-23-B12.md` ("Limiters", "Only the proxy may reach Lodestar") records the per-address design and reads the Argon2 run as a cost to the attacker; it is the server that pays it.

### APISEC-04 OpenAPI marks the token routes as session routes, and its "served routes" test compares two hand lists
- Severity: polish
- Kind: docs
- Confidence: confirmed
- Where: `crates/api/src/openapi.rs:28-36`, `crates/api/src/openapi.rs:169-184`, `crates/api/tests/openapi.rs:18-185`, `crates/api/tests/openapi.rs:298-310`, `web/src/lib/api/openapi.json`
- What: `finish()` gives every path outside `OPEN` the `session` cookie requirement. `POST /webhooks/{integration}` takes no session at all (Basic or Bearer webhook token), and `GET /feeds/{id}` also takes an API key or `?token=`; the document says `security: [{session: []}]` for both and defines no key or token scheme. Separately, `it_lists_exactly_the_served_routes` compares the document with the hand-kept `ROUTES` list, not with the router: a route added to `router()` with neither a `utoipa::path` nor a `ROUTES` line is invisible to it and to the "only the open paths answer without a session" test. `/torznab/api` is already such a route (deliberately undocumented, TZ-0).
- Failure scenario: a generated client sends the session cookie to the webhook route and no token; a future route merged without a guard and without a `ROUTES` entry passes every test in this file.
- Suggested fix: add `apiKey` and feed-token schemes and set them on the two routes (and no session on the webhook); derive the served list from the router (axum can list a `Router`'s routes in a test via a recording layer, or walk each module's `routes()`), or at least assert that `ROUTES` plus a short allowlist equals it.
- Tracked?: `docs/decisions/2026-11-11-TZ-0.md:100` records only that Torznab is not in OpenAPI. The rest: not found.

### APISEC-05 `Auth` and its state derive `Debug` over the password hash, the session key and the setup token
- Severity: polish
- Kind: security
- Confidence: confirmed (the derive; no current code prints it)
- Where: `crates/api/src/auth.rs:143-174`, `crates/api/src/auth/http.rs:71-75`, `crates/api/src/lib.rs:79-81`
- What: `State` (`password`, `key: [u8; 32]`), `Auth` (`state`, `setup_token`) and through them `SessionAuth` and `ApiState` all `#[derive(Debug)]`. Every request-side secret in this crate has a hand-written redacting `Debug` (`Secret`, `Minted`, the request DTOs); the server-side ones do not. A `{:?}` of any of these four types would write the Argon2 hash, the HMAC key that signs every session, and a pending setup token.
- Failure scenario: a later `tracing` field, panic message or test assertion formats `ApiState` or `SessionAuth` with `{:?}`; the session key lands in a log file that the logs API and the archive download then serve.
- Suggested fix: a manual `Debug` for `State` and `Auth` that prints only `set_up`, `notice` and whether a token is pending.
- Tracked?: not found.

### APISEC-06 Behind a proxy that rewrites `Host`, every write fails `cross_origin`, and the guide does not say so
- Severity: polish
- Kind: docs
- Confidence: likely (code confirmed; nginx's default `proxy_set_header Host $proxy_host` is from memory)
- Where: `crates/api/src/extract.rs:206-218`, `crates/api/src/upload.rs:313-323`, `docs/user/03-keep-lodestar-safe.md:28`, `docs/operations.md:444`
- What: `same_origin_json` compares the browser's `Origin` host with the `Host` the server receives and consults no forwarded-host header, even from a trusted proxy. A reverse proxy that sends the upstream name as `Host` (plain nginx `proxy_pass` does unless told otherwise) makes setup, login and every other write answer `403 cross_origin`. The user docs mention only `LODESTAR_SECURE_COOKIES` for proxies.
- Failure scenario: a user puts Lodestar behind nginx with a minimal config; the UI loads, and sign-in fails with "Cross-origin requests aren't allowed."
- Suggested fix: document "the proxy must pass the original `Host`" beside the secure-cookies note (and in the proxy-auth section); optionally accept `X-Forwarded-Host` from a trusted proxy peer.
- Tracked?: not found in `docs/user/`, `docs/operations.md` or `docs/decisions/`.

## Checked and found sound
- **Every route's guard** (`crates/api/src/*.rs` `routes()`): see the table. `export::routes()` and `ops::purge_routes()` look unguarded where they are defined but are merged inside `jobs::routes()`' `protected`. `route_layer` wraps the 405 fallback too, so a missing session is always 401 first.
- **CSRF for cookie and proxy-header sessions:** of the 87 annotated non-GET handlers, 85 call `same_origin_json` (directly, or through `jobs::control` / `ops::set_paused`); the other two are upload, which has a stricter copy with `Sec-Fetch-Site`, and the webhook receiver, which takes a token and no cookie. No CORS layer exists, so a preflight never succeeds. The session cookie is `HttpOnly; SameSite=Strict; Path=/`, `Secure` on configuration.
- **Scope escalation** (`auth/guard.rs:163-279`): key requests are GET/HEAD only (`403 key_read_only`), `/keys` and every other management route are session-only, a handler's `Set-Cookie` is stripped from key responses, two different presented secrets are refused, a feed token is accepted only as `?token=` on feed routes, and scope is checked as a full mask. A key cannot mint keys or write.
- **Session handling** (`auth.rs`): stateless HMAC-SHA256 cookie over expiry and nonce, constant-time compare, key rotated on password change and on revoke-others, login signs with the key the password was checked against, no pre-auth session to fixate. Save-then-publish (`commit`) keeps memory and `auth.json` in step; the temp file is created fresh with 0600.
- **Password hashing:** Argon2id, 19 MiB, t=2, p=1, 16-byte random salt, 10 to 1,024 bytes, verified off the async workers.
- **First-run claim** (`auth.rs:399-459`): 64-bit token, logged as a `Secret` value, compared in constant time, rate-limited per address, spent only after the save, re-checked under the save lock after hashing so two racing setups cannot both win. No protected route answers before setup (`verify` and `proxy_signs_in` both require a password).
- **Trusted-proxy mode** (`auth/proxy.rs`): the user header counts only when the TCP peer is in a trusted range; `/0` and ranges holding the unspecified address are refused; exactly one header value; reserved header names refused; `X-Forwarded-For` is walked from the right and stops at the first unparseable entry; all proxy headers are ignored when the mode is off; `LODESTAR_PROXY_AUTH=off` recovery. A proptest holds the walk to "never a trusted address, never panics".
- **API key and webhook token storage and checks:** 256-bit secrets from the OS RNG, SHA-256 at rest, constant-time compare (`crates/store/src/api_keys.rs:209-236`, `webhooks.rs:384-403`), one answer for every failure, failures counted per address, secrets shown once with `Cache-Control: no-store`.
- **Torznab** (`torznab.rs`, `torznab/query.rs`, `torznab/xml.rs`): the `apikey` never reaches a log (`request_log.rs` logs the matched route template and the TCP peer only) or a response; the query is bounded (4 KiB, 64 parameters, 256-byte `q`, 32 words, 32 categories) and built as an AST, never parsed as query language; XML values are escaped in text and attributes with controls and bidi overrides removed; private rows are excluded twice.
- **Upload** (`upload.rs`): raw body only, exact content type, per-address bucket before the body is read, 8 MiB cap enforced while reading whatever `Content-Length` said, 60 s read deadline, 4 in flight; no file name or path is taken from the request, so there is nothing to traverse.
- **Export** (`export.rs`): CSV cells are defused (`'` before a leading `= + - @ tab CR`), controls and bidi characters written as escapes, quoted per RFC 4180; file names are generated from the clock; streaming is chunked, throttled, capped at 1,000,000 rows, bounded by stall, pause and lifetime limits, and at most 4 jobs run.
- **Logs** (`logs.rs`, `crates/lodestar/src/logging/source.rs:241-262`, `names.rs:30-61`): session only; a download name is parsed into a typed `LogName` and must also be in the folder listing as a regular file; streams re-check the session at every heartbeat and are capped (2 per session, 8 in all); repeated identical lines are folded, so refused-attempt events cannot flood the log.
- **Settings** (`settings.rs`, `crates/lodestar/src/settings/mod.rs:328-457`, `1247-1293`): secret fields are returned as `null`, list items lose their secret keys, and a saved secret is lent to a connection test or kept on a patch only while scheme, host and port (or the bound SMTP fields) match the saved endpoint. Test results carry fixed words and at most an HTTP status. At most 4 tests run, each under a 30 s ceiling.
- **Outbound SSRF guard** (`crates/integrations/src/net.rs`, design level): one resolution per connection with every answer checked, no system proxy, no automatic redirects, each followed hop re-checked, metadata and link-local never allowed, notification channels never on loopback.
- **Embedded UI** (`web.rs`, `web_build.rs:405-437`): compile-time asset table (no filesystem), decoded paths with dot, empty or backslash segments are 404, `nosniff` everywhere, hash-based CSP without `unsafe-inline` or `unsafe-eval`, `frame-ancestors 'none'`, `base-uri 'none'`. The status page's looser inline policy is paired with `textContent`-only rendering (`status.html:162`).
- **Error bodies:** `internal()` logs the cause and returns a fixed message; settings, updates and system errors carry fixed text or a stable code; JSON rejections name the field path, never a value.

## Area-level concerns
- **Lockout as denial of service is a recorded trade-off that deserves a second look** (`docs/architecture.md:2014`, `docs/decisions/2026-10-23-B12.md` "Limiters"). Sign-in, setup, password change and every fresh-password confirmation share one bucket per client address (burst 5, one per 12 s), and a correct password is refused when it is empty. Where clients share an address (Docker Desktop and the userland proxy, a reverse proxy without proxy-header auth, NAT), one bad login every 12 s from anyone keeps the admin out and blocks key creation, proxy changes and purges. The failed-key bucket has the same shape: one junk key every 6 s from a shared address stops a valid Torznab key from that address. A separate bucket for confirmations made from an already valid session would remove the worst of it.
- **CSRF is enforced per handler, not by a layer.** It is complete today, but nothing fails if a new write handler omits `same_origin_json`, and the comparison already exists twice (`extract.rs` and `upload.rs`). A middleware on the `/api/v1` router for every non-GET method (with the webhook route exempt) would make it structural. It matters most in proxy-header mode, where `SameSite` does not apply.
- **No connection-level limits are configured** (`crates/lodestar/src/services.rs:872`): `axum::serve` with no request timeout, concurrency limit or connection cap layer. Suspected, not confirmed: axum's `serve` sets no hyper timer, in which case hyper's header-read timeout is not enforced and idle or slow connections are held without bound. I could not read axum 0.8.9's source here to confirm.
- **Sessions cannot be revoked one at a time.** Logout only clears the browser's cookie; a copied cookie stays valid for up to 30 days unless the password changes or "sign out other sessions" rotates the key. This is the documented stateless design; it is worth stating in the user guide's "keep Lodestar safe" page.
- **Open operational endpoints.** `/metrics` and `/healthz` are open by decision and carry no torrent names, but they do name the enabled source plugins and the volumes, and `/openapi.json` gives the exact version. With APISEC-02 these are readable from outside the LAN.
- **Test coverage:** the guard and limiter paths are well covered (`tests/keys.rs`, `tests/auth_proxy.rs`, `auth/lock_tests.rs`, `tests/auth_failed_save.rs`). Not covered: a test that walks the real router for unguarded routes (APISEC-04), and any test of many addresses logging in at once (APISEC-03).
