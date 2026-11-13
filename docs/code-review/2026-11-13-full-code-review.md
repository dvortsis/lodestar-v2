# Full code review, 2026-11-13

Reviewed `main` at `55e155a9` (2026-11-12T23:19Z). This is a review record, not a list of completed fixes. Nothing here changes code.

**203 findings: 16 rated blocking by their reviewer, 100 important, 87 polish.** 156 are marked confirmed (the code path was traced end to end), 44 likely and 3 suspected. 153 of them were not found in `runner/slices.yaml`, `docs/decisions/`, `docs/build-plan.md` or an open PR.

## How it was done
- **Twenty area reviews,** one per subsystem, each read-only and each with its own report in [`2026-11-13-full-code-review/`](2026-11-13-full-code-review/). Each report lists what was read in full, what was skimmed and what was not read, then its findings, what it checked and found sound, and area-level concerns.
- **Nothing was built or run.** Every finding comes from reading the code. Memory and time figures are estimates from the code, not measurements.
- **A second read by the coordinating session** of the findings in "What to act on first" below. Each row says what that second read covered. The other findings carry their reviewer's confidence only.
- **A dependency sweep** by the coordinating session: `Cargo.lock` against the RustSec advisory database (cloned 2026-11-12), and `npm audit` on `web/` and `tools/status-connector`.
- **Severity** follows `runner/standards.md` §5 as each reviewer applied it. Where the second read disagrees, the table below says so. The reports keep the reviewer's original rating.

## What to act on first
Ordered by what is at stake: content safety first, then availability under hostile input, then wrong results and lost settings, then the release pipeline.

| # | Finding | Ids | Second read |
|---|---|---|---|
| 1 | **The child-safety core judges a truncated view.** It reads the first 128 tokens of a name or path and the first 1,000 files. The full name (up to 4,096 bytes) and every path (up to 200,000 files) are stored, shown and searchable. About 135 characters of filler use up the token cap. Both caps fail open | FILTER-01 | Confirmed: `MAX_TOKENS = 128` (`crates/filter/src/normalize.rs:28`), `take(MAX_FILES)` with 1,000 (`crates/filter/src/core.rs:461`, `:991`), against `max_files: 200_000` in `crates/metainfo/src/info.rs` |
| 2 | **CS-8a, the locked floor's "renamed repost" rule, can never fire.** Nothing in production writes a payload signature: `Store::add_cs_sig` is called only from tests, so Report abuse and Confirm record the hash and not the signature | FILTER-02 | Confirmed: the only non-test writers of `cs_sigs` are `add_cs_sig` itself and the snapshot carry |
| 3 | **A rule or list change is never applied to torrents already stored.** A hash that joins the floor through a blocklist file or a floor-marked subscription blocks new intake only. The torrent already stored under it stays stored, indexed and exportable. §20.3's retroactive re-evaluation is not built | FILTER-03 | Confirmed: the only production callers of `delete_torrents` are Report abuse, Confirm and the cluster intake |
| 4 | **An update rollback restores the old store without the content-safety holds or deletions recorded since.** The automatic rollback, in-app Roll back and `lodestar rollback --update` scrub source purges only. `purge_for_restore`, which carries deletions, reported hashes, tombstones, quarantine and blocklist subscriptions, runs only for `lodestar rollback <snapshot>`. Both Windows rollbacks scrub nothing. Two reviewers found this independently | UPDATE-01, STORE-01, UPDATE-05 | Confirmed: `choose_snapshot` in `crates/lodestar/src/update/journal.rs` calls `apply_source_purges` only; `purge_for_restore` has one production caller, `cli/rollback.rs` |
| 5 | **A `flag` or `demote` rule of your own replaces what a pack or list would have done, with no warning or password.** Your rules decide before packs (§20.1), and adding a rule counts as strengthening (§20.11), so a broad light rule switches packs off for everything it matches. The core still decides first | APIFEAT-01 | Confirmed, and re-graded: the code follows §20.1's order, so this is a gap in §20.11's list of weakening changes. It needs an owner decision, not a bug fix |
| 6 | **One DHT host can fill the fetch pool with keys that can't be evicted.** An `announce_peer` is harvested with the announcer as its only peer. Keys that have a peer are never evicted. A host that announces random hashes and then stalls connections fills `max_pending`, and new discoveries from every source are dropped | PIPE-01 | Code path confirmed (`evict_one` skips `Ready`; announces pass `harvest_hash` with the peer). The fill rate is the reviewer's estimate; not exercised |
| 7 | **Any inbound DHT query spends the node's outbound query budget.** The verification ping for a new querier is sent before any inbound limit and draws on the same bucket as crawling. A modest flood from spoofed addresses stalls the crawl | DHT-01 | Confirmed: `crates/dht/src/node/actor.rs` sends the ping before `reply_rate.try_take` |
| 8 | **One hostile torrent can stall the single indexer thread.** Building its search document checks each file's extension against a growing `Vec`, which is quadratic in distinct extensions, with up to 200,000 files | SEARCH-01 | Confirmed: `exts.contains(&ext)` per file in `crates/search/src/doc.rs`. The stall time is estimated |
| 9 | **The store's writer task can panic, and a stage panic is not contained.** `Store::write` calls `Inserted::id()` on entries that may be `Refused` when the classification generation is stale. A panic in a stage stops ingest silently, and the next shutdown saves an empty fetch queue over the last snapshot | STORE-02, PIPE-02 | STORE-02 confirmed at `crates/store/src/lib.rs:764-775`. PIPE-02 not re-read |
| 10 | **Some saved settings are dropped at every start.** Saved rows are validated one at a time in key order, so a field whose check depends on a later field is judged against that field's default. The logging data preset ("Hashes, until I turn it off") and `file_size_mib` above 256 revert after any restart | LOGSET-01 | Confirmed: the load loop in `crates/lodestar/src/settings/mod.rs` and the checks in `settings/logging.rs` |
| 11 | **Sorted `sig:<hash>` searches can return stale results.** The per-segment browse cache key has no part for the reference torrent, which can live in another segment | SEARCH-02 | Cache key confirmed (`crates/search/src/sorted.rs`, `index.rs`). The failure was not run |
| 12 | **A worker that returns within the lease window strands the leases it lost.** PR 779 (WK-11b) fixes it, but the reviewer reads its trigger as firing only after a spool segment is removed, so a restart with an empty spool would not send the list | CLUSTER-01 | Not re-read. Check the trigger in PR 779 before it merges |
| 13 | **A companion's credential goes to whatever answers on its loopback port.** A refused request to `127.0.0.1` falls back to `[::1]`, and nothing ties the listener to the supervised process. It needs a local process, and the managed path is unreachable until the first pin lands | CMP-01 | Fallback confirmed in `crates/companions/src/detect/mod.rs`. Re-graded to important: local attacker, latent today. Fix before the first pin |
| 14 | **No notification is ever delivered, and the digest is never sent.** Senders, routing settings, "Send test" and the digest preview all exist. Nothing dispatches. The settings screens show these as live. Three reviewers reached this from different sides | INTEG-01, LOGSET-03, APIFEAT-04, APIFEAT-03 | INTEG-01 confirmed: `notify::deliver` has no production caller and nothing reads `route_alerts`, `route_updates`, `route_health` or `digest_channels` |
| 15 | **"Matched in N files" never shows more than 20.** The row prints the length of a list the server caps at 20. It is also a count computed in the client, which `runner/standards.md` §4 forbids | WEBCORE-01 | Confirmed: `MATCHED_FILES = 20` in `crates/api/src/search.rs`; `hit.matched_files?.length` in `ResultRow.svelte` |
| 16 | **Three open advisories in `Cargo.lock`, and no advisory check in CI.** See "Dependency sweep" | INFRA-09 and the sweep | Done by the coordinating session |
| 17 | **The release pipeline has gaps the binaries' gate doesn't cover.** The Docker image, `:latest` included, is pushed on any `v*.*.*` tag with no environment approval. A backport tag moves `:latest` backwards. Release binaries are built with a floating `stable` while Docker pins 1.97.1. `freshness.yml` re-signs whatever manifest is on the release | INFRA-01, INFRA-03, INFRA-08, INFRA-02, INFRA-04 | Not re-read. Four rest on GitHub settings or action behaviour that the repository doesn't show, and their reports say which |
| 18 | **The updater's safety nets have holes beyond row 4.** Stored key revocations are never consulted. A rollback keeps a search index that is then ahead of the store. The supervisor's three-attempt cap is lost when `pending` can't be written, which gives an endless restart loop | UPDATE-02, UPDATE-03, UPDATE-04, STORE-03 | Not re-read |
| 19 | **The safety net has gaps where the risk is highest.** The peer-wire parsers have no fuzz target and no property test. Three real-server Playwright specs match no CI project and never run. CI fuzzing is 60 s per target from an empty corpus. At least six tests assert after a fixed sleep, which the project's own standard calls blocking | WIRE-05, TEST-03, TEST-01, TEST-04, TEST-02, DHT-03, LOGSET-04, WEBCORE-05 | Three of the DHT sleeps confirmed (`crates/dht/tests/{scrape,network,harvest}.rs`) |

