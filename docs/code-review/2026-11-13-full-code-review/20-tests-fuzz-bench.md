# Test, fuzz and benchmark quality review
Reviewed at 55e155a9.

No blocking findings by the brief's definition. TEST-02 (fixed sleeps) and TEST-07 (short deadlines) are things `runner/standards.md` §2 and §5 call blocking when they appear in a pull request; they are rated here by what they cost the product, which is test reliability, not behaviour.

## Coverage
- Read in full: `.github/workflows/ci.yml`, `e2e.yml`, `web.yml`, `soak-screen.yml`; `.github/scripts/ci_changes.py`; `runner/standards.md`; `web/playwright.config.ts`, `web/vitest-setup.ts`, `web/e2e/README.md`, `web/e2e/smoke.test.ts`, `web/e2e/coverage.test.ts`, `web/e2e/snapshot.ts`; `fuzz/README.md`, `fuzz/Cargo.toml` and ten fuzz targets (`utp`, `krpc`, `filetree`, `tokenizer`, `bencode`, `release`, `tracker_scrape`, `cluster_frame`, `worker_spool`, `query`); `docs/soak/tools/analyze_cr.py` lines 77-670 and 810-1135 (parsing, segment analysis, measurement, statistics, effect fit, gate); `docs/soak/tools/analyze_cr0.py` lines 218-300 and 403-680 (screen, summary, memory fit).
- Swept by grep or script, then read at every hit: all 319 `sleep` calls in Rust test code; all 47 `#[ignore]`; 65 short deadlines on waits; every file-level `cfg` gate in `crates/*/tests`; every `test.skip`, `fixme`, `.only` and `waitForTimeout` under `web/`; 6,260 Rust tests and 2,502 vitest cases for bodies with no assertion; all 22 proptest regression files against their source files; source-to-test line counts for every crate.
- Skimmed: `bench/src/bin/search-bench.rs` and `size-report.rs` (gate logic and thresholds only), `web/.size-limit.mjs` (entries and limits), `web/lighthouserc*.json`, the headers of the eight fixture specs, `docs/soak/tools/linux/collect.py` (memory fields and units only), the decisions files for fuzzing, e2e, soak memory fit and test flakes.
- Not read: the bodies of most of the 96 Playwright specs (about 18k lines; sampled by sweep only), the other 29 fuzz targets, `bench/src/fixture.rs` and `synth.rs`, the PowerShell collectors and their tests, `docs/soak/tools/linux/test_collect*.py`, `bench/relevance`. Nothing was built or run, so no flake rate here is measured.

## Findings

### TEST-01 Three real-server Playwright specs match no CI project and never run
- Severity: important
- Kind: test
- Confidence: confirmed
- Where: `web/playwright.config.ts:86` (`core` matches `/(routes|tokens|harness|theme-gate|coverage)\.test\.ts$/`), `web/playwright.config.ts:101` (`fixture` matches `/\.fixture\.test\.ts$/`), `web/e2e/filters-flows.fixture.routes.test.ts:11-14`, `web/e2e/insights.fixture.routes.test.ts:19`, `web/e2e/settings-form.fixture.routes.test.ts:14`, `.github/workflows/e2e.yml:526`
- What: these three specs need a real `lodestar serve --fixture` server and skip unless `E2E_FIXTURE_URL` is set. Their names end in `.fixture.routes.test.ts`, so they match the `core` project (pull requests, where the variable is never set, so they skip) and not the `fixture` project (the nightly job runs `npx playwright test --project=fixture`, which takes only names ending `.fixture.test.ts`). They run in no CI job.
- Failure scenario: a change breaks the floor-rule refusal (the 428 and 401 step-up when a core filter rule is switched off), the settings 409 merge or 422 bounds message, or the Insights reconciliation against the server's rollups (architecture §10.17). Pull-request e2e passes because the mock answers; the nightly fixture job passes because it never selects these files.
- Suggested fix: rename them to `*.fixture.test.ts` or widen the `fixture` project's `testMatch`, and have the fixture job fail when a selected file ran zero tests. Eight files signing in will exceed the login burst of 5 per address, so this needs the shared `storageState` that `docs/decisions/2026-11-01-X-e2e-fixture-ci.md` already names as the durable fix.
- Tracked?: partly. `docs/decisions/2026-11-01-G-m5a.md:20` calls `insights.fixture.routes.test.ts` "local only". `2026-11-01-X-e2e-fixture-ci.md` speaks of "the five fixture files" and doesn't mention the other three. No slice found.

