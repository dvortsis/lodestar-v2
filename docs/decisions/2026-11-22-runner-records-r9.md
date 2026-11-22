# runner-records-r9 decisions (#1040)

The runner's records PR after r8 (#1025). It records the runner's calls from 2026-11-21Z to 2026-11-22Z. Each call is flagged to the owner, who can veto it (§2.19). The slices' own decisions files hold the detail.

## Calls in merged slices
- **X-safety-undo-danger (#1027):** final sizes Filters dialogs 36.04 kB and Filters pack panels 7.92 kB, as r8 said. r8's table also said Settings InsightsScreen and NotificationsScreen drifted; they didn't once the branch took in `main`, and their limits stayed.
- **X-page-frame (#1028), option C:**
  - **Shared class constants** in `ui/layout/classes.ts`, not `PageFrame` or `PageHeader` components. A compiled component cost about 0.75 kB per route: option A pushed 7 views past 45 kB, and option B broke Search's 45 kB cap (§10.1).
  - **One page title style,** `PAGE_H1`: the display size (28 px), the focus ring, and wrapping anywhere. The Settings title moves from the title size to the display size.
  - **The Settings title's left edge** is at 312 px from a 1024 px window, beside the section list; other pages keep 24 px.
  - **An e2e spec checks the frame** on every screen, waiting until each screen has loaded.
  - **Search's first view** is 44.70 kB (main 44.39 kB), under its 45 kB cap. The components version measured 45.31 kB.
  - **Size limits,** each the measured size rounded up to 0.01 kB: CSS 19.79, Settings section 45.58, Filters 31.53, Insights 45.45, Language 45.24, Notifications 45.66, Sources 34.04, Insights stage 40.98, Builder 8.44 kB.
  - **Search results (lazy) went from 40.83 to 40.84 kB,** carried in #1028 under §2.18: `main` measured 40,832 B, and #1028 itself adds 0 B.
  - **`QueryBar.svelte` lost one class line** (`px-4 py-2`). X-zero-rescue is told at launch.
- **X-undo-danger-tests (#1030):** tests only. Every flag branch #1027's review named now has a test.
- **X-snapshot-stale-outputs (#1031):** the smoke test fails when a Settings screen in the registry has no nav link. Nits left for a later touch: the id regex also counts commented-out entries (it fails loudly, which is safe); symlinked feature folders aren't followed; "1.8×" is an estimate, not a measurement.
- **X-outputs-e2e-race (#1032):** the Outputs export test waits for the screen's first `/jobs` read before starting an export. Nits left: the held-read flag in the press-first order, the "slow" comment, and a "Test ended" message.
- **X-facet-group-case (#1033):**
  - **S1-20** (blocking, security, facets label) was found and fixed in the same PR, and was never held. A label may differ from its value only by ASCII letters in upper case, and must be NFKC-stable ("S1-20: labels limited to ASCII re-casing"). The runner checked the fix.
  - **Size limits:** Facet rail 18.85 kB (18,848 B) and Builder 8.45 kB (8,441 B). The largest `/facets` rise at p95 is +1.3 ms (browse at group limit 100), under the 5 ms line. On a 1,000,010-torrent skewed bench, the largest rise is +0.72 ms, so the label walk stays uncapped, with no cache.
  - **The facets goldens** (`facets_browse`, `facets_get_limits`, `facets_selection`) were re-blessed for the label only.
  - **Search cards went to 8.02 kB for a while** to absorb build wobble, and came back to 8.01 kB with #1038.
- **X-grid-two-col-large (#1034):**
  - CSS 19.80 kB (19,792 B). `main` measured 19,780 B, so the PR adds 12 B, not the 9 B its decisions file says.
  - `large:col-span-2` and `large:gap-x-4` were needed. `items-baseline` and `items-end` were dropped: they were polish, and would have added bytes on top.
  - Lazy chunks grew by 40, 25 and 10 B. The column is 478 px, not 512.
- **X-filters-dialog-test-timeout (#1035):** a `beforeAll` warms the Filters screen's on-demand modules once. Nit: its decisions file says these are "every `import()` the screen makes", but `./rule-text` and `PackImpact.svelte` are lazy too. Neither costs a dialog test anything.
- **X-ui-checks-1 (#1036):**
  - Chunk-load failure, focus return to the opener, and the Settings save gap-fill are now checks, in the core and keyboard-only projects.
  - **The Search results chunk gap is pinned as an expected gap.** When that chunk fails, the page shows only the query bar, with no message, and throws an uncaught rejection. X-search-chunk-retry (below) fixes it and must flip that case.
- **X-builder-group-case (#1037), record only:**
  - No release-group value can reach the Builder's labelled value options (`valueLabel`). Group has no fixed list in `/search/fields`.
  - Group rows, their labels and the query chips show the server's lower-cased form, as the bar does. That's accepted for now; X-group-chip-label (below) follows up.
- **X-web-build-determinism (#1038):**
  - **The build is byte-stable.** The few bytes of noise came from content-hash chunk names spreading into the files that import them, which changes how gzip packs them.
  - **The CI size step alone** sets `LODESTAR_STABLE_NAMES=1`: chunk names come from each chunk's sorted module ids. The shipped build keeps content hashes, which cache-busting needs. The measured offset against the shipped build is −18 to +32 B on the tree #1038 measured; the fresh review measured −19 to +33 B on a later `main`.
  - **There's no build-twice guard** (runner call). It would add about 1.5 min to every push, and the fresh review showed two stable builds come out byte-identical.
  - **Size limits:** Settings messages 4.26 kB (4,259 B stable names, 4,247 B content hashes), and Search cards 8.01 kB.
  - **CSS asset names still carry content hashes.** X-size-ci-script (running) covers them, plus one command for CI's measurement.
- **CR-SAMPLE-BACKOFF-ESCALATE (#1029):**
  - **It's behind its switch,** `LODESTAR_DHT_SAMPLE_BACKOFF_ESCALATE`, off by default. The first backoff is 300 s. A second failure, or a reply saying sampling isn't supported, cools the node for 6 h.
  - **It's in the runner's landing lane** after three `slices.yaml` conflicts in a row: other slices kept adding their entries on the line next to it.
  - **It turned `main`'s `infra` job red** (inside `ci-ok`). `config.rs` now reads `LODESTAR_DHT_SAMPLE_BACKOFF_ESCALATE`, and the fleet test requires every switch it reads to be one the soak collector knows. #1029 merged green because `.github/scripts/ci_changes.py`'s `INFRA` filter misses `config.rs` and `docs/soak/tools/`, so the job never ran on it. The runner caught it in this PR's review.
  - **Fix main first:** X-backoff-escalate-passthrough passes the switch through the fleet VM stack and the soak collector (off unless set), keeps the tests that iterate the switches green, and widens the `INFRA` filter. It adds its own `slices.yaml` entry. Until it lands, any PR that touches `infra/` fails `ci-ok` for a reason that isn't its own (§2.18).

## Calls in running slices
- **X-ui-checks is split into four parts.** The umbrella entry is now `expanded` in `dashboard.yaml`.
  - **-1:** chunk failure, focus return and the Settings save gap. Merged as #1036.
  - **-2:** overlay timings and radius, rebuild and paused states, and the phone alert-and-Send flow. The layout-shift (CLS) check is dropped: Lighthouse already gates first paint, and the navigation case adds little.
  - **-3:** a pseudo-locale built from the message catalogue, in a separate build that runs nightly and on manual dispatch only, with a unit test of the transform. It catches hard-coded English and costs pull requests nothing.
  - **-4:** five fixture screenshots, with the fixture project's regeneration flow in the same PR, so its baselines have an owner from day one.
- **X-insights-source-names:**
  - **`SourceYield` is a new type** for the by-source rows, which always carry a name. `by_ip_family` keeps `SegmentYield`, so its JSON and CSV stay byte-identical.
  - **Plugin names aren't sanitised.** They're compile-time constants in each manifest, not untrusted input, and Svelte renders them as text. A server test checks every built-in name: non-empty, at most 64 characters, with no control or invisible characters. If plugin names ever come from outside the binary, that's a new slice with its own security review.
  - **Names are visible exactly where ids already are.** Insights is behind sign-in and already lists every source's id, so §18.7 is unchanged.
  - **The slice's PR adds** `crates/api/tests/golden/insights_*` and `crates/sources-plugins/src/catalogue.rs` to its own owns.
- **X-relevance-finish-flake:**
  - **CI's failure was** the success check right after `drop(held)` (`relevance_sets.rs:346`), not the one after `writer.finish()`.
  - **The cause:** both locks are `flock` locks, which belong to the open file description. On Linux and macOS, another test thread's spawn copies the descriptor into its child between fork and exec. The lock then outlives the test's drop for that moment. Windows doesn't copy non-inheritable handles, so it's unaffected.
  - **The fix is in the test:** a mutex around `spawn()` only, with the lock holders dropped while it's held. No wait and no sleep; every assertion stays.
- **X-size-ci-script:**
  - **`npm run size:ci`** runs CI's exact measurement through a small node wrapper, so it also works on Windows. `web.yml` calls it.
  - **CSS asset names** come from their modules under the same variable.
  - When it lands, CLAUDE.md and `runner/standards.md` say to measure with `npm run size:ci`. Those files are the runner's, so that change goes in a runner PR.

## Release notes (for the composer)
- **The Insights CSV export gains a `name` column after `key`,** in both `segments.by_source` and `discover.by_source` (X-insights-source-names). A tool that reads columns by name keeps working. One that reads them by position will find the later columns moved one place right.

## New slices
- **X-search-chunk-retry:** a retry state and message when the Search results chunk fails, and a `.catch` on `loadResults()`. It flips #1036's expected-gap case to assert the retry state.
- **X-group-chip-label:** group chips, Builder rows and facet-click rows show the facet rail's label when one is known. The query text stays lower-cased, under the same S1-20 rule. Low priority.
- **X-checksum-wrap:** the install dialog's 64-character checksum wraps 63+1 in its 478 px column. That predates #1034. Split it evenly, 32+32.
- **X-fixture-updates-on:** a `lodestar serve --fixture` variant with Updates and Backups switched on, so X-ui-checks-2's rebuild and paused states can also run against the real server. It's on the Rust side, so outside X-ui-checks. Its owns are refined at launch (the CLI and the e2e wiring).