**Re-graded by the second read.** APIFEAT-01 and CMP-01 as above. DHT-02 (transaction ids are a counter) goes from blocking to important: forging a reply also needs the source address of a node the crawler has just queried. DHT-03 is blocking only under the test standard.

## Themes
- **Content safety is enforced at intake and almost nowhere after it.** Rows 1 to 5 share a cause: the filter is a gate on new items. Nothing re-judges what is stored when rules change, when a snapshot comes back, or when a cap cuts the read short. FILTER-07 (a floor change between a batch's judgement and its commit) and SRC-02 (a hit on any source's claim text deletes the verified torrent, a recorded decision awaiting the owner) belong here too.
- **Built, shown in the UI, and wired to nothing.** Notifications and the digest (row 14), the "Search API" key scope (APISEC-01), "Send selected files" and `.torrent` sends (INTEG-03), the store-backed plug-in state (SRC-01), the trained file-tree dictionary the size budget assumes (STORE-04), `interrupt_import_jobs` (IMP-03), CA rotation (CLUSTER-09), Prowlarr's hand-off to its source (CMP-05), Insights Serve (APIFEAT-03). Most are noted as follow-ups in a decisions file and have no slice.
- **Limits that count items, not cost.** PIPE-01, PIPE-04, DHT-04, DHT-10, DHT-11, CLUSTER-02, SRC-07, APIFEAT-05, WIRE-03, FILTER-08, FILTER-09: a bound exists, but it is per count or global, so one peer, worker or source can take all of it.
- **Restart and failure paths are the least exercised.** LOGSET-01, PLAT-01, PIPE-02, PIPE-03, UPDATE-04, SEARCH-03, SEARCH-06, STORE-03, IMP-03, SRC-01, SRC-04, CLUSTER-05, PLAT-04. Few of these have a test that restarts or injects the failure.
- **Windows trails Unix on file permissions and rollback.** PLAT-02, PLAT-03, UPDATE-05, UPDATE-06, and PLAT-05 for Unix outside Docker.
- **Documents and gates have drifted from the code.** The detail drawer's size gate is 28,064 B against a documented 15 KB (TEST-05). 60 of 164 metric families are missing from `docs/metrics.md` (PLAT-06). Three query fields are missing from `docs/query-language.md` (SEARCH-08). Most §3 performance budgets are not a required check (TEST-06).