### TEST-02 Fixed sleeps followed by a positive assertion
- Severity: important (blocking under `runner/standards.md` §2 and §5 if raised on a pull request)
- Kind: test
- Confidence: confirmed for the pattern; the flake rate is not measured
- Where:
  - `crates/lodestar/src/settings/log.rs:327-332`: 300 ms, then asserts the mask applied and the file opened. It also needs a 2 s wall-clock deadline set at line 324 not to pass first.
  - `crates/lodestar/src/settings/log.rs:351-358`: 100 ms, then 200 ms, then asserts a save applied.
  - `crates/dht/tests/network.rs:168-171`: 800 ms, then `queries_in > 0`.
  - `crates/dht/tests/network.rs:184-188`: 800 ms, then `denied > 0`.
  - `crates/dht/tests/harvest.rs:291`: "Let it join, then ask", 500 ms before `get_peers`.
  - `crates/dht/tests/scrape.rs:209`: 500 ms before a scrape whose counts are asserted exactly.
- What: each waits a fixed time for a background task (the log follower, a DHT node's bootstrap) and then asserts the work happened. None polls for the condition.
- Failure scenario: on a loaded Windows runner the follower task or the node's first bootstrap packet is scheduled later than the sleep, and the assertion fails with no code change. In `log.rs:327` a stall of over a second also lets the deadline pass before the first assertion. For `harvest.rs` and `scrape.rs` I did not confirm what a lookup does on a node that hasn't joined; if it parks until bootstrap ends, the sleep is only dead weight there.
- Suggested fix: poll the same condition with the helpers the crates already have (`eventually` in `crates/dht/tests/pause.rs`, the 15 s loop at `crates/lodestar/src/settings/tests.rs:657`).
- Tracked?: not found. PR 778 (merged after the review commit) changes `crates/lodestar/tests/windows_service.rs`, `crates/sources-importers/src/bitmagnet/pg_tests.rs` and `infra/fleet` only. `docs/decisions/2026-11-12-X-flake-dht-scrape-join.md` puts a `scrape.rs` abort down to tokio 1.54.0 and leaves the join sleep in place.

### TEST-03 The peer-wire parsers have no fuzz target and no property test
- Severity: important
- Kind: test
- Confidence: confirmed
- Where: `crates/peer-wire/src/message.rs:47` (`parse_handshake`), `:84` (`parse_extended_handshake`), `:153` (`parse_metadata_message`), `:174` (`parse_pex`); `crates/peer-wire/src/mse.rs:122,197` (`initiate`, `respond`); `crates/peer-wire/src/wire.rs:69` (`read_message`); `fuzz/Cargo.toml` (no `lodestar-peer-wire` dependency); `crates/peer-wire/Cargo.toml:34` (`proptest` is a dev-dependency no file uses)
- What: every byte these functions read comes from an arbitrary internet peer: the BEP 3 handshake, the BEP 10 extended handshake, BEP 9 `ut_metadata`, `ut_pex` and the MSE handshake. The crate has 49 tests against scripted fake peers, but none of the 39 fuzz targets builds it and it has no `proptest!` block. CLAUDE.md and `runner/standards.md` §2 require parsers to have property tests or fuzz targets, and CLAUDE.md names the peer protocol among the correctness-critical parsers. The uTP transport underneath is fuzzed; the protocol on top is not.
- Failure scenario: a later edit to `parse_metadata_message` (it slices `&payload[used..]`) or to `decode_compact` (it indexes `b[b.len() - 2]`) adds a panic on a short or odd-length payload. Nothing generates inputs that would find it; the fake peers send only the shapes their authors thought of.
- Suggested fix: a `peer_wire` target over the four pure parsers (no panic; `metadata_data` and `pex_message` round-trip), and a second that drives `fetch` and the MSE responder over an in-memory duplex fed fuzzed bytes, asserting the metadata size cap.
- Tracked?: not found in `runner/slices.yaml`, `docs/build-plan.md` or `docs/decisions/`. M8-SEC-1 plans an hour of fuzzing per existing target, which doesn't add this one.

### TEST-04 CI fuzzing is a 60-second smoke run from an empty corpus, with small inputs and no allocation limit
- Severity: important
- Kind: test
- Confidence: confirmed for the flags; likely for the 4 KiB input cap (libFuzzer's documented default, not run here)
- Where: `.github/workflows/ci.yml:417`; `.gitignore` (`/fuzz/corpus`); `fuzz/seeds/` (24 of 39 targets); `fuzz/fuzz_targets/cluster_frame.rs:5-6`; `fuzz/fuzz_targets/worker_spool.rs:10-11`
- What:
  - Each target runs with `-max_total_time=60` and no other flag. The corpus is ignored by git and no step caches it, so every run starts from the seeds, or from nothing for the 15 targets with no seeds: `bencode`, `krpc`, `utp`, `tokenizer`, `release`, `tracker_scrape`, `lists`, `full_scrape`, `dht_restart`, `update`, `cluster_assign`, `cluster_frame`, `cluster_have`, `cluster_filter_snapshot`, `worker_spool`.
  - No `-max_len`. libFuzzer then caps inputs at 4,096 bytes unless a seed is larger, so the size caps the README describes (256 KiB webhook bodies, 512 KiB cluster payloads, the 8 MiB torrent-cache cap) are never approached.
  - `cluster_frame` and `worker_spool` say in their headers that their allocation caps are checked by running with `-malloc_limit_mb=8` and `=64`. CI passes neither, so "never allocates past the caps" isn't checked by any automated run.
  - No `-timeout`. libFuzzer's default is 1,200 s, so an input that takes 30 s passes.
  - The longer campaigns on record are one-off: 18 targets for 15 minutes each and two for 30 minutes, all on 2026-11-05. The other 20 targets, the five cluster ones, `update`, `dht_restart` and `utp` among them, have no recorded run longer than 60 s.
- Failure scenario: a length-prefixed decoder in a cluster target allocates from a peer-chosen count. Reaching it needs a valid header, which 60 s from an empty corpus may not build, and an allocation of a few hundred MB wouldn't trip libFuzzer's default 2 GB limit if it did.
- Suggested fix: cache `fuzz/corpus/<target>` between runs; keep each target's `-max_len`, `-timeout=10` and `-malloc_limit_mb` in a small table under `fuzz/`; seed the 15 bare targets from the crates' existing test vectors; give the nightly run, which already runs all 39, ten minutes each.
- Tracked?: partly. `docs/decisions/2026-11-05-X-fuzz-1105.md` records the one campaign. M8-SEC-1 (`runner/slices.yaml:972`) plans "an hour of fuzzing for each of the 27 targets" before 1.0 and G-v1-security wants a clean campaign. There are 39 targets now, and neither slice changes what CI runs.

### TEST-05 The detail drawer's size gate is 28,064 B; the documented budget is 15 KB
- Severity: important
- Kind: docs
- Confidence: confirmed
- Where: `web/.size-limit.mjs:1607` (`entry('Detail drawer (own JS + CSS)', drawerView, '28064 B')`); `docs/architecture.md:329` ("Detail-drawer chunk | ≤ 15 KB"); `runner/standards.md:89` ("Detail drawer ≤ 15 KB"); `docs/decisions/2026-10-30-L3-L5-landing.md:10` ("§10.1's 15 KB drawer target ... unchanged")
- What: the gate CI enforces is nearly twice the budget that §10.1 and the standards state, and a further on-demand "Detail sections" entry sits beside it. The limit has been raised a few bytes at a time under the runner's §2.26 authority, each raise noted in a comment or a decisions file, but no record changes the 15 KB figure. A reviewer applying `runner/standards.md` §4 would believe 15 KB is enforced. The first-load budgets (45 KB JS, 20 KB CSS, 35 KB fonts, 100 KB total) are enforced as written.
- Failure scenario: a slice adds 500 B to the drawer; the runner raises the limit by 500 B under §2.26, as the comments above the entry show it has at least four times since 2026-11-01; the drawer keeps growing with every check green and the stated budget still reading 15 KB.
- Suggested fix: decide which number is real. Either record the current figure as the budget in §10.1 and `runner/standards.md` (the owner set 15 KB, so the runner flags it under §2.19), or open a slice to bring the drawer back.
- Tracked?: each raise is recorded (`2026-11-01-X-detail-headroom.md`, `2026-11-01-S-send.md`, `2026-11-05-ENR-8.md`). Nothing found that reconciles the gate with §10.1.

### TEST-06 Most §3 performance budgets are not enforced by a required check
- Severity: important
- Kind: gap
- Confidence: confirmed
- Where: `.github/workflows/ci.yml:267-290,305-317,653-655`; `bench/src/bin/search-bench.rs:57-88`; `crates/api/tests/insights.rs:1460-1526`; `web/lighthouserc.json`
- What: architecture §3 says the performance targets are "enforced as CI benchmark gates". What CI does today:

| Budget (§3, §10.1, §20.8) | Check | Required? |
|---|---|---|
| Store, blob, torrent index and file index bytes per torrent | `size-report --gate`, 50k torrents | yes (`ci-ok`) |
| Search JS, CSS, fonts, total first load | `size-limit` | yes (`web-ok`) |
| LCP, TBT (for INP), CLS, byte weight | Lighthouse, median of 3, on the bare `/search` page with no backend and no results | yes (`e2e-ok`) |
| Logging hot path | `logging::bench` | yes (`ci-ok`) |
| Keyword, browse, facets, suggest, file-level p95; new torrent to searchable under 5 s | `search-bench --gate` at 200k torrents, against the absolute thresholds set for 10M | no (left out of `ci-ok` on purpose) |
| Filter under 50 µs per torrent | `props` `budget`, release | no |
| Reporting overhead | `reporting-overhead --gate` | no (nightly only) |
| Insights queries under 50 ms | `a_year_of_rollups_answers_within_budget` asserts the 50 ms only when `!cfg!(debug_assertions)`; CI runs it in debug, so the budget is never asserted | no |
| Full-name regex under 500 ms | none | no |
| Coordinator ingest 1,000/s, grant p95, worker RSS and bytes on the wire | none (an ignored measurement, `worker/runtime.rs:2915`) | no |
| RSS under 1 GB at 10M | soak evidence only | no |
| 60 fps scrolling | none | no |

- Failure scenario: a change makes keyword search five times slower. At 200k torrents the p95 is still under the 30 ms set for 10M, so `search latency (§3)` stays green; had it gone red, `ci-ok` would still pass and the PR could merge under §2.18. An Insights query that regresses to 200 ms passes `cargo test --workspace`, because the test named for the budget only prints the time in a debug build.
- Suggested fix: until the 10M runner exists, compare the 200k run to a committed baseline with a tolerance (as `relevance` does with `bench/relevance/baseline.txt`), so a relative regression is visible. Run the Insights budget test in release in the nightly job, or rename it so it doesn't claim a budget it doesn't check.
- Tracked?: yes, for the 10M measurement. M8-PERF-1 (regex and Insights rows; PR 780 is its first part), M8-PERF-2 (ingest and worker), M8-PERF-3 (a dedicated runner) and G-v1-10m. The comment at `ci.yml:424-432` records why the timing jobs are outside `ci-ok`. The debug-only Insights assertion and the absolute thresholds at 200k are not mentioned in any of them.

### TEST-07 Waits with deadlines under the 10 s the standard asks for
- Severity: polish
- Kind: test
- Confidence: confirmed
- Where (the ones that wait on real work; 65 hits were read):
  - `crates/pipeline/tests/health.rs:107-115` (`until`: 200 × 25 ms, about 5 s), used throughout the file
  - `crates/dht/tests/pause.rs:66` (`eventually`: 100 × 50 ms), `:50` (2 s for a ping reply)
  - `crates/dht/tests/harvest.rs:77` and `crates/dht/tests/scrape.rs:86` (2 s for a UDP reply)
  - `crates/api/tests/crawler_events.rs:238` (5 s), `crates/api/src/events.rs:1322` (5 s)
  - `crates/lodestar/src/system/address/tests.rs:360,650,685,808` (5 s), `crates/lodestar/src/settings/log.rs:333` (5 s), `crates/lodestar/src/logging/source_tests.rs:388` (5 s)
  - `crates/lodestar/tests/serve.rs:215` (8 s for the real binary's first crawler event)
  - `crates/sources/src/pool.rs:3048,3074,3097,3103` (1 s), `crates/crawler-sim/tests/e2e/net.rs:165` (3 s)
- What: `runner/standards.md` §2 asks for a deadline of at least 10 s so tests pass on the slowest runner under load. These poll correctly but give up early.
- Failure scenario: `cargo test --workspace` on a Windows runner starves one test binary for a few seconds; a 2 s wait for a loopback UDP reply fails.
- Suggested fix: raise the shared helpers to 15 s; that covers most call sites in one edit each.
- Tracked?: not found.

### TEST-08 Fixed windows that assert nothing happened can pass without testing anything
- Severity: polish
- Kind: test
- Confidence: likely
- Where: `crates/lodestar/src/ops/tests.rs:276-280,872`; `crates/lodestar/src/settings/tests.rs:680`; `crates/lodestar/src/system/restart.rs:962` (20 ms); `crates/lodestar/src/system/address/tests.rs:657,681`; `crates/pipeline/tests/health.rs:222,383`; `crates/api/src/jobs.rs:740`; `crates/api/tests/export.rs:1007,1219`; `crates/api/tests/insights.rs:1939`; `crates/sources/tests/pool.rs:2216`; `crates/dht/tests/pause.rs:109`
- What: each sleeps 20 ms to 1 s and then asserts a counter did not move. The code comments say so and call it a negative check. These can't fail spuriously, which is why they were kept, but on a slow runner the thing they guard against may simply not have had time to happen, so a real regression passes.
- Failure scenario: a paused pool starts fetching again 400 ms after the pause on a loaded runner; the 300 ms window at `ops/tests.rs:872` closes first and the test is green.
- Suggested fix: where the code has a later observable event, wait for that event and then assert the counter (for example, send a second input that the task must process after the one under test). Where it doesn't, accept the weakness but lengthen the 20 ms and 100 ms windows.
- Tracked?: not found; the comments show the choice was deliberate.

### TEST-09 Wall-clock upper bounds on real work
- Severity: polish
- Kind: test
- Confidence: likely
- Where: `crates/api/src/jobs.rs:810,827,830` (a pacer wait must take under 600 ms, and a free one under 50 ms); `crates/api/tests/export.rs:1004` (a detail request and a search in under 1 s); `crates/pipeline/tests/health.rs:462` (shutdown in under 2 s); `crates/peer-wire/tests/fetch.rs:194` and `crates/peer-wire/tests/utp.rs:157` (5 s); `crates/store/src/compress.rs:324` (5 s); `crates/lodestar/src/worker/runtime.rs:2787` (5 s)
- What: these assert that something finished within a bound measured on the real clock, in debug builds on shared runners. `bench/src/bin/search-bench.rs:32-34` records that the same kind of bound "failed whenever a busy CI runner starved it" and was moved out of the unit tests for that reason.
- Failure scenario: the test thread is descheduled for 60 ms between two lines of `jobs.rs:828-830`.
- Suggested fix: use paused tokio time for the pacer test, as the 59 sleeps in `crates/sources` already do; widen the others to bounds that only a hang would exceed.
- Tracked?: not found.

### TEST-10 `coverage.test.ts` cannot fail, and three routes are outside the smoke matrix
- Severity: polish
- Kind: test
- Confidence: confirmed
- Where: `web/e2e/coverage.test.ts:1-2,23-31`; `web/e2e/support/routes.ts`
- What: the test finds routes missing from the smoke matrix and writes them to an annotation; its only assertion is `expect(ROUTES.length).toBeGreaterThan(0)`. The `TODO(W6 follow-up)` at the top says to turn it into a failure. Today `/logs`, `/insights/[stage]` and `/settings` have pages and no smoke entry, so they get no axe run per breakpoint and theme. Each has functional specs of its own.
- Failure scenario: a new route ships with no smoke entry; every theme and breakpoint project stays green.
- Suggested fix: add the three routes and assert `uncovered` is empty.
- Tracked?: the TODO in the file; no slice found.

### TEST-11 Ignored timing tests that no job runs
- Severity: polish
- Kind: gap
- Confidence: confirmed
- Where: `crates/search/tests/cache.rs:376` (`cached_hits_are_within_two_milliseconds`), `crates/api/tests/explain.rs:372` (2 ms), `crates/api/tests/insights.rs:1468` (50 ms), `crates/filter/tests/sp4.rs:1072` (50 µs with the signature set at its cap), plus about 40 tests marked as measurements
- What: 47 tests are `#[ignore]`d. CI runs three groups of them with `--ignored`: the filter `budget` (`ci.yml:317`), `logging::bench` (`ci.yml:333`) and `anon_mappings_cost` (`soak-screen.yml:60`, release branches only). The rest run only by hand. Most are honest measurements that only print, but the four above assert a bound.
- Failure scenario: the 2 ms cached-hit bound is broken and nothing reports it.
- Suggested fix: run the asserting ones in release in the nightly job, or drop the assertion and call them measurements.
- Tracked?: not found.

### TEST-12 The fuzz README and the 1.0 plan undercount the targets
- Severity: polish
- Kind: docs
- Confidence: confirmed
- Where: `fuzz/README.md` (29 rows), `fuzz/fuzz_targets/` (39 files), `runner/slices.yaml:972` ("each of the 27 targets")
- What: the README has no row for `cluster_frame`, `full_scrape`, `json`, `local_torrent`, `magnetico_row`, `query`, `rss`, `tokenizer`, `torznab` or `worker_spool`. M8-SEC-1 names 27 targets.
- Failure scenario: the 1.0 campaign is run from the plan's count and leaves 12 targets out.
- Suggested fix: add the rows; have M8-SEC-1 say "every target in `fuzz/fuzz_targets`".
- Tracked?: not found.

### TEST-13 The e2e `engage` helper is a four-second fixed nudge, and page errors don't fail specs
- Severity: polish
- Kind: test
- Confidence: confirmed for the code; suspected for any flake
- Where: `web/e2e/support/events-server.ts:39-47`; `web/e2e/alerts.routes.test.ts:20` (the only spec that listens for `pageerror`)
- What: `engage` dispatches an event 40 times, 100 ms apart, with `waitForTimeout`. It always costs 4 s and assumes the chrome mounts within that time. Two spec files import it. `web/e2e/restart.routes.test.ts:26-30` does the same job with `expect(...).toPass({ timeout: 15_000 })`. Separately, the shared fixture in `web/e2e/support/test.ts` fails a test on an unmocked API call but not on an uncaught exception in the page.
- Failure scenario: the chrome mounts after 4.1 s on a slow run; the stream never starts and the next assertion times out.
- Suggested fix: use the `toPass` form from `restart.routes.test.ts` everywhere; collect `pageerror` in the shared fixture and fail at teardown.
- Tracked?: not found.

## Checked and found sound
- `docs/soak/tools/analyze_cr.py` statistics: the incomplete beta continued fraction, `t_cdf`, the bisection quantile and the OLS inverse match the textbook forms, and `test_analyze_cr.py:663` pins t(0.975, 3) to 3.182. `fit_effect` and `outcome` implement rules 6.2 and 6.3 of `docs/decisions/2026-11-02-CR-SPEC.md` as written: block intercepts, per-block trends, degrees of freedom of segments minus 2 × blocks minus 1, and first-match outcomes.
- The same file's evidence handling: a counter reset or a vanished series invalidates the segment (`counter_resets`); a series present at one end of a delta raises rather than reading as zero (`delta`); gaps are measured from process start and a failed scrape doesn't close one; dropped latency samples count as slow; rates divide by the measured minutes between boundary scrapes, not by a nominal hour.
- `docs/soak/tools/analyze_cr0.py` `memory_fit`: slope, standard error and the bar test are computed correctly; a restart inside the window refuses the fit; an incomplete window gives a slope and no verdict. Its known weakness, that plain OLS on minute-apart samples understates the interval, is a recorded decision (`docs/decisions/2026-10-30-X-soak-memory-fit.md`) and is printed in the report.
- Collector units: `linux/collect.py:876-879` converts `/proc` kB to bytes, and Docker mode reads cgroup `anon` for the gate's field.
- All 22 committed proptest regression files map to a live source file with a `proptest!` block, at the path proptest reads by default.
- No `assert!(true)`, no `.only`, no `test.fixme` anywhere. The 70 Rust tests and 6 vitest cases with no assertion token are "never panics" properties or thin wrappers over asserting helpers; three were opened to confirm.
- 199 of the 319 Rust test sleeps pace a polling loop with a deadline, and 59 run under paused tokio time (50 of them in `crates/sources`). About 30 more pace a fake peer or server, which the standard allows.
- `size-report --gate` fails closed: a value it can't measure, or a summary it can't write, fails with or without `--gate` (`bench/src/bin/size-report.rs:619-629`, with tests).
- `ci-ok` fails on any failed or cancelled needed job, and the `changes` job is among them, so a crash in `ci_changes.py` can't turn into all-skipped-green. Root-only tests are run under `sudo` and the job fails if they print "skipping" (`ci.yml:204-235`). The PostgreSQL importer tests fail instead of skipping when the server is missing.
- Snapshot regeneration can't be merged by accident: a run that regenerated baselines fails `e2e-ok` (`e2e.yml:255-257`). Playwright runs with `retries: 0` and `forbidOnly` in CI, so flakes aren't hidden by retries.
- The eight fuzz targets read in full assert real invariants (round-trip, canonical re-encode, bounded counts, offsets on char boundaries), not only "no panic". The fuzz job is in `ci-ok`, and a change under `fuzz/` runs every target.

## Area-level concerns
- **Pull-request e2e never touches the binary.** Every PR spec runs against `vite preview` and an OpenAPI mock typed from `schema.d.ts`. That catches contract drift at compile time, but what the binary adds (its CSP against the pre-paint theme script, SPA fallback, precompressed assets, cookie flags, real auth and rate limits) is seen only by the nightly `fixture` job, with five spec files. That job is outside `ci-ok` and, unlike `all-themes`, opens no tracking issue when it fails. PR 784 (X-ci-main-signal) may cover the signalling; I didn't read it.
- **Coverage by flow (§10).** Setup wizard, login, search, the detail drawer, filters, settings save, updates and companions each have mock-backed specs, most with a keyboard-only twin. There is no workers screen or import screen in `web/src/lib/features` yet, so no spec either. Chromium is the only browser. Mobile is covered as viewport widths (320 and 390 in the smoke matrix) and seven spec blocks with `hasTouch` and `isMobile`; nothing runs on WebKit or Firefox.
- **Lighthouse measures an empty page.** The gate is real and required, but it loads `/search` with no backend, so LCP, CLS and TBT for a page of results are not gated. `docs/decisions/2026-10-24-S-search-2a.md` records a 0.063 layout shift that only a hand run against the fixture caught.
- **Platform reach.** Seventeen integration-test files in `crates/lodestar/tests` are `cfg(unix)` or Linux-only, and the updater modules under `crates/lodestar/src/update` are `cfg(unix)`. Windows has its own four files. macOS runs only on `main` and nightly, so a macOS break is found after merge. This is recorded in `ci.yml`'s header.
- **Updater tests run through debug-only seams** (`crates/lodestar/src/update/probation.rs:110`, `update/mod.rs` `testing::trust_root`). The release branch of those conditionals, with the embedded keys and the real version, is exercised by `release.yml`'s smoke job and the manifest round trip, not by `cargo test`.
- **Thin spots by line count** (source lines : test lines): `release-parser` 2,528 : 442, with one property file and a fuzz target; `bencode` 857 : 352; `log` 4,381 : 2,080; `cluster` 9,113 : 6,593. `crates/lodestar/src/update/windows/rollback.rs` (1,205 lines) and `update/probation.rs` (464) have no tests in the file; rollback is driven from `helper.rs`'s 95 tests, and I did not verify how much of probation `tests/supervisor.rs` reaches. The download-client code in `crates/integrations` is tested only against in-process fakes; the four `live` tests need a running client and are ignored.
- **Not swept:** `is_ok()` assertions on results whose error is the behaviour under test, and the bodies of most Playwright specs. Both need reading rather than grep, and I ran out of budget for them.
