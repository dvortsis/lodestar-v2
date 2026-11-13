# API feature handlers review
Reviewed at 55e155a9.

## Coverage
- Read in full (non-test code): `crates/api/src/lib.rs`, `extract.rs`, `error.rs`, `events.rs`, `search_live.rs`, `search.rs`, `insights.rs`, `insights/csv.rs`, `torrents.rs`, `compare.rs`, `collections.rs`, `user_items.rs`, `saved.rs`, `alerts.rs`, `ops.rs`, `jobs.rs`, `facets.rs`, `workers.rs`, `companions.rs`, `companions_guided.rs`
- Read in part: `filters.rs` (handlers, `assess_tracked`, `apply`, `correct`, class rules, re-queues, preview jobs, queue and tombstones; not `override_change`, `warning`, `undo`, `release*`, `overview`), `filters/subscriptions.rs` (`check_subscribe` only), `insights/stages.rs` (readers, handler and cache path; not every stage builder), `digest.rs` (handler, formatters, escaping; not `model` or `render_*`), `log_events.rs` (`change_detail`)
- Skimmed (outline only): `filters/packs.rs`, `filters/standin.rs`, `dto/**` (spot checks of `ops`, `events`, `filters`; route and method parity with `web/src/lib/api/openapi.json` checked by script for all 87 routes in the area: no drift)
- Followed into other crates to confirm findings: `crates/filter/src/engine.rs` (`below_overrides`), `crates/filter/tests/load_fail_closed.rs`, `crates/store/src/{insights,insights_days,collections,saved,filter,rules}.rs`, `crates/lodestar/src/{services,percolator}.rs`, `crates/lodestar/src/settings/{insights,mod}.rs`
- Not read: the in-file test modules and `crates/api/tests/**` beyond targeted greps (time); DTO field-by-field comparison with `openapi.json` (CI regenerates and diffs it; I only checked routes and methods)