## Dependency sweep
CI runs no advisory check (INFRA-09), so this was done by hand.

| Crate | Version | Advisory | Exposure here |
|---|---|---|---|
| `libsqlite3-sys` | 0.38.2 (bundled SQLite) | RUSTSEC-2026-0459: a crafted database with a `-wal` file of a different page size can return uninitialised memory. Fixed in 0.39.0 with `rusqlite` 0.41 | **Relevant.** The Magnetico importer opens SQLite files the user supplies (`crates/sources-importers/src/magnetico.rs`). It opens read-only with `NOFOLLOW`, defensive mode and untrusted schema, none of which addresses this |
| `ctutils` | 0.4.2 | RUSTSEC-2026-0385: comparisons meant to be constant-time can branch. Fixed in 0.4.4 | Reaches the build through `digest` and `phc`, under `argon2` (password hashing). Low, but a one-line bump |
| `rustix` | 1.1.5 | RUSTSEC-2026-0486 (unsound): the xattr functions with a zero-length buffer. Fixed in 1.1.6 | Not reached: nothing in `crates/` calls an xattr function |

- **`web/`, shipped bundle (`npm audit --omit=dev`):** 7 low, nothing higher (`@sveltejs/kit`, `cookie`, `deepmerge`, `bits-ui` and their dependants).
- **`web/`, development tooling:** 19 high and 8 moderate, all under `@lhci/cli` and `@size-limit/preset-app` (puppeteer, `tmp`, `extract-zip`, `basic-ftp`). They run in CI and on developer machines, not in the product.
- **`tools/status-connector`:** 6 high and 1 moderate (`wrangler`, `miniflare`, `undici`, `ws`, `sharp`, `zod`, `yaml`).
- **Pins.** `runner/standards.md` §2 asks for exact pins. Most manifests use caret requirements, held only by `Cargo.lock` (PLAT-12).

## Needs an owner decision
Each of these reverses or loosens nothing by itself, but the fix touches content safety or a number the owner set, so CLAUDE.md sends it to the owner.
1. **FILTER-01:** should the core's caps fail closed (hold as "too large to judge") or be lifted for the core?
2. **APIFEAT-01:** should a user rule with a lighter action than a pack count as weakening (warning and password), or should the engine keep the strongest decision?
3. **FILTER-03:** what should happen to a stored torrent when its hash joins the floor: delete at once, or queue for review?
4. **UPDATE-01, UPDATE-05:** confirm that every restore entry point must carry holds and deletions, Windows included. PR 795 would add a third caller of the unscrubbed Windows restore.
5. **SRC-02:** the recorded decision that a core hit on any source's claim text deletes the verified torrent was flagged to the owner in `docs/decisions/2026-11-02-SRC-2.md` and no answer was found. It should be settled before X-consent-gate-lift widens what sources can run.
6. **INFRA-01:** should the image publish wait for the same `release` environment approval as the binaries?
7. **TEST-05:** the detail drawer budget. The gate is 28 KB and the architecture says 15 KB. One of them should change, under §2.26.

## Already in flight
- PR 779 (WK-11b) covers most of CLUSTER-01. See row 12 for the gap.
- PR 797 (WK-18) fixes two of CLUSTER-06's four points.
- PR 795 (X-win-upd-manual-rollback) is the Roll back half of UPDATE-06, and inherits UPDATE-05.
- PR 788 (X-utp-backoff) covers two uTP behaviours the wire review noticed, so they are in its area notes and not findings.
- PR 781 (X-unraid-validate-all) is INFRA-14.
- PR 790 (X-restart-save-lag) is next to WEBFEAT's restart notes.
- PR 778 (X-test-blockers-1112) does not touch the fixed sleeps listed here.
- CLUSTER-04 is WK-12, WK-13 and WK-G. TEST-06's 10M measurements are M8-PERF-1 to 3.

## Coverage and limits
- **Read-only.** No build, no test run, no fuzzing, no live system. A finding marked confirmed was traced through the code, not reproduced.
- **Not everything was read.** The workspace is about 570k lines of Rust and 120k of web source. Each report's "Coverage" section says what was skipped. The largest gaps: most test code; `crates/search/src/files.rs` scorers; `crates/filter` `matcher.rs` and `prefilter.rs` bodies; `crates/sources/src/pool.rs` beyond dispatch and eviction; `crates/log/src/url.rs` free-text rules; `crates/cluster` `assign`, `placement` and `have`; the worker filter snapshot; `infra/operator/injection`; most Playwright spec bodies; the wizard steps, Compare and Appearance screens; `docs/user/`.
- **GitHub settings are invisible from the repository.** Rulesets, environments, secrets and branch protection were not checked. INFRA-02, -03, -04 and -07 depend on them.
- **No user documentation review.** `docs/user/`, `docs/operations.md` and the README were not read against the app.
- **A second review ran in parallel** in another session on 2026-11-12 (UI capture and design). Its findings are not merged here. The r4 plan already takes two fixes from it (X-watch-stall-retry, X-utp-spurious-rto).

## All findings
Severity is the reviewer's. "Tracked?" is what the reviewer found in `runner/slices.yaml`, `docs/decisions/`, `docs/build-plan.md` and the open PRs: `no`, `partly`, or `yes or recorded` (a slice, a PR or a decisions file names it; the report says which).

