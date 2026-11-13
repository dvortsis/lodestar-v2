# Sources framework and plugins review
Reviewed at 55e155a9.

Context that scopes every finding: the binary registers only four source plugins (`crates/lodestar/src/services.rs:257-364`): the DHT, the torrent cache (External), the Sonarr/Radarr webhooks (None) and Prowlarr/Jackett (Account). `control::consent_text_approved` (`crates/pipeline/src/control.rs:595`) refuses every class but None in this build, so of the plugins in my area only the webhooks can be switched on today. Nyaa, Sukebei, Internet Archive, Academic Torrents, download clients, full scrape, StashDB, ThePornDB and Wikidata are compiled in and tested but not registered. Findings marked **latent** are real in the code and become live when the plugin is wired or the consent gate lifts (X-consent-gate-lift).

## Coverage
- Read in full (non-test): `crates/sources/src/{lib,plugin,claims,manifest,state,http,egress,consent,enrich,identify}.rs`; every file in `crates/sources-plugins/src/` (`prowlarr`, `arr`, `torrent_cache`, `full_scrape`, `archive_org`, `nyaa`, `academic`, `clients`, `stashdb`, `tpdb`, `wikidata`, `catalogue`, `lib`); `crates/sources-feeds/src/{common,torznab,rss,json}.rs`.
- Read the parts that decide my area's questions: `crates/pipeline/src/unverified.rs` (store_claims, judge, ingest, Verify, with_mismatch), `crates/pipeline/src/torrent_cache.rs` (Gate, Cursor, judge), `crates/pipeline/src/lib.rs` (plugin start-up 800-960, intake 1565-1700), `crates/pipeline/src/{adult_enrich,identify}.rs` (run loop, lookup, cursor, budget), `crates/lodestar/src/torrent_cache.rs` (feeder), `crates/lodestar/src/services.rs` (registration), `crates/sources/src/pool.rs` (on_discovery, restore, add_peers, next_ready, evict_one, drop_source, lose).
- Skimmed: `crates/sources-feeds/src/{archive_org,stashdb,tpdb,wikidata}.rs` (limits, item finishing, a scan for indexing, slicing, unwrap and casts), `crates/integrations/src/net.rs` (NetPolicy, check_url, redact), `crates/api/src/webhooks.rs` (module contract and constants), `crates/api/src/torrents.rs` (scene_link), tests for sleeps and for what failure paths they pin, `fuzz/fuzz_targets/` and `fuzz/Cargo.toml` for each parser's target.
- Not read: the rest of `crates/sources/src/pool.rs` (budget/reservations, streaming lookups, timers, dispatch, about 1,900 lines) and `pool/admission.rs` (crawler scheduling, not source trust); `crates/sources/src/{funnel,testing,egress_tests}.rs`; `crates/store/src/{unverified,claims}.rs` (store reviewer's area; I relied on its documented contract for `hold_claims` and `put_unverified`); the bodies of the test files; the UI. Open PRs 787, 789, 793, 799 were not read.

## Findings

### SRC-01 Every plugin gets an in-memory `PluginState`; the store-backed one is never constructed
- Severity: important
- Kind: gap
- Confidence: confirmed (latent)
- Where: `crates/pipeline/src/lib.rs:898-907`; `crates/sources/src/enrich.rs:171-253` (`StoreState`, constructed only in its own test at `:296`); users of `kit.state`: `crates/sources-plugins/src/full_scrape.rs:547-590`, `crates/sources-plugins/src/archive_org.rs:227-229,288,301-307`, `crates/pipeline/src/adult_enrich.rs:580-585,910-960`, `crates/pipeline/src/identify.rs` (same pattern)
- What: `Pipeline::start` builds each plugin's kit with `Arc::new(MemoryState::new())` and `UserLimits::default()`. The V14 `source_state` table and the `StoreState` adapter exist, but nothing outside a unit test ever calls `StoreState::new`. So everything a plugin or enrichment runtime keeps "so a restart doesn't reset it" lives in memory: the full scrape's last attempt per tracker, archive.org's cursor/resume/floor, and the enrichment runtimes' cursor, daily count and hit/miss caches. `docs/decisions/2026-11-05-ENR-1.md:18` states "`StoreState` is the `source_state`-backed `PluginState` the host gives the kit"; the host does not. `UserLimits::default()` also means no user poll or rate setting can reach a limiter.
- Failure scenario: once wired, a restart forgets "scraped at T": the tracker is full-scraped again inside the same day (the three guards in `full_scrape.rs:10-16` all reset together, since the other two are the in-memory limiter). The adult-enrichment day count restarts at 0, so the owner's 5,000 lookups a UTC day (Q15) can be exceeded by restarting, and the 30-day/7-day caches are lost, so every item is asked again. Archive.org restarts a day back.
- Suggested fix: build the kit with `StoreState::new(store, id)` for every plugin in `Pipeline::start`, and add one pipeline-level test that restarts the pipeline and reads a plugin's state back. The existing restart tests (`tests/full_scrape.rs:234`, `tests/archive_org.rs:280`, `pipeline/tests/identify.rs:845`) pass a shared state by hand, so they pass on wiring production does not have.
- Tracked?: `docs/decisions/2026-11-01-SRC-0.md:18` assigns it to "SRC-1, SRC-4 and SRC-5"; none did it. No slice found in `runner/slices.yaml`.

### SRC-02 A floor or core hit on any source's claim text deletes the verified torrent stored under that hash
- Severity: important
- Kind: security
- Confidence: confirmed (a recorded decision)
- Where: `crates/pipeline/src/unverified.rs:990-1033` (`ingest` → `store.hold_claims`), `:517-580` (`judge_texts`: name with file paths, description, publisher, details texts), `docs/decisions/2026-11-02-SRC-2.md:21,28`
- What: claim text is untrusted feed text, yet a core/floor match on it holds every swarm hash of the torrent already stored under the key and deletes that torrent in the same transaction. Non-core hits were changed (#383) to drop only the source's claims for exactly this reason ("a feed could get an unrelated, legitimate torrent permanently refused by claiming its hash"); the core path was kept, and the decisions file says "The runner flags that abuse vector to the owner".
- Failure scenario: a public indexer behind Prowlarr, a Nyaa or Academic feed entry, or a release title relayed through a webhook carries the info hash of a legitimate verified torrent and a title (or description, publisher, file path, category) that matches a CS pattern. The verified torrent is removed from the store and the index and its hashes are tombstoned or quarantined, so the crawler cannot add it back. One Torznab page can do this for up to 1,000 hashes (`PAGE_LIMITS`).
- Suggested fix: on a core hit where a verified torrent is stored, drop the source's claims and re-judge the torrent on its own verified metadata; hold only if that hits too (or quarantine for review, never tombstone, on claim text alone). At least get the owner's answer recorded before X-consent-gate-lift.
- Tracked?: recorded in `docs/decisions/2026-11-02-SRC-2.md:28` and pinned by `crates/pipeline/tests/unverified.rs:261` (`a_core_hit_on_a_stored_torrents_key_holds_and_deletes_it`); no owner answer found in `docs/owner-decisions.md` or `docs/decisions/`.

### SRC-03 While a service's host is blocked, the enrichment runtimes pass items over and spend the day's count without sending anything
- Severity: important
- Kind: correctness
- Confidence: confirmed by reading (latent; no test pins it)
- Where: `crates/pipeline/src/adult_enrich.rs:773-812` and `:656-661`; `crates/pipeline/src/identify.rs:735-772` and `:636-640`; the fail-fast it meets: `crates/sources/src/http.rs:833-844` (`acquire` returns `HttpError::Blocked` at once), `crates/sources-plugins/src/stashdb.rs:253-259`, `tpdb.rs:223-229`, `wikidata.rs:246-262`
- What: `one()` takes a lookup from the day's budget (`Budget::take`) and then calls the plugin. When the limiter has the host blocked (a 401 or 403 blocks for `backoff.max`, 6 h; a 429 or 503 with `Retry-After` for up to 24 h), or Wikidata's own `not_before` hold is running (up to an hour after a `maxlag` answer), the plugin returns `Err` immediately. The runtime treats any `Err` as `Step::Asked`: the cursor is saved past the item (`background.done`), nothing is cached, and the next item follows after `interval` (1 s or 2 s). No code checks the host's state before taking the budget (`HostState` appears in neither run loop; `identify::timed_out` knows the problem and guards only its own charge).
- Failure scenario: StashDB answers one 429 with `Retry-After: 3600`. For the next hour the backfill "asks" an item a second: 3,600 items are passed over until the next round (a round rests a day), and 3,600 of the 5,000 daily lookups are spent on requests that never left. With a wrong API key the whole 5,000 go in 83 minutes, every day. For Wikidata each `maxlag` hold skips one item per 2 s it lasts. In `identify`, a host in ordinary back-off costs one skipped item and one count per 60 s lookup timeout.
- Suggested fix: before `Budget::take`, read `kit.http.limiter().state(host)` for the plugin's endpoints and return `Step::Wait` (item back to the front of the queue) while it is not `Ready`; map a plugin's "held" error to `Step::Wait` as well. Add a test with a 429 + `Retry-After` fixture that asserts the cursor and the count do not move.
- Tracked?: not found.

### SRC-04 Feed plugins poll at once on every start; no plugin persists its last poll as SRC-0 said it would
- Severity: important
- Kind: gap
- Confidence: confirmed (latent)
- Where: `crates/sources/src/http.rs:869-890` (`try_poll`, in-memory), `crates/sources-plugins/src/nyaa.rs:202-256`, `academic.rs:140-189`, `archive_org.rs:200-255`, `prowlarr.rs:580-604,697-759`, `prowlarr.rs:141-196` (`UsenetPolls`, the per-indexer daily API-hit cap, also in memory); `docs/decisions/2026-11-01-SRC-0.md:48`
- What: the poll gate and the daily quotas live in the limiter, which "is in memory and resets on restart". SRC-0's decision covers that with "a feed plugin stores its last poll time where a restart-loop would otherwise poll early". Only the full scrape does. Nyaa (§18.1: at least 5 minutes), Academic (hourly), Internet Archive (hourly) and each Prowlarr indexer (15 minutes) are polled the moment the plugin starts, whatever the time since the last poll. (The torrent cache's feeder waits one 60 s interval after start, so it is the mildest case.)
- Failure scenario: a container in a crash loop with `restart: always` (or a user restarting to apply settings, which Prowlarr and the torrent cache require) polls Nyaa on every start, far inside its 5-minute cap, and every indexer behind Prowlarr at once; with the Usenet setting on, the 48-hits-a-day cap per indexer restarts too. Third parties ban for this.
- Suggested fix: record each unit's last poll as wall-clock time in `PluginState` (after SRC-01) inside `Client::poll` itself, so every plugin gets it, and seed `try_poll` from it at start; add start-up jitter so plugins do not all fire in the same second.
- Tracked?: the rule is in `docs/decisions/2026-11-01-SRC-0.md:48`; no slice implements it for these plugins.

### SRC-05 The torrent cache throws away queued lookups whenever every cache is backing off, and a dropped interest lookup is never retried
- Severity: important
- Kind: correctness
- Confidence: confirmed by reading (latent: External class)
- Where: `crates/sources-plugins/src/torrent_cache.rs:800-822` (`lookup`), `:1032-1060` (run loop: `Err(_) => {}`), `crates/lodestar/src/torrent_cache.rs:200-226` (`seen`), `:227-250` (page feed), `:52,61`
- What: `lookup` passes over a cache whose host or unit is not `Ready` and returns `Miss::Failed` without waiting. With one listed cache (the common case), any failure puts it in back-off for at least 60 s and up to 6 h, and a 401/403 blocks it for 6 h. During that time the run loop takes each queued lookup, fails it instantly and moves on, so both lanes drain at once. The host feeder then (a) refills 64 unverified keys every `PAGE_EVERY` (60 s), each discarded the same way, so the cursor walks the table without asking anything, and (b) has already put each interest key in `seen` because the push succeeded, so opening the same torrent again does nothing until 4,096 other keys have been seen or Lodestar restarts.
- Failure scenario: the cache returns one 502. The user opens a torrent's detail a few seconds later; its lookup is dropped, and every later open is skipped by `seen`. Over a 6 h back-off about 23,000 unverified keys are passed over until the next round.
- Suggested fix: when no cache is ready, wait (with the shutdown select) until the earliest one is, or put the lookup back; mark an interest key seen only when a cache was actually asked, and give `seen` an expiry. Count the drops (`Stats::skipped` counts caches, not lookups).
- Tracked?: not found. `docs/decisions/2026-11-05-SRC-13.md` records "passed over, not waited out" for one cache among several, not the all-held case.

### SRC-06 A source whose every poll fails still reads as healthy; the limiter's "blocked by source" state and the parsers' summaries reach nobody
- Severity: important
- Kind: gap
- Confidence: confirmed
- Where: `crates/api/src/dto/ops.rs:233-246` (`SourceHealth`: `last_error`, `last_run`, `running`, two counts); `crates/sources/src/http.rs:648-654,919-945` (`HostState`, "as the Sources screen shows it", `state`/`unit_state` used only by the torrent cache and `identify::timed_out`); `crates/sources-plugins/src/nyaa.rs:231-237`, `academic.rs:169-175`, `archive_org.rs:252-257`, `prowlarr.rs:740-745` (non-200 answers and the parser's `Summary.stop`/`skipped` discarded); `crates/pipeline/src/lib.rs:947-955` (`last_error` set only when `run` returns `Err`)
- What: the plan's SRC-0 and SRC-14 promise a "blocked by source" state and per-source health. The API's health has no field for a host that is blocked or backing off, and plugins swallow every failed poll (`_ => return Poll::Done`) and every parser stop reason. Only a plugin that ends its run with `Err` (Prowlarr on a refused key or a setup error) sets `last_error`; such a plugin is then never restarted until the next start.
- Failure scenario: Nyaa answers 403 (blocked for 6 h, again and again), or its feed changes shape so every item is skipped as `no_hash`, or DNS fails. Settings → Sources shows the source enabled and running with no error; the only sign is a yield of 0.
- Suggested fix: expose per-plugin limiter state (worst host/unit state with its `until`, last HTTP status class, last poll time) and the last parse summary through `SourceHealth`; have plugins report a failed poll to a shared status instead of discarding it.
- Tracked?: not found (SRC-14a/14b decisions cover egress status and the unenforced schedule/agents only).

### SRC-07 BitTorrent unverified items have no cap and no expiry, so a source can grow the store and the id space without bound
- Severity: important
- Kind: resource
- Confidence: confirmed for the missing bound; the growth figures are arithmetic from the caps, not measured
- Where: `crates/store/src/unverified.rs:75-84,431-433,1398-1412` (`USENET_CAP` and `cap_network`, applied to the Usenet tier only); producers: `crates/sources-plugins/src/prowlarr.rs:98-103,739-748` (up to 1,000 items a page, per indexer, every 15 minutes), `nyaa.rs:75-79` (300 a poll, every 5 minutes), `full_scrape.rs:652-663` with `crates/tracker-scrape/src/full.rs:62-68` (every scraped key carries a tracker claim, so each becomes a row: up to 4,000,000 per tracker per day), `arr.rs` behind `crates/api/src/webhooks.rs:72-75` (one a second per integration)
- What: a key with claims that isn't stored yet becomes an `unverified_items` row with an id from `id_seq`. The plan caps "bare dead hashes" at 1M (Q11), and `docs/decisions/2026-11-02-SRC-2.md:68-69` meets that through the pool's `max_pending` for keys without claims. Keys with claims have no such bound: nothing evicts or ages a BitTorrent unverified item, and with `sources.verify_unverified` off (the default) none is fetched for verification. An item leaves only when the crawler happens to fetch the same hash (promotion), on a rule hit or hold, or when its source is purged; a made-up hash never does.
- Failure scenario: one hostile public indexer behind Prowlarr answers each poll with 1,000 fresh random hashes and titles that trip no rule: 96,000 rows a day, about 35 million a year, each also taking an id (`2026-11-02-SRC-2.md:124` measured +22.5 MiB of hot-column memory per million interleaved ids). A hostile tracker in the full-scrape list adds 4 million a day. The refile sweep and the verification and torrent-cache cursors then walk all of it.
- Suggested fix: a per-source and a total cap on BitTorrent unverified items, oldest first as for Usenet (`cap_network` already takes a network), and an age-out for items never seen again; count evictions per source so SRC-06's health can show a flooding source.
- Tracked?: `X-unverified-cap-index` (`runner/slices.yaml:766`) is about the Usenet cap's index only. Nothing found for the BitTorrent tier.

### SRC-08 `Retry-After` as an HTTP date is ignored
- Severity: polish
- Kind: correctness
- Confidence: confirmed
- Where: `crates/sources/src/http.rs:548-570`
- What: `parse_retry_after` reads delta-seconds only; a date reads as absent. A 429 or 503 with a date then counts as `Verdict::Retry` and is retried after the plain back-off (60 s first for most plugins), which can be much sooner than the server asked.
- Failure scenario: archive.org answers `503` with `Retry-After: <a date one hour ahead>`; the plugin asks again after 60 s, 120 s, 240 s.
- Suggested fix: parse the IMF-fixdate form (the crate already has `rfc2822` in `sources-feeds/common.rs`) against the response's `Date` header or the wall clock, capped at `MAX_RETRY_AFTER`.
- Tracked?: the limitation is stated in the doc comment; no decisions entry found.

### SRC-09 Large untrusted bodies are parsed on async worker threads
- Severity: polish
- Kind: resource
- Confidence: confirmed
- Where: `crates/sources-plugins/src/prowlarr.rs:740` (Torznab page, up to 16 MiB, up to 8 agents at once), `:705` (indexer list, 8 MiB), `nyaa.rs:237`, `archive_org.rs:257` (16 MiB JSON), `torrent_cache.rs:887,907-922` (an 8 MiB `.torrent` parsed twice and hashed), `clients.rs:458-460`
- What: these parsers run inline in the plugin's task. Wikidata alone moves its 8 MiB parse to `spawn_blocking` (`wikidata.rs:218`), with the reason stated. A hostile or broken manager that sends 16 MiB pages to 8 agents holds runtime workers for the parse time while the DHT and the API share the runtime.
- Suggested fix: parse bodies above a small threshold on the blocking pool, as Wikidata does.
- Tracked?: not found.

### SRC-10 The Account gate is met by any stored credential name, not by the ones the plugin declares
- Severity: polish
- Kind: gap
- Confidence: confirmed (latent)
- Where: `crates/sources/src/consent.rs:213-226` (`need`), `:269-273` (the comment that says so), `crates/pipeline/src/control.rs:1442-1477` (names checked by format only)
- What: `need` returns `None` for an Account source as soon as one non-egress credential of any name is stored. `PATCH /sources/{id}/credentials` accepts any name that passes `check_name`, declared or not. So Prowlarr can be enabled with a credential called `x` and no `api_key`; it then fails at run with "needs setup".
- Suggested fix: require every `CredentialSpec { required: true }` name in `need`, and refuse undeclared names at write.
- Tracked?: slice `X-consent-gate-lift` (`runner/slices.yaml:678`); the code comment names it.

### SRC-11 No plugin is behind a cargo feature
- Severity: polish
- Kind: gap
- Confidence: confirmed
- Where: `crates/sources-plugins/Cargo.toml:11-15` (only `testing`), `crates/sources-plugins/src/lib.rs:22-33`; spec: `docs/architecture.md:2959`, `docs/plan/2026-11-01-v06-sources.md` §1 and §4.1 ("each behind its `src-*` cargo feature")
- What: all twelve manifests are compiled into every build, including the adult-enrichment plugins that are "on hold" and the nine that the binary does not register.
- Suggested fix: add the `src-*` features the plan lists, or record the decision not to.
- Tracked?: not found in `docs/decisions/`.

## Checked and found sound
- **Filter before storage, fail closed.** Every discovery with claims is refused at intake if its hash is held or on a blocking list (`pipeline/src/lib.rs:1608-1617`), then judged by the full engine on name with files, description, publisher, details texts and adult-section texts before any write (`unverified.rs:885-1033`); usenet items the same without a hash. A store or check error stores nothing. `tests/safety.rs` (SRC-G) drives every plugin kind through the real pipeline.
- **Verified metadata is never overwritten by claims.** Claims live per `(torrent, source)`; promotion keeps the verified name, files and sizes and only adds the `claim-mismatch` reason (`unverified.rs:1536-1562`). Enrichment answers are refused unless the key is a stored, public torrent the runtime asked about (`:955-976`), and only listed enrichment sources may attach an adult section (`:931-950`).
- **What "verified" means for a torrent cache.** The info dict is re-parsed from its raw span with no encoding hint and must hash (SHA-1, or truncated SHA-256) to the key, in the plugin (`torrent_cache.rs:907-975`) and again in the pipeline (`pipeline/src/torrent_cache.rs:360-369`), then takes the same filter path as a BEP 9 fetch. Fields outside the hash (comment, trackers) are claims only.
- **No source-supplied URL is ever fetched.** GUIDs, links and enclosures are stored (after `external_ref` strips credentials and odd hosts) and never requested; claimed trackers are validated by `public_tracker_url` and never dialled (`docs/decisions/2026-11-05-X-claim-trackers.md`). Redirects are never followed by `Fetch`; `send_following` re-checks every hop against the endpoint list, refuses https→http and drops all but five headers across origins (`egress.rs:1244-1286`). The scene link the API shows must be `https` on the plugin's own endpoint (`api/src/torrents.rs:1050-1070`).
- **Egress is pinned per plugin.** Fixed-endpoint plugins get `Endpoints::manifest` (public addresses, exact scheme/host/port); user-entered ones are narrowed to the configured origins (`prowlarr.rs:472-475`, `clients.rs:165-169`, `full_scrape.rs:447-451`, `torrent_cache.rs:779-790`). The torrent-cache template check (`torrent_cache.rs:310-409`) refuses `%`, `\`, userinfo, non-plain hosts and anything the URL parser would rewrite, and the consent version covers the full template.
- **Private finds stay private.** Prowlarr treats anything but `public` as private (`prowlarr.rs:1316,1386`); webhook and client finds are always `private_source`; `store_claims` drops `external_ref` and trackers for private finds in one place (`unverified.rs:300-328`); `Lookup::new` cannot be built for a private key and the pipeline's gate re-checks at send time.
- **Parsers.** Torznab and RSS stream with `quick-xml`, stop on `DOCTYPE`, resolve only character references and the five XML entities, and cap bytes, depth, elements, items, attributes and text (`sources-feeds/src/{torznab,rss,common}.rs`). The JSON reader is iterative for skips, depth- and value-capped, and checks raw string length before decoding (`json.rs`). I found no reachable index, slice or arithmetic panic in `common.rs`, `torznab.rs`, `rss.rs`, `json.rs` or the plugin mappers; sizes and dates are range-checked before use; the Jackett list parser caps depth, elements and attributes (`prowlarr.rs:1346-1485`). Every parser has a registered fuzz target (`fuzz/Cargo.toml`: `torznab`, `rss`, `json`, `archive_org`, `stashdb`, `tpdb`, `wikidata`, `prowlarr`, `arr_webhook`, `client_list`, `full_scrape`, `torrent_cache`, `verified_claims`, `claims`).
- **HTTP hygiene.** 16 MiB body cap on buffered answers and plugin-declared caps on streams, enforced twice (`http.rs:275,433-503`); per-host GCRA with global floors that no setting can loosen (`manifest.rs:337-423`); 401/403 and `Retry-After` block the host or unit; back-off is jittered and bounded; `Debug` for requests, responses and headers redacts by allow-list and URLs to scheme and host (`http.rs:107-190`). API keys are format-checked before they go into a header or URL (`prowlarr.rs:610-614`). Wikidata sends `maxlag` and a project User-Agent.
- **Kill switch and off-switch.** A request re-reads the source's stored state and the route before sending and is paced when refused (`egress.rs:859-918`); in-flight requests and streamed bodies abort when the route drops.
- **Shutdown.** Every plugin run loop selects on `shutdown.requested()` around polls, sends and sleeps; Prowlarr's agents are a `JoinSet` dropped on exit.
- **Tests wait correctly.** The sleeps in `crates/sources/tests/*`, `egress_tests.rs` and the plugin tests run on paused time or inside a 600 × 25 ms condition poll (`tests/support/harness.rs:55-64`); the one `#[ignore]` is a labelled measurement.
- **Schedule windows and the agents setting are stored but not enforced**, and the API says so (`enforced: false`): recorded in `docs/decisions/2026-11-02-SRC-14a.md:79-87`.

## Area-level concerns
- **Most of this area has never run in the product.** Nine of twelve plugins are unregistered and two of the other three are behind the consent gate. The contract tests are thorough at plugin level, but SRC-01, SRC-03, SRC-04 and SRC-05 are all integration faults that plugin-level tests cannot see. Before X-consent-gate-lift, one end-to-end test per plugin through `Pipeline::start` (restart included) would catch this class.
- **In-memory pacing is the single point every politeness cap rests on.** Poll gates, daily quotas, `UsenetPolls`, the torrent cache's `next_poll` and Wikidata's `not_before` all reset on restart (SRC-01, SRC-04).
- **One shared intake.** All sources feed one bounded channel read by one task; a discovery with claims waits there for the 64 MiB claims budget and the writer (`pipeline/src/lib.rs:1620-1636`). A feed plugin that emits a large batch with the waiting `emit` (a full scrape, a 1,000-item Torznab page) can hold the intake while the DHT's `try_emit` drops. I did not measure it; `docs/decisions/2026-11-02-SRC-2.md:65` records the related per-source pending cap as a hand-off that has no slice.
- **A junk reason from one source's claims lands on the verified torrent.** `claim-mismatch` (weight 30 of 100, `classify/src/junk.rs:52`) is added when a pre-claimed file list contradicts the verified one. A hostile feed can pre-claim hashes with wrong file lists to demote torrents it does not own. Today no registered plugin supplies file claims for public finds, so this is a design note for AnimeTosho and later feeds.
- **Enrichment identity comes from open-edit data.** Wikidata matches need an exact title, kind and (when known) year and a single candidate, and every text is judged, which is careful; still, anyone can add an alias to an item. Worth stating in the ENR-13 drawer copy (PR 793) that the identification is a third party's.
- **A plugin that ends with `Err` stays down until restart** (no supervisor outside the companion gate). Prowlarr treats a 401/403 on any one indexer page, or a 404/redirect on the list, as fatal (`prowlarr.rs:586-603,802-811`); a reverse proxy that answers 403 or 404 briefly stops the source for good. Together with SRC-06 the user sees only the last error string.