## Findings
### APIFEAT-01 Adding a `flag` or `demote` rule switches off packs and lists for what it matches, with no warning or password
- Severity: blocking
- Kind: security
- Confidence: confirmed
- Where: `crates/api/src/filters.rs:1147-1156` (`Change::AddRule` sets no `weakening`), `crates/api/src/filters.rs:1172-1180` (`UpdateRule` that only enables), `crates/api/src/filters.rs:3807-3871` (`apply_change` asks for the password only when `assessment.confirm`), `crates/filter/src/engine.rs:1242-1284` (the first matching user rule returns its decision before the pack and list layer), `crates/filter/tests/load_fail_closed.rs:423-427` (a test that pins it)
- What: §20.11 treats adding a rule as strengthening, so `POST /filters/apply` with `{"op":"add_rule",…}` applies with a session alone. But user rules are first match wins and decide before the pack layer: in `below_overrides` a matching, readable user rule returns `settle(…)` and the packs and subscribed lists are never consulted. A new rule with a light action therefore replaces whatever a pack or list would have done to every torrent it matches. The repository's own test shows it: with rule `movie → flag` and the malware pack on, `Movie.2019.mp4.exe` is judged `(User(1), Flag)`, not the pack's Hide. The engine already knows this loosens: stand-ins are special-cased so they "never pre-empt a stronger decision" (`docs/decisions/2026-11-01-F-filter-load-fail-closed.md:21`), but ordinary rules are not, and `assess` does not rejudge for `AddRule`.
- Failure scenario: Someone at an unlocked session (the case the password prompt exists for) sends `{"change":{"op":"add_rule","query":"<any broad query, for example size:>0>","action":"flag"}}`. The response is 200 with no warning (the API's own test `strengthening_applies_without_the_password`, `crates/api/tests/filters.rs:177-190`, adds `ext:exe → hide` this way, and lines 2602 and 2760 add `flag` rules with no password). From then on every new torrent that no core rule holds is stored and searchable with a flag, whatever the malware, spam, lure and scam packs or a subscribed non-floor blocklist say. §20.11 requires a warning and the password for "disabling a pack or pack rule"; this does the same thing without either. The core (floor, CSX, CS rules) still decides first and is not bypassed.
- Suggested fix: Either make the engine keep the strongest of the user rule's decision and the pack/list layer's (as it does for stand-ins), or have `assess` treat `AddRule` (and an enable-only `UpdateRule`) as weakening unless its action is at least as strong as anything the later layers can decide, with a rejudged preview. Add an API test: flag rule plus a pack that hides, no password, must be refused or must not lower the verdict.
- Tracked?: not found in `runner/slices.yaml`, `docs/build-plan.md` or the open PRs. The stand-in decision file records the mechanism for unreadable rules only.

### APIFEAT-02 False positive, Confirm and Reveal name only work on the newest 100,000 quarantine rows
- Severity: important
- Kind: correctness
- Confidence: confirmed
- Where: `crates/api/src/filters.rs:126` (`PREVIEW_ROWS`), `:506-518` (`Snapshot::read`), `:1322-1344` (assess `FalsePositive`), `:2219-2226`, `:2269-2278`, `:2289-2296` (apply `FalsePositive`, `Confirm`, `RevealName`), `crates/store/src/filter.rs:424-441` (`quarantined(limit)`: newest first, `LIMIT`)
- What: These changes look their hash up with `store.quarantined(PREVIEW_ROWS).into_iter().find(…)`, which reads the newest 100,000 rows and scans them. `/filters/queue` pages past that with its cursor, so the UI lists older rows whose actions then fail. Each such action also reads and decodes up to 100,000 rows to find one. `RemoveCs1` and `ResolveFloorRemoval` previews count tombstones in the same 100,000-row window, so their counts understate on a larger table without `estimate` being set.
- Failure scenario: A drop or quarantine rule is unreadable after a rollback, so its stand-in quarantines every new torrent (the documented fail-closed behaviour, about 860,000 a day at 10 a second). The user fixes the rule, pages back through the queue and marks an older row a false positive: `404 not_in_quarantine`; Confirm gives `404 entry_not_found`. `Release` still works because it deletes by hash.
- Suggested fix: Add `Store::quarantine_row(hash)` (a primary-key lookup) and use it in all three; count tombstones for a hash set in SQL.
- Tracked?: not found.

### APIFEAT-03 Insights Serve is permanently null although its counters are recorded
- Severity: important
- Kind: gap
- Confidence: confirmed
- Where: `crates/api/src/insights/stages.rs:744-770` (`serve_kpi`, `serve`), `crates/api/src/insights.rs:1667-1671,1724-1729` (`Serve::default()`, `stages.serve: None`), `crates/lodestar/src/services.rs:1132-1147` (`searched` writes `metric::SEARCHES` and `metric::SEARCH_LATENCY`), `crates/pipeline/src/rollups.rs:35-37,155-156`
- What: Every search adds to the `SEARCHES` (by outcome) and `SEARCH_LATENCY` (by bucket) rollups, and the pipeline keeps and compacts them, but nothing in the API reads them. `GET /insights/serve` returns every KPI and every trend point as null, the overview's `serve` is `Serve::default()`, `stages.serve` is always `None`, and the digest and the `insights` SSE event carry the same nulls.
- Failure scenario: A user opens Insights → Serve after weeks of searching: searches, zero-result rate and latency p50/p95 all show a dash, and the stage strip's Serve tile is empty, although the data is in `rollup_hour`.
- Suggested fix: Read `SEARCHES` (dim = outcome) and `SEARCH_LATENCY` (key = bucket) with `read_metric` in `serve()`, fill `searches`, `zero_result_rate`, `latency_p50_ms`/`p95_ms` and the trend, and fill `stages.serve` in the overview and the digest.
- Tracked?: Deferred in `docs/decisions/2026-10-23-B11a.md` (lines 32 and 69, "when #48 merges"); the counters merged and no slice in `runner/slices.yaml` picks the read up.

### APIFEAT-04 The digest is rendered and previewed but never sent; its schedule settings do nothing
- Severity: important
- Kind: gap
- Confidence: confirmed
- Where: `crates/api/src/digest.rs:97-110` (`digest()`, whose only caller is `preview`), `crates/lodestar/src/settings/insights.rs:70-92` (`digest`, `digest_day`, `digest_hour`, `digest_channels`)
- What: Settings → Insights stores a digest frequency (off, daily, weekly), a day, an hour and channels. Nothing reads them: there is no scheduler and no `NotificationKind::Digest`. `GET /digest/preview` is the only consumer of the renderer.
- Failure scenario: A user sets the digest to weekly on Monday 03:00 with an email channel and saves. No digest ever arrives, and nothing says so.
- Suggested fix: Build the sender follow-up, or hide or mark the schedule fields "not available yet" until it exists.
- Tracked?: `docs/decisions/2026-10-23-B11c.md` lists it under "Left to the sender follow-up"; no slice for that follow-up is in `runner/slices.yaml`.

### APIFEAT-05 Collections have no size or count cap, and every list and page checks every entry under a shared store lock
- Severity: important
- Kind: resource
- Confidence: confirmed
- Where: `crates/api/src/collections.rs:66-90` (`all_entries`, `visible_count`), `:158-176` (`list`), `:225-274` (`get_one`), `:204,322,410,458,553` (`summary_of` on the writer connection after each change), `crates/store/src/collections.rs:68-75,176-208` (no limits)
- What: `GET /collections` reads every entry of every collection and does a torrent-row and an enrichment lookup for each, to count the visible ones. `GET /collections/{id}` does the same for the whole collection on every page, whatever `after` and `limit` are. Each mutation recounts the whole collection while holding the user-data writer lock. Neither the number of collections nor a collection's size is capped (saved searches are capped at 1,000 and class rules at 1,000 for this kind of reason), and `POST /collections/{id}/items` adds 500 per call. The reader is the one `Arc<Mutex<Store>>` that search hits, detail and Insights also wait on.
- Failure scenario: A user builds 20 collections of 50,000 torrents each (bulk "add all results"). Opening the Collections page does 2,000,000 point lookups under the reader lock; every search arriving meanwhile waits for it before it can read its hit rows.
- Suggested fix: Cap collections and entries per collection in the store, count visible entries with one SQL join (as `Store::shown_alerts` and `unread_shown` do for alerts), and page `get_one` with `position > ?` plus a counted total.
- Tracked?: not found.

### APIFEAT-06 A session's event streams are refused for up to about 15 minutes after four connections die without closing
- Severity: important
- Kind: resource
- Confidence: likely
- Where: `crates/api/src/events.rs:583-603` (`acquire`: `SessionFull` past `max_per_session`, 4), `:413-428` (a slot is released only when the response body drops), `:1093-1104` (429 `too_many_streams`), `crates/lodestar/src/services.rs:838-850` (the listener: no TCP keepalive or write timeout; none found anywhere in `crates/lodestar/src`)
- What: A slot is freed when hyper drops the body, which needs the socket to fail. A client that vanishes without a FIN or RST (a phone leaving Wi-Fi, a VPN toggled, a NAT mapping dropped) leaves the connection open until the kernel gives up retransmitting the 15 s heartbeat, about 15 minutes with Linux defaults. Each reconnect takes a new slot, and the fifth is refused. Live queries already handle their cap by replacing the oldest; streams refuse the newest. A client that stays connected but stops reading keeps its slot with no limit, and its session is no longer re-checked, because the task is parked in `send` rather than at the heartbeat. I did not test this against a running server; it rests on the absence of any socket timeout in the serve path.
- Failure scenario: Two tabs on a laptop that changes network twice in ten minutes: four dead streams hold the session's four slots, both tabs get `429 too_many_streams` ("close a tab") on every 5 s retry, and no live counts, alerts or settings events arrive until the old sockets time out.
- Suggested fix: When a session is at its cap, close its oldest stream and admit the new one; give `send` a deadline (say two heartbeats) after which the stream ends; set TCP keepalive on accepted sockets.
- Tracked?: not found.

### APIFEAT-07 Overview annotations keep the oldest 1,000 and drop the newest, with no flag
- Severity: polish
- Kind: correctness
- Confidence: confirmed
- Where: `crates/api/src/insights.rs:1624,1678-1682` (`store.events(...)` then `a.truncate(MAX_ANNOTATIONS)`), `crates/store/src/insights.rs:135-147` (`ORDER BY at, id`, no `LIMIT`)
- What: `store.events` returns the window's events oldest first and unbounded; the overview truncates to the first 1,000, so on a long range the most recent restarts, pauses and imports are the ones dropped. `/insights/events` reports `truncated`; the overview has no such field.
- Failure scenario: `range=all` on an install with more than 1,000 restart/pause/intensity events: the trend shows annotations for the first months and none for the recent ones.
- Suggested fix: Keep the newest 1,000 (or thin evenly), add `annotations_truncated`, and give `Store::events` a `LIMIT`.
- Tracked?: not found.

### APIFEAT-08 A purge paused past its limit fails as a bare 500 `internal`
- Severity: polish
- Kind: correctness
- Confidence: confirmed
- Where: `crates/api/src/ops.rs:917-923` (`Stop::PauseExpired` → `OpsError::Internal`), `:281` (`internal(format!("ops: {e}"))`), `crates/api/src/jobs.rs:560-563` (the documented `pause_expired`)
- What: `/jobs/{id}/pause` documents that a job not resumed within 10 minutes "fails with `pause_expired`". For a purge the worker maps it to `OpsError::Internal`, so the job's `error` is `{code: "internal", message: "Something went wrong on the server."}` and an `api.internal_error` event is logged for an expected outcome.
- Failure scenario: A user pauses a purge and comes back after 15 minutes: Jobs shows "Something went wrong on the server" instead of saying the pause ran out and that purging again resumes it.
- Suggested fix: Finish with a 409 `pause_expired` `ApiError`, as exports do.
- Tracked?: not found.

### APIFEAT-09 `alerts` events carry real torrent and alert ids while Adult is hidden
- Severity: polish
- Kind: correctness
- Confidence: confirmed
- Where: `crates/api/src/alerts.rs:148-157` (`alert: a.id`, `max_id: a.torrent_id`), `crates/api/src/dto/events.rs:175-187`, against `crates/api/src/events.rs:232-248` and `crates/api/src/search_live.rs:32-34,413`
- What: `docs/decisions/2026-10-22-B4b-r2.md` makes `torrents.max_id` null while Adult is hidden, because sequential ids beside exact counts reveal how many hidden torrents arrived; `search_live` follows it (`since`, `through`). The `alerts` event, added later, always sends the highest torrent id and the highest alert id. Search hits also carry `id`, so the rule is only partly effective anyway; either apply it everywhere or drop it.
- Failure scenario: With Adult hidden, a client compares the gap between two `alerts` events' `max_id` with the `torrents.added` counts in between and reads off how many hidden torrents were stored.
- Suggested fix: Null `max_id` (and send a per-search ordinal instead of the global alert id) while Adult is hidden, or record that the rule is dropped.
- Tracked?: not found (the decision file covers `torrents` only).

### APIFEAT-10 The `docker run` join command puts the token on a command line
- Severity: polish
- Kind: security
- Confidence: confirmed
- Where: `crates/api/src/workers.rs:334-352` (`-e LODESTAR_TOKEN={token}` in `docker_run`, under the comment "never a command-line flag (WK-2: argv is visible to every local user)")
- What: The CLI forms pipe the token on standard input, but the ready-to-copy `docker run` line passes it as `-e NAME=value`, which is in `docker`'s argv, in shell history and in `docker inspect` afterwards. The token is single use and lasts 15 minutes, so the exposure is short.
- Failure scenario: On a shared host another local user reads the token from `ps` while `docker run` starts and joins a rogue worker first under the same name.
- Suggested fix: Emit `read -rs LODESTAR_TOKEN && export LODESTAR_TOKEN && docker run … -e LODESTAR_TOKEN …` (the name only), as `cli_read` does.
- Tracked?: not found.

### APIFEAT-11 One saved search whose stored AST no longer deserializes turns the whole list into a 500
- Severity: polish
- Kind: correctness
- Confidence: likely
- Where: `crates/api/src/saved.rs:88` (`serde_json::from_str(&s.search.ast).map_err(internal)?` inside `view`), `:142-156` (`list` collects with `?`)
- What: `GET /saved-searches` and `GET /saved-searches/{id}` fail with 500 if any row's `ast` JSON isn't a `Node` this build reads (a downgrade, or a query-language change that renames a variant). The filters handle the same situation row by row (`filter_rows_unreadable`); here one row hides all of them, so the UI can't offer its delete. I did not find a way to write such a row with this build alone, hence "likely".
- Failure scenario: After a rollback to an older build, the Alerts page shows an error and no saved search can be opened, edited or deleted from the UI.
- Suggested fix: Fall back to re-parsing `query`, or return the row with `ast: null` and an `unreadable` flag.
- Tracked?: not found.

### APIFEAT-12 The pacer test asserts wall-clock upper bounds
- Severity: polish
- Kind: test
- Confidence: likely
- Where: `crates/api/src/jobs.rs:790-831` (`the_pacer_shares_one_rate_and_never_bursts`: `took < 3 s`, `took < 600 ms`, `free.elapsed() < 50 ms`, on the real clock)
- What: The test runs on real time and fails if the runner is descheduled for longer than its slack (50 ms for the unpaced call). Standards §2 asks tests to pass on the slowest runner under load.
- Failure scenario: A loaded Windows CI runner pauses the test thread for 60 ms: the last assertion fails on code that is correct.
- Suggested fix: Run it with `#[tokio::test(start_paused = true)]` and `tokio::time::Instant`, keeping only the lower bounds and exact virtual-time checks.
- Tracked?: Open PR 778 (X-test-blockers-1112) is about test flakes; I could not see whether it covers this test.

## Checked and found sound
- `events.rs`: per-client state is bounded (shared broadcast buffer of 256, a 16-frame queue, a ring of 512); a slow client gets one `resync` and is never grown; the slot and the topic listener counts are released when the body and the subscription drop; `Last-Event-ID` parsing is strict, and `seq + 1` cannot overflow because `seq > ring.seq` is checked first; events made while Adult was shown are withheld on replay and live; the session is re-checked at each heartbeat.
- `search_live.rs`: registrations are capped per session (4, oldest replaced) and in all (256); they expire after the session's last stream closes; a release only removes the caller's own; counts are per session, never broadcast; the lease runs on the blocking pool with cancel-on-drop.
- `search.rs`: `limit` is clamped to 1–200, `offset + limit` saturates and is capped at 10,000, `tz` is range-checked, sort keys are a closed list; the cursor is HMAC-tagged (16 bytes), length-capped, bound to the query/visibility/sort fingerprint and to a per-process epoch; a client AST must round-trip through the parser; the search runs under `spawn_blocking` with cancel-on-drop.
- `facets.rs`: per-facet limits are checked against the engine's cap, the lease is opened with the same fingerprint as `/search`, a 5 s budget applies, and private source names are withheld from non-session callers.
- `insights.rs`: custom ranges are validated (non-negative, ordered, at most 20 years); bucket arithmetic is in whole UTC hours with `div_euclid`; ratios return `None` on a zero denominator; unmeasured buckets are null, not 0; the `expect("in range")` lookups in `sealed_days` and `compacted_days` hold because both ends are day-aligned and the store groups by `(hour / 24) * 24`; the store is read on the blocking pool in one read transaction.
- `insights/csv.rs`: text cells go through `export::csv_cell`; the file name is built from the validated panel name and integers only; `nosniff` and `no-store` are set.
- `digest.rs`: HTML is built through an escaping builder whose raw path takes only `&'static str`; response strings are stripped of control and format characters and cut to 64 characters; fix links are built only from `^[a-z_]{1,32}$` targets.
- `torrents.rs`: the hash is validated before any lookup; Adult while hidden is a 404 on detail and files; private sources and private claims are withheld from API-key and feed callers and fail closed without an `Access`; scene links are restricted to `https` on the plugin's own hosts; the drawer-interest rate limit is bounded (1,024 clients, then a shared bucket).
- `compare.rs`: 2–5 distinct hashes; withheld and unknown torrents share one 404; the file diff holds one decoded tree at a time and caps each list at 2,000; sibling lookups are cancelled when the client goes away.
- `user_items.rs`, `alerts.rs`: cursors only move a position (no trust in them); withheld rows are skipped and never counted; the feed gives one 404 for every feed the caller can't have and its XML comes from the escaping renderer.
- `ops.rs`, `jobs.rs`: purge needs a fresh password every time and is single-flight per source under `purge_start`; an egress change needs the password; credentials are read by hand so a refusal never quotes a value; a panicking or dropped job worker finishes as failed and frees its slot; at most 4 jobs run and 64 are kept.
- `workers.rs`: a join token needs a fresh password, the name is validated before the binary is asked, the `Host` used in the commands is restricted to plain DNS or IP characters, budget ranges are checked, and worker 0 is refused; revoke needs no password, as the Danger zone row in `docs/architecture.md` §10 (line 772) asks for a confirmation dialog only.
- `companions.rs`, `companions_guided.rs`: ids and fields are shape-checked before the binary is asked, install must echo the offered version and SHA-256, removal and forget must name the companion, the reveal checks the password before the id, and the reveal and logs are `no-store`. Install without a password is as the companions plan records it.
- Route and method parity: all 87 routes registered by the files in this area appear in `web/src/lib/api/openapi.json` with the same methods.
- `filters.rs` (`apply`): changes whose weakening depends on stored state (core switch, pack overrides, pack version acceptance, held list versions) are read again under the store lock and refused with `changed_since_preview` if the password did not cover them; subscribe URLs go through the SSRF guard's DNS-free checks.

## Area-level concerns
- **One reader connection for everything.** `ApiState.reader` is a single `Arc<Mutex<Store>>`. Search hit rows, detail, compare, collections, user items, saved searches, alerts, facets and all of Insights serialize on it. Most handlers lock per lookup, but `GET /saved-searches` (`saved.rs:67-80`, every alert of every search, two lookups each, up to 1,000 × 1,000), `GET /user-items/labels`, the collections endpoints (APIFEAT-05) and a cold 20-year Insights range hold it for a whole scan. A small pool of read-only connections would remove the coupling.
- **Two ways of taking a poisoned lock.** Search, torrents, compare, collections, user items, saved searches, alerts and facets use `.lock().expect("reader lock")`; Insights uses `unwrap_or_else(PoisonError::into_inner)` on the same mutex. A panic in any holder (I found none reachable) would leave Insights working and turn every search and detail request into a 500 until restart. Pick one, preferably the tolerant one, since the store connection has no invariant a panic breaks.
- **No caps on user filter rows.** User rules, exceptions, CSX-5 entries, Always-allow entries and list subscriptions have no maximum (`crates/store/src/rules.rs:445-458`); classification rules are capped at 1,000. Each apply reloads and recompiles the whole set, and each subscription is a recurring outbound fetch.
- **`GET /classify` is heavy for a page load.** `filters.rs:3083-3141` reads up to 10,000 corrections, each with its full file tree, and classifies each again on every call.
- **Tests cover the stated contract, not the layer interaction.** The filters API tests assert that strengthening changes need no password, and the engine tests assert first-match-wins, but nothing tests the two together (APIFEAT-01). I found no test for a quarantine larger than `PREVIEW_ROWS`, for a collection with withheld entries at scale, or for an event stream whose client stops reading.
- **`ReportAbuse` deletes stored torrents and adds floor entries with a session alone** (`filters.rs:2192-2206`), for any number of hashes in one request. §20.11 counts it as strengthening, so this follows the spec; it is still the one destructive filters action with no confirmation on the server side, and undoing it (`RemoveCs1`) needs the password and cannot bring the rows back.
- **Error messages echo input without a length cap** in a few places (`filters.rs:194` `hex20` quotes the whole string; `unknown_rule`, `unknown_pack`, `unknown_list` quote ids). The values are JSON-escaped and the body limit bounds them; `events.rs` cuts its echo to 64 characters, which is the better pattern.