### Wire parsers and small protocol crates
[`01-wire-parsers.md`](2026-11-13-full-code-review/01-wire-parsers.md): 0 blocking, 5 important, 1 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| WIRE-01 | important | confirmed | `fetch.max_metadata_mib` accepts 1–64, but the info parser rejects anything over 16 MiB | no |
| WIRE-02 | important | confirmed | HTTP scrape client follows tracker redirects to loopback, private and link-local addresses | no |
| WIRE-03 | important | likely | One hostile info dict can cost about 100 MiB of decoder memory, outside the fetch reservation | no |
| WIRE-04 | important | likely | A legacy `name` escapes the 4,096-byte cap when `name.utf-8` is present, and reaches encoding detection whole | no |
| WIRE-05 | important | confirmed | peer-wire has no fuzz target and no property test | no |
| WIRE-06 | polish | confirmed | uTP delivers one buffered packet past a FIN | no |

### DHT crawler
[`02-dht.md`](2026-11-13-full-code-review/02-dht.md): 3 blocking, 9 important, 3 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| DHT-01 | blocking | confirmed | Any inbound query can spend the node's outbound query budget (verification ping before any limit) | no |
| DHT-02 | blocking | confirmed | Transaction ids are a counter, so any queried node can predict them and forge replies | no |
| DHT-03 | blocking | confirmed | Fixed sleeps that assume a join has finished, then assert on it | no |
| DHT-04 | important | likely | Reply lists are not capped, so one reply can fill the sampler frontier and pin its cooldown table | no |
| DHT-05 | important | likely | IPv6: Sybil limits are per address, and external-IP votes cross families | no |
| DHT-06 | important | confirmed | Bootstrap router names are resolved once; a start without DNS never joins | no |
| DHT-07 | important | confirmed | A thin table in one family re-queries every family's bootstrap routers every 30 s, with no backoff | no |
| DHT-08 | important | likely | A stale bucket that cannot change is refreshed again on every tick | no |
| DHT-09 | important | confirmed | BEP 42 is never applied to other nodes' ids, though the module says it is | no |
| DHT-10 | important | likely | Maintenance lookups have no query cap and no per-IP diversity | no |
| DHT-11 | important | confirmed | Inbound replies have only a global rate limit; one source can take it all, and replies amplify | no |
| DHT-12 | important | confirmed | The passive path and the reply limiter have no test, and the simulator sends no inbound queries | partly |
| DHT-13 | polish | confirmed | `sample_infohashes` replies advertise hashes we only saw, not hashes we store | no |
| DHT-14 | polish | confirmed | `dht-state.bin` is written and fsynced on the actor task; a corrupt file is dropped silently | no |
| DHT-15 | polish | confirmed | Smaller items | no |

### Store
[`03-store.md`](2026-11-13-full-code-review/03-store.md): 1 blocking, 3 important, 6 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| STORE-01 | blocking | confirmed | Update rollback restores a pre-migration snapshot without the store's deletions and content-safety holds (only source purges are applied) | no |
| STORE-02 | important | confirmed | `Store::write` panics on a refused prepared torrent when the classification generation is stale, killing the writer task | no |
| STORE-03 | important | likely | After a power loss the store can fall behind the search index, and nothing detects it: ids are then reused under stale index documents | no |
| STORE-04 | important | confirmed | No production code trains or applies the file-tree dictionary the architecture and its size budget assume | no |
| STORE-05 | polish | likely | A migration that fails after its snapshot leaves one more full-size snapshot on every retry | no |
| STORE-06 | polish | likely | `PRAGMA journal_mode=WAL` is set but its result is never checked | partly |
| STORE-07 | polish | confirmed | Review-queue and tombstone pages sort the whole table on every page | no |
| STORE-08 | polish | suspected | A verified hybrid can make `upgrade_partial` hit the `torrents_v2` unique index and fail the whole batch | no |
| STORE-09 | polish | confirmed | Per-batch `count(*)` scans on the import and usenet write paths | no |
| STORE-10 | polish | confirmed | `writes_wait_for_another_writer` relies on a fixed 300 ms sleep and can pass without testing anything | no |

### Search
[`04-search.md`](2026-11-13-full-code-review/04-search.md): 2 blocking, 6 important, 4 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| SEARCH-01 | blocking | confirmed | Building a torrent's document is quadratic in its distinct file extensions, and they are indexed without a cap | no |
| SEARCH-02 | blocking | likely | Sorted `sig:<info hash>` searches serve stale per-segment results from the browse cache | no |
| SEARCH-03 | important | confirmed | One stored torrent that fails to load stalls indexing, alerts and any rebuild indefinitely | no |
| SEARCH-04 | important | confirmed | Decomposed (NFD) Japanese and Korean names never match their composed queries | no |
| SEARCH-05 | important | confirmed | `/search` has no server-side time budget | partly |
| SEARCH-06 | important | likely | A rebuild that retries a failed step can drop a torrent from the new index | no |
| SEARCH-07 | important | confirmed | A rebuild never checks free disk space | no |
| SEARCH-08 | important | confirmed | `verified:`, `source:` and `network:` are parsed but absent from `docs/query-language.md` | partly |
| SEARCH-09 | polish | confirmed | `docs/query-language.md` promises more than paging and ranking deliver | yes or recorded |
| SEARCH-10 | polish | likely | Zero-width and other format characters split words instead of being dropped | no |
| SEARCH-11 | polish | confirmed | A file-index error makes `-file:(…)` match everything | yes or recorded |
| SEARCH-12 | polish | confirmed | Small parser edges | no |

### Ingest pipeline
[`05-pipeline.md`](2026-11-13-full-code-review/05-pipeline.md): 1 blocking, 3 important, 4 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| PIPE-01 | blocking | likely | Any DHT peer can pin the fetch pool: keys whose only peer is one busy IP stay `Ready`, are never evicted, and are re-scanned on every loop pass | no |
| PIPE-02 | important | confirmed | A panic in one stage is not contained: ingest stops silently, and the next shutdown wipes the saved fetch queue | no |
| PIPE-03 | important | confirmed | A failed store write drops the whole ingest batch with its drained counters, and nothing slows fetching | partly |
| PIPE-04 | important | confirmed | The emitter-to-intake channel is bounded by count only, so the claims byte budget leaves up to 16,384 discoveries of up to 1 MiB unbudgeted | no |
| PIPE-05 | polish | likely | A purged key's in-flight fetch result is applied to a later entry for the same key, which can leak a lookup slot | no |
| PIPE-06 | polish | confirmed | An upload is answered `Accepted` once queued, not once committed | no |
| PIPE-07 | polish | confirmed | Shutdown waits for plugins one after another, 15 s each | no |
| PIPE-08 | polish | confirmed | Two health tests assert "nothing happened" after a fixed 300 to 400 ms sleep | no |

### Content filter
[`06-filter.md`](2026-11-13-full-code-review/06-filter.md): 3 blocking, 6 important, 2 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| FILTER-01 | blocking | confirmed | The child-safety core reads only the first 128 tokens of a name or path and only the first 1,000 files; the rest is stored, shown and searchable unjudged | no |
| FILTER-02 | blocking | confirmed | CS-8a (locked floor, renamed reposts) can never fire: nothing in production ever stores a CS-8 payload signature | no |
| FILTER-03 | blocking | confirmed | A hash that joins the floor or a dropping list from a file or a subscription never removes the torrent already stored under it; no rule change is applied to stored torrents at all | no |
| FILTER-04 | important | confirmed | Look-alike mapping covers about 36 letters, not the UTS #39 skeleton §20.4 promises; several cheap spellings pass the core | no |
| FILTER-05 | important | confirmed | The built-in exceptions cancel explicit evidence more widely than §20.5.4 describes: one context word in a name, or a rating word before an age | partly |
| FILTER-06 | important | confirmed | A subscribed list is cut at 4,000,000 entries and its invalid lines dropped with no trace | no |
| FILTER-07 | important | likely | A floor change between a batch's judgement and its commit is not seen, and nothing repairs it afterwards | no |
| FILTER-08 | important | likely | Per-torrent token map uses an unkeyed, invertible hash as its table hash: a crafted file list makes one judgement quadratic | no |
| FILTER-09 | important | likely | Decoding a list holds the download, the expansion and a second copy at once, three times the expansion when it isn't UTF-8 | no |
| FILTER-10 | polish | confirmed | A stored list blob that doesn't decode is skipped in silence | no |
| FILTER-11 | polish | confirmed | Two percolator tests wait on a condition with no deadline | yes or recorded |

### API: auth and security surface
[`07-api-security.md`](2026-11-13-full-code-review/07-api-security.md): 0 blocking, 3 important, 3 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| APISEC-01 | important | confirmed | The "Search API" key scope reaches no route | no |
| APISEC-02 | important | likely | No Host-header check: DNS rebinding reaches the open routes and `/login` from any website | no |
| APISEC-03 | important | likely | Concurrent Argon2 runs are not capped: many client addresses can exhaust memory and the blocking pool | no |
| APISEC-04 | polish | confirmed | OpenAPI marks the token routes as session routes, and its "served routes" test compares two hand lists | yes or recorded |
| APISEC-05 | polish | confirmed | `Auth` and its state derive `Debug` over the password hash, the session key and the setup token | no |
| APISEC-06 | polish | likely | Behind a proxy that rewrites `Host`, every write fails `cross_origin`, and the guide does not say so | no |

### API: feature handlers
[`08-api-features.md`](2026-11-13-full-code-review/08-api-features.md): 1 blocking, 5 important, 6 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| APIFEAT-01 | blocking | confirmed | Adding a `flag` or `demote` rule switches off packs and lists for what it matches, with no warning or password | no |
| APIFEAT-02 | important | confirmed | False positive, Confirm and Reveal name only work on the newest 100,000 quarantine rows | no |
| APIFEAT-03 | important | confirmed | Insights Serve is permanently null although its counters are recorded | yes or recorded |
| APIFEAT-04 | important | confirmed | The digest is rendered and previewed but never sent; its schedule settings do nothing | yes or recorded |
| APIFEAT-05 | important | confirmed | Collections have no size or count cap, and every list and page checks every entry under a shared store lock | no |
| APIFEAT-06 | important | likely | A session's event streams are refused for up to about 15 minutes after four connections die without closing | no |
| APIFEAT-07 | polish | confirmed | Overview annotations keep the oldest 1,000 and drop the newest, with no flag | no |
| APIFEAT-08 | polish | confirmed | A purge paused past its limit fails as a bare 500 `internal` | no |
| APIFEAT-09 | polish | confirmed | `alerts` events carry real torrent and alert ids while Adult is hidden | no |
| APIFEAT-10 | polish | confirmed | The `docker run` join command puts the token on a command line | no |
| APIFEAT-11 | polish | likely | One saved search whose stored AST no longer deserializes turns the whole list into a 500 | no |
| APIFEAT-12 | polish | likely | The pacer test asserts wall-clock upper bounds | yes or recorded |

### Cluster and remote workers
[`09-cluster-workers.md`](2026-11-13-full-code-review/09-cluster-workers.md): 1 blocking, 5 important, 5 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| CLUSTER-01 | blocking | confirmed | A worker that comes back within the lease TTL strands every lease it lost; the fix under way is triggered only after a spool replay | partly |
| CLUSTER-02 | important | confirmed | No cap on the leases a worker may hold: an authenticated worker can grow the coordinator's lease table and fetch queue without bound | no |
| CLUSTER-03 | important | likely | The worker has no liveness check on the coordinator: a silent link stalls it for as long as the OS keeps retransmitting | no |
| CLUSTER-04 | important | confirmed | "Trust but verify" is not built: nothing a worker says about a torrent's name or files is ever checked, and there is no quarantine or purge | yes or recorded |
| CLUSTER-05 | important | confirmed | Stopping a worker discards its fetched results and reports nothing | no |
| CLUSTER-06 | important | confirmed | Worker packaging: the Unraid template at `main` yields a permanently "unhealthy" container and a token field that cannot be cleared; its Intensity field does nothing | yes or recorded |
| CLUSTER-07 | polish | confirmed | Full scans of the lease table under the shared lock on every heartbeat, tick and intake batch | no |
| CLUSTER-08 | polish | confirmed | The join retry the coordinator supports is unreachable from the worker: a lost reply spends the token and blocks the name | no |
| CLUSTER-09 | polish | confirmed | CA rotation and the coordinator's secrets-key reseal have no caller | no |
| CLUSTER-10 | polish | confirmed | Join attempts are limited by concurrency only; each one takes the PKI lock and reads the store | no |
| CLUSTER-11 | polish | suspected | The workers' have-filter panics on a poisoned lock, and its insert hook sits on the commit path | no |

### Self-updater
[`10-updater.md`](2026-11-13-full-code-review/10-updater.md): 1 blocking, 5 important, 5 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| UPDATE-01 | blocking | confirmed | An update rollback restores the pre-migration snapshot without the deletion journal or the content-safety holds | no |
| UPDATE-02 | important | confirmed | Stored revocations are never consulted, and the "installed under a revoked key" notice can't fire for a rotation learnt from `trust.json` | no |
| UPDATE-03 | important | likely | An update rollback that restores the store keeps the shared search index, which is then ahead of the store | partly |
| UPDATE-04 | important | confirmed | The supervisor's attempt count is lost when `pending` can't be written, so a failing version restarts forever | no |
| UPDATE-05 | important | confirmed | Both Windows rollbacks restore a snapshot with no scrub at all, which breaks SRC-14b's "every restore entry point" condition | partly |
| UPDATE-06 | important | confirmed | Windows inventory: what still answers `not_built_yet` or is silently absent | yes or recorded |
| UPDATE-07 | polish | confirmed | A `trust.json` that can't be saved is silently ignored after a check | no |
| UPDATE-08 | polish | likely | After the documented recovery-key remedy, an existing `trust.json` with any rotation in it reads as damaged for good | no |
| UPDATE-09 | polish | confirmed | File I/O on async workers in the Unix engine | no |
| UPDATE-10 | polish | confirmed | A refused overlay leaves `active` pointing at it, and `<version>.bad` folders are never removed | yes or recorded |
| UPDATE-11 | polish | confirmed | Stale module docs say Windows installs nothing | no |

### Companions
[`11-companions.md`](2026-11-13-full-code-review/11-companions.md): 1 blocking, 5 important, 2 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| CMP-01 | blocking | likely | A companion's credential is sent to whatever listens on its loopback port, including `[::1]` | no |
| CMP-02 | important | confirmed | A native qBittorrent has no downloads folder, so downloads land in its own folder, which Remove deletes | no |
| CMP-03 | important | confirmed | In a container, generated templates never check the host folder against Lodestar's own data folder or other companions' folders | no |
| CMP-04 | important | confirmed | `lodestar companion remove --yes` leaves the managed download client in Integrations | no |
| CMP-05 | important | confirmed | Prowlarr is never handed to the Prowlarr source; the sink for it is unreachable | no |
| CMP-06 | important | confirmed | A native qBittorrent can only run on the direct route; the spec's interface binding and kill-switch cover don't exist for it | yes or recorded |
| CMP-07 | polish | confirmed | `fetch.rs` says downloads use Lodestar's egress route; they always go direct | yes or recorded |
| CMP-08 | polish | confirmed | `reseal_rows` and `is_secret_row` have no production caller | no |

### Integrations
[`12-integrations.md`](2026-11-13-full-code-review/12-integrations.md): 0 blocking, 4 important, 5 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| INTEG-01 | important | confirmed | No notification is ever delivered: alerts, updates, health and the digest have senders, settings and a working "Send test", but no dispatcher | yes or recorded |
| INTEG-02 | important | confirmed | The RSS feed's magnet enclosure is wrong for v2-only torrents | no |
| INTEG-03 | important | confirmed | "Send selected files" and `.torrent` sends are built in every adapter and reachable from nothing | no |
| INTEG-04 | important | confirmed | qBittorrent's file selection skips the shared file-count cap | no |
| INTEG-05 | polish | confirmed | Download clients, Torznab, SABnzbd and NZBGet send credentials over plain `http` to any host, with no warning; ntfy and email refuse that | no |
| INTEG-06 | polish | confirmed | ntfy's plaintext-credential rule checks the name only; email also checks the address it connected to | no |
| INTEG-07 | polish | likely | The metadata address list misses two cloud endpoints that sit in otherwise allowed ranges | no |
| INTEG-08 | polish | confirmed | TLS verification can't be relaxed per target, so a LAN client with a self-signed certificate can only be used over plain `http` | no |
| INTEG-09 | polish | likely | The first write of `secrets.key` needs hard links | no |

### Sources framework and plug-ins
[`13-sources-plugins.md`](2026-11-13-full-code-review/13-sources-plugins.md): 0 blocking, 7 important, 4 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| SRC-01 | important | confirmed | Every plugin gets an in-memory `PluginState`; the store-backed one is never constructed | yes or recorded |
| SRC-02 | important | confirmed | A floor or core hit on any source's claim text deletes the verified torrent stored under that hash | yes or recorded |
| SRC-03 | important | confirmed | While a service's host is blocked, the enrichment runtimes pass items over and spend the day's count without sending anything | no |
| SRC-04 | important | confirmed | Feed plugins poll at once on every start; no plugin persists its last poll as SRC-0 said it would | yes or recorded |
| SRC-05 | important | confirmed | The torrent cache throws away queued lookups whenever every cache is backing off, and a dropped interest lookup is never retried | no |
| SRC-06 | important | confirmed | A source whose every poll fails still reads as healthy; the limiter's "blocked by source" state and the parsers' summaries reach nobody | no |
| SRC-07 | important | confirmed | BitTorrent unverified items have no cap and no expiry, so a source can grow the store and the id space without bound | yes or recorded |
| SRC-08 | polish | confirmed | `Retry-After` as an HTTP date is ignored | yes or recorded |
| SRC-09 | polish | confirmed | Large untrusted bodies are parsed on async worker threads | no |
| SRC-10 | polish | confirmed | The Account gate is met by any stored credential name, not by the ones the plugin declares | yes or recorded |
| SRC-11 | polish | confirmed | No plugin is behind a cargo feature | no |

### Feeds and importers
[`14-feeds-importers.md`](2026-11-13-full-code-review/14-feeds-importers.md): 0 blocking, 3 important, 2 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| IMP-01 | important | confirmed | A verified info dict never upgrades a complete-but-unverified import; the follow-up it is left to is not scheduled | partly |
| IMP-02 | important | likely | Bitmagnet adapter: source-defined functions in index predicates or expressions on the read tables run in the importer's session at plan time | no |
| IMP-03 | important | confirmed | `Store::interrupt_import_jobs` has no caller: the restart rule in IMP-1's state model is not implemented | no |
| IMP-04 | polish | confirmed | `import resume` on a job that has already read its whole source can block it for good | no |
| IMP-05 | polish | confirmed | The legacy `bitmagnet::import()` is still public and writes with no filter | yes or recorded |

### Logging, redaction and settings
[`15-logging-settings.md`](2026-11-13-full-code-review/15-logging-settings.md): 1 blocking, 3 important, 4 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| LOGSET-01 | blocking | confirmed | Saved settings with a cross-field check are dropped at every start (logging `data` preset, `file_size_mib`) | no |
| LOGSET-02 | important | confirmed | The logs folder can exceed its total cap by up to one file's size | no |
| LOGSET-03 | important | confirmed | Three settings groups are saved and shown as live, but nothing reads them | yes or recorded |
| LOGSET-04 | important | confirmed | `settings/log.rs` tests wait with fixed sleeps and then assert the work finished | yes or recorded |
| LOGSET-05 | polish | confirmed | `config.toml` values bypass the ranges the app enforces for the same settings | partly |
| LOGSET-06 | polish | confirmed | The current log file's age is counted from process start, so restarts keep lines past the maximum age | no |
| LOGSET-07 | polish | likely | An unusable logs folder is retried and reported every heartbeat, with an uncapped side log on Windows | no |
| LOGSET-08 | polish | likely | Config parse errors go to stderr unscrubbed, with the offending line | no |

### Binary: platform, lifecycle, operations
[`16-platform.md`](2026-11-13-full-code-review/16-platform.md): 0 blocking, 5 important, 9 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| PLAT-01 | important | confirmed | A start that fails after the pipeline is running drops everything without a shutdown | no |
| PLAT-02 | important | likely | The Windows service's folder check ignores the permissions of the entries inside it | no |
| PLAT-03 | important | confirmed | `lodestar backup` on Windows writes `auth.json` and `seal.key` with whatever permissions the target folder has | no |
| PLAT-04 | important | confirmed | The portable Windows build stops uncleanly when its window is closed or Windows shuts down | no |
| PLAT-05 | important | confirmed | On Unix the data directory and the store are created with the default umask | no |
| PLAT-06 | polish | confirmed | Sixty of the 164 `/metrics` families have no row in `docs/metrics.md`, and no test checks the rest | no |
| PLAT-07 | polish | confirmed | `/stats.disk_free_bytes` is always null although the free-space read now exists, and nothing warns of a filling disk | yes or recorded |
| PLAT-08 | polish | confirmed | Several tests in this area wait with deadlines under the 10 s the standard requires | yes or recorded |
| PLAT-09 | polish | likely | `seal.key` is created in place, so a crash during the first start can leave an empty file that blocks every later start | no |
| PLAT-10 | polish | confirmed | `Handles::shutdown` skips the remaining stops when the pipeline or the indexer fails to stop | no |
| PLAT-11 | polish | confirmed | On Unix a stop signal during a long start is acted on only after the start finishes, and a second signal is ignored | no |
| PLAT-12 | polish | confirmed | Dependency pins in the manifests are mostly not exact | no |
| PLAT-13 | polish | confirmed | `begin_purge` runs a blocking store read on an async worker | no |
| PLAT-14 | polish | suspected | The service's refusal log in `%SystemRoot%\Temp` can be pre-created by an unprivileged user | no |

### Web: core and search
[`17-web-core.md`](2026-11-13-full-code-review/17-web-core.md): 1 blocking, 4 important, 5 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| WEBCORE-01 | blocking | confirmed | "Matched in N files" never shows more than 20 | no |
| WEBCORE-02 | important | confirmed | After "Block this…", the "N results removed" announcement is usually missing and sometimes the wrong number | no |
| WEBCORE-03 | important | confirmed | One 4xx answer from `/events` stops the stream for the life of the tab | no |
| WEBCORE-04 | important | likely | `LineChart` rebuilds uPlot on every data update once its shape or format has changed | no |
| WEBCORE-05 | important | confirmed | `LineChart.test.ts` waits a fixed 20 ms and then asserts that nothing happened | no |
| WEBCORE-06 | polish | confirmed | After Back from the sign-in page, a 401 no longer redirects | no |
| WEBCORE-07 | polish | confirmed | A superseded reload of the same query clears the newer request's "refining" state | no |
| WEBCORE-08 | polish | confirmed | The bulk selection keeps rows that a reload removed | no |
| WEBCORE-09 | polish | confirmed | Recent searches stay in `localStorage` after sign-out | no |
| WEBCORE-10 | polish | confirmed | Jobs screen formats the rate with its own `Intl.NumberFormat` | no |

### Web: feature screens
[`18-web-features.md`](2026-11-13-full-code-review/18-web-features.md): 0 blocking, 4 important, 5 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| WEBFEAT-01 | important | likely | Wizard steps built from string keys never load their section's labels: raw field names and enum values are shown | no |
| WEBFEAT-02 | important | confirmed | Filters & Safety never shows the server's pack-state conditions | no |
| WEBFEAT-03 | important | confirmed | Saving a source's settings replaces every saved setting with what the form shows, and the form always starts from defaults | yes or recorded |
| WEBFEAT-04 | important | confirmed | Live log: a `resync` frame empties the view, also while paused, and reopens with no backoff | yes or recorded |
| WEBFEAT-05 | polish | likely | Updates store drops the newer of two overlapping status reads | no |
| WEBFEAT-06 | polish | confirmed | Cancel on a running import or export has no confirmation | no |
| WEBFEAT-07 | polish | confirmed | A companion's log lines are shown without the live log's control-character handling | no |
| WEBFEAT-08 | polish | confirmed | Raw identifiers shown where a message is expected | no |
| WEBFEAT-09 | polish | confirmed | Jobs formats a rate with `Intl.NumberFormat` directly | no |

### CI, release, packaging and infrastructure
[`19-ci-packaging-infra.md`](2026-11-13-full-code-review/19-ci-packaging-infra.md): 0 blocking, 9 important, 5 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| INFRA-01 | important | confirmed | The Docker image is pushed to GHCR (`:X.Y.Z`, `:X.Y`, `:latest`) on any `v*.*.*` tag push, with no environment gate or owner approval | no |
| INFRA-02 | important | likely | `freshness.yml` re-signs whatever envelope sits on the `update-manifest` release, so a rolled-back manifest stays "current" for good | no |
| INFRA-03 | important | likely | A backport tag moves `:latest` backwards | no |
| INFRA-04 | important | likely | The released image is built from the shared Actions cache, and mutable-tag actions on `main` can write that cache | partly |
| INFRA-05 | important | confirmed | Rust tests that read files outside `crates/` are skipped on PRs that change only those files, and `ci-ok` passes | partly |
| INFRA-06 | important | confirmed | `fleet-emergency-down.yml` is outside the `infra` job's path filter, so its structural tests don't run on a PR that changes it | no |
| INFRA-07 | important | likely | The Cloudflare deploy token is a repository secret, and a manual run deploys any branch's Worker | yes or recorded |
| INFRA-08 | important | confirmed | Release binaries are compiled with whatever `stable` is on the day; the Docker image with 1.97.1 | yes or recorded |
| INFRA-09 | important | confirmed | No dependency-advisory check anywhere in CI, and nothing refreshes the pins | yes or recorded |
| INFRA-10 | polish | confirmed | Live mode no longer writes the spend ledger, so the $40 monthly cap rests on a hand edit; the code and comments still describe the retired reaper | yes or recorded |
| INFRA-11 | polish | likely | The scheduled freshness job has no watchdog of its own | no |
| INFRA-12 | polish | confirmed | The signing jobs install minisign from apt at run time; CI pins it by hash because apt was unreachable once | no |
| INFRA-13 | polish | confirmed | `sign.ps1` writes the certificate to a fixed temp name and passes its password on the command line | no |
| INFRA-14 | polish | confirmed | The Unraid template checker is in the repository and no workflow runs it | yes or recorded |

### Tests, fuzzing and benchmarks
[`20-tests-fuzz-bench.md`](2026-11-13-full-code-review/20-tests-fuzz-bench.md): 0 blocking, 6 important, 7 polish.

| Id | Reviewer's severity | Confidence | Finding | Tracked? |
|---|---|---|---|---|
| TEST-01 | important | confirmed | Three real-server Playwright specs match no CI project and never run | partly |
| TEST-02 | important | confirmed | Fixed sleeps followed by a positive assertion | no |
| TEST-03 | important | confirmed | The peer-wire parsers have no fuzz target and no property test | no |
| TEST-04 | important | confirmed | CI fuzzing is a 60-second smoke run from an empty corpus, with small inputs and no allocation limit | partly |
| TEST-05 | important | confirmed | The detail drawer's size gate is 28,064 B; the documented budget is 15 KB | yes or recorded |
| TEST-06 | important | confirmed | Most §3 performance budgets are not enforced by a required check | yes or recorded |
| TEST-07 | polish | confirmed | Waits with deadlines under the 10 s the standard asks for | no |
| TEST-08 | polish | likely | Fixed windows that assert nothing happened can pass without testing anything | no |
| TEST-09 | polish | likely | Wall-clock upper bounds on real work | no |
| TEST-10 | polish | confirmed | `coverage.test.ts` cannot fail, and three routes are outside the smoke matrix | yes or recorded |
| TEST-11 | polish | confirmed | Ignored timing tests that no job runs | no |
| TEST-12 | polish | confirmed | The fuzz README and the 1.0 plan undercount the targets | no |
| TEST-13 | polish | confirmed | The e2e `engage` helper is a four-second fixed nudge, and page errors don't fail specs | no |
