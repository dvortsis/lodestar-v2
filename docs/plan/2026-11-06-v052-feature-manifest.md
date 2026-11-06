# v0.5.2 feature manifest: the freeze list for the cut

Planning slice P-v052-manifest, 2026-11-06, for the build runner. This is the freeze that `docs/build-plan.md` §0.4 asks for before `release/v0.5.2` is cut from `main`:
- every behaviour that is on in the build, with its own acceptance;
- every partly landed feature, marked inactive or excluded, with what keeps it off.

The cut itself waits for the v0.5.1 tag (`ext:v051-tagged`, gate `P-v052-cut` in `runner/slices.yaml`).

**Authority:** `docs/owner-decisions.md` §2.13, §2.28, §2.39, §2.40; build-plan §0.4 (v0.5.2), §0.5 (C8/CR-COMB) and §0.2 (`sources.verify_unverified` stays off until C8 ends); `docs/releases/v0.5.2.md` (D-v052-notes); `docs/decisions/` from 2026-10-30 on. Recorded choices are in `docs/decisions/2026-11-06-P-v052-manifest.md`.

## 1. Baseline and how to read this

| | Commit | What it is |
|---|---|---|
| v0.5.1 cut | `c3f6341a` (#200 CR-0f, 2026-10-30) | Where `release/v0.5.1` split from `main` |
| v0.5.1 release branch | `dba1a5c0` (rc.4 docs, #463) | rc.4: the cut plus the ports below |
| This manifest | `origin/main` at `63a2c6df` (#511, 2026-11-05) | The 297 first-parent merges in `c3f6341a..63a2c6df` |

**Already on the release branch.** These PRs were ported to `release/v0.5.1`, so v0.5.1's own gate covers them: R-gauges (#219, rc.2), #357 (rc.3), #402 and #406 (rc.3), and #432, #433, #434, #442 and #448 (rc.4). LEGAL-1r (#457 on the release branch) is the release-branch build of the notice gate. `main`'s LEGAL-1 (#472) differs from it: notice version 3, shown as the first wizard step.

**Classes.** Every merge falls in exactly one class:

| Class | Meaning | Where | Count |
|---|---|---|---|
| Enabled | On in a default build: a new install, or a v0.5.1 data folder after the upgrade | §2 | 157 (9 already in v0.5.1) |
| Inactive | Shipped in the binary, kept off by a default or switch the user or operator can change | §3 | 28 |
| Excluded | Compiled in but unreachable: never registered or wired | §3 | 21 |
| No user-visible behaviour | Docs, plans, CI, tests, tooling, benches, refactors | §4 | 91 |

A PR that is partly on (for example RP-UC, on for fresh stores and off on upgraded ones) is listed in §2 and the note says what stays off.

**Merges after `63a2c6df`.** P-v052-cut classifies each one into these same classes in its delta report. A merge that adds a default-on behaviour, a migration or a re-soak trigger gets an addendum to this file before the cut (decisions file, D2).

## 2. Enabled behaviours

"Acceptance" names the test that proves the behaviour. The release gate in §5 and §6 (soaks, upgrade smoke, rollback check) covers the build as a whole.

| PR | Slice | Behaviour on in the build | Acceptance | Note |
|---|---|---|---|---|
| #210 | IMP-1 | Store migration V13: durable import job and candidate tables | `crates/store/tests/import.rs`, `crates/store/tests/swarm.rs` | **one-way migration V13** |
| #211 | (owner) freshness key | Updater accepts generation-0 freshness key; weekly re-sign job | unit tests in `crates/update/src/keys.rs`, `crates/update/src/minisign.rs` | update checks only; updates `mode` default `off` |
| #217 | S-settings-reconcile | One Settings implementation | `web/e2e/settings-form.routes.test.ts`, `web/e2e/settings.routes.test.ts` |  |
| #219 | R-gauges | Storage and cache size gauges on `/metrics` | `docs/soak/tools/linux/test_collect.py`; v0.5.1 rc.2 | **Already in v0.5.1.** Ported in rc.2 (#222) |
| #220 | (updater view) | Updater state read from a published view | unit tests in `crates/lodestar/src/update/engine.rs` |  |
| #221 | X-welcome-focus | Welcome heading focus ring | `web/e2e/smoke.test.ts` welcome snapshots |  |
| #223 | fix/pool-generation | Pool-wide lookup generations: stale timer can't expire a re-found key's lookup | `crates/sources/tests/pool.rs` `a_stale_timer_cannot_expire_a_rediscovered_keys_lookup` | unconditional fetch-pool fix — crawler-lifecycle re-soak trigger |
| #225 | IMP-2 | Magnetico SQLite import adapter | `crates/sources-importers/src/magnetico/tests.rs`; fuzz `magnetico_row` | reached through `lodestar import` (#257) |
| #230 | fix/logging-nits | Rotated logs numbered past every kept one of same ms | `crates/lodestar/src/logging/source_tests.rs` |  |
| #232 | fix/dht-queued-cancel-count | Metric counting admitted DHT requests dropped from the queue | CR-SIM golden `admission-pressure.txt` | metric only: counts in the default build (decisions `2026-10-30-dht-queued-drops.md`); crawl behaviour unchanged |
| #233 | CR-0d-2b-2 | Status says why the DHT isn't sampling yet | `crates/api/tests/ops.rs`, `crates/lodestar/src/ops/tests.rs`, `web/e2e/status-stalled.routes.test.ts` |  |
| #236 | CR-gate-hash-mismatch | `failed_verification` counts hash mismatches only | `crates/sources/tests/pool.rs`; CR-SIM goldens | metric semantics change in the fetch path; touches `peer-wire` and pipeline gate |
| #240 | F-filters-packs | Pack rules honour Neutralize terms | `crates/filter/tests/semantics.rs` |  |
| #241 | F-rollback-journal | `lodestar rollback` never brings back a deleted torrent | unit tests in `crates/store/src/snapshots.rs`, `crates/lodestar/src/cli/rollback.rs` |  |
| #243 | S-appearance | Appearance: theme picker and custom theme editor | `web/e2e/appearance.routes.test.ts`, `web/src/lib/features/appearance/ThemeEditor.test.ts` |  |
| #245 | F-rollback-holds | Rollback keeps safety holds recorded after the snapshot | unit tests in `crates/store/src/snapshots.rs`, `crates/lodestar/src/cli/rollback.rs` |  |
| #247 | S-theme-boot | Saved appearance applied at start-up | `web/e2e/theme-boot.routes.test.ts` |  |
| #248 | IMP-3 | Bitmagnet import adapter hardening (TLS, CLI) | `crates/sources-importers/src/bitmagnet/tests.rs`, `crates/lodestar/tests/cli.rs` | CLI only |
| #249 | LU-1-neutralize | LU-1 junk rule honours Neutralize terms | `crates/filter/tests/semantics.rs` |  |
| #250 | S-search-4 | Autocomplete, smart chips, natural phrasing, Builder | `web/e2e/search-assist.keyboard.test.ts` |  |
| #253 | IMP-4a | Import driver: filter, merge, provenance, removal | `crates/sources-importers/src/driver/tests.rs`, `crates/store/tests/import.rs` | CLI only |
| #254 | S-search-5 | Search table mode, column chooser, selection, bulk bar | `web/e2e/search-table.routes.test.ts`, `web/e2e/search-bulk.routes.test.ts` |  |
| #256 | S-search-6 | Live banner and Cards density | `web/e2e/search-cards.routes.test.ts`, `web/e2e/search-live.routes.test.ts` |  |
| #257 | IMP-4b | `lodestar import` CLI and docs | `crates/lodestar/src/cli/import/tests.rs` |  |
| #258 | S-insights-2 | Insights Fetch/Understand/Index/Serve stage pages | `web/e2e/insights-stages.routes.test.ts` |  |
| #259 | S-settings-2 | Settings → Language & region | `web/e2e/settings-screens2.routes.test.ts` |  |
| #262 | IMP-5 | Imports kept out of crawl-yield Insights; import annotations | `crates/api/tests/insights.rs`, goldens `crates/api/tests/golden/insights_*` |  |
| #263 | B-settings-sections a | Integrations, notifications, trackers, insights settings; sealed secrets | `crates/lodestar/src/settings/api_tests.rs`, `crates/lodestar/src/settings/screen_tests.rs` | **Tracker scraping is on by default:** `scrape.enabled` defaults to `true` (`crates/lodestar/src/config.rs:342-351`), unchanged since v0.5.1 |
| #264 | M-settings-engine-2 | On-demand section messages; Automatic snap-back fix | `web/src/lib/features/settings/form/SettingField.auto.test.ts` |  |
| #265 | S-insights-imports | Crawled/imported split and import markers in Insights | `web/e2e/insights-imports.routes.test.ts` |  |
| #266 | B-search-recovery | Per-clause counts and index state | `crates/api/tests/search_recovery.rs` |  |
| #267 | B-refetch-funnel | Start-time refetches kept out of the funnel | `crates/pipeline/tests/pipeline.rs` | pipeline coordinator change, default on |
| #269 | B-settings-sections b | Connection tests per integration; clear usage data | `crates/api/src/settings/checks/tests.rs`, `crates/api/tests/usage.rs` |  |
| #270 | S-locale-boot | Saved language, time zone, first day at start-up | `web/e2e/locale-boot.routes.test.ts` |  |
| #271 | S-search-recover-ui | "Removing X gives N" and index-state banners | `web/e2e/search-recover-ui.routes.test.ts` |  |
| #272 | B-search-live | Query-scoped live match counts (percolator) | `crates/api/tests/openapi.rs`; unit tests in `crates/search/src/percolate.rs` |  |
| #273 | B-settings-sections c | Send torrents to a download client (API) | `crates/api/tests/send.rs`, `crates/integrations/tests/{aria2,deluge,rtorrent,transmission}.rs` | needs a saved client |
| #274 | X-polish-1031 | Refetch queue, Insights floor, recovery cache fixes | `crates/pipeline/tests/pipeline.rs`, `crates/api/tests/search_recovery.rs` |  |
| #275 | S-search-live-ui | Live banner uses the `search_live` feed | `web/e2e/search-live.routes.test.ts` |  |
| #276 | B-import-server-1 | Server finishes imported jobs; one filter loader | `crates/lodestar/src/cli/import/tests.rs`, `crates/store/tests/import.rs` |  |
| #277 | S-settings-2b | Trackers, Integrations, Notifications, Insights & digest screens | `web/e2e/settings-screens2b.keyboard.test.ts` |  |
| #278 | X-search-files-perf | Compact file sets, LRU eviction, cancellable file clauses | `crates/search/tests/suggest.rs`; `bench/src/bin/search-bench.rs` |  |
| #279 | X-polish-1031b | Crawler polish, API schema guard, restore docs | `crates/dht/src/node/hint_tests.rs` | crawler hunks in hints (inactive) and pool |
| #280 | B-import-refile | Refile waiting import candidates when rules change | `crates/pipeline/tests/import_refile.rs` |  |
| #281 | S-insights-3 | Insights print layout and digest preview | `web/e2e/insights-print.routes.test.ts` |  |
| #282 | X-import-refile-2 | Import polish | `crates/pipeline/tests/import_refile.rs` |  |
| #283 | X-search-files-2 | Cold broad file clauses under 150 ms | `crates/search/src/lease/tests.rs`; gate-file-cold bench |  |
| #284 | S-logs-1 | Live log page `/logs` | `web/e2e/logs.keyboard.test.ts` |  |
| #285 | M-security-settings | Security screen: password, sessions, secure cookies | `web/e2e/settings-security.keyboard.test.ts` |  |
| #286 | X-polish-1031c | File-scan stop, re-queue shutdown, hint order bound | `crates/api/tests/filters.rs` | hint hunk inactive |
| #287 | B-held-exact | Re-hold only what the store still holds | `crates/api/tests/filters.rs` | content safety |
| #288 | M-security-settings-2 | API keys, feed tokens, trusted-proxy sign-in screens | `web/e2e/settings-security.keyboard.test.ts` |  |
| #289 | S-logs-2a | Settings → Logging: events, data kinds, opt-ins | `web/e2e/settings-logging.keyboard.test.ts`, `web/src/lib/features/settings/logging/privacy.test.ts` |  |
| #290 | X-requeue-2 | Re-queues stay pending while search is off | `crates/api/tests/filters.rs` |  |
| #291 | B-held-precommit | Refuse a new hold from before its commit | `crates/api/tests/filters.rs` | content safety |
| #293 | X-settings-registry-slim | Smaller Settings chunk (no behaviour change) | `web/src/lib/features/settings/shell/screens.test.ts`; `npx size-limit` |  |
| #294 | S-logs-2b | Logging outputs, log files, Danger zone, preview | `web/e2e/settings-logging.keyboard.test.ts` |  |
| #297 | X-polish-1031d | Re-queue list cap (`classify.requeue_all` key) | `crates/api/tests/filters.rs` | soak report columns are tooling |
| #298 | S-logs-3 | Logging event labels, console format labels | `web/scripts/event-names.test.ts`, `web/e2e/settings-logging.keyboard.test.ts` |  |
| #299 | S-about | Settings → About | `web/e2e/settings-about.keyboard.test.ts` |  |
| #300 | S-filters | Filters & Safety screen | `web/e2e/filters-screen.keyboard.test.ts` |  |
| #301 | X-security-confirm-teardown | Closed dialogs unmount on dismiss | `web/src/lib/features/settings/security/teardown.test.ts` |  |
| #302 | S-wizard | First-run wizard (part 1) | `web/e2e/wizard.keyboard.test.ts`, `web/e2e/welcome.routes.test.ts` |  |
| #303 | S-filters-2b | Adult facet labels | `web/src/lib/features/search/facets/FacetRail.test.ts` |  |
| #304 | X-alert-search-headroom | Bundle headroom for Alerts and search | `web/src/lib/features/alerts/components.test.ts`; `npx size-limit` |  |
| #305 | X-pipeline-1101 | Backfill retry with backoff | `crates/pipeline/tests/pipeline.rs` | pipeline, default on |
| #306 | S-updates | Settings → Updates (part 1) | `web/e2e/settings-updates.keyboard.test.ts` |  |
| #307 | X-stale-tab | "A newer Lodestar is running" banner | `web/e2e/stale-tab.keyboard.test.ts` |  |
| #308 | X-web-hygiene-1101 | CSS headroom, first-load guard, chunk-load catches | `web/scripts/first-load.test.ts` |  |
| #309 | S-wizard-2 | Wizard part 2: download client, rule packs | `web/e2e/wizard.keyboard.test.ts`, `web/src/lib/features/wizard/PartTwo.test.ts` |  |
| #310 | S-updates-2 | Updates → Advanced | `web/e2e/settings-updates.keyboard.test.ts` |  |
| #312 | B-update-5 | Download, staging and Install now (API) | `crates/api/tests/api.rs`, goldens `crates/api/tests/golden/updates_install*.txt` | user-initiated; scheduled `updates.mode` = `off`; release-image supervisor check pending (P-v052-cut) |
| #313 | X-detail-headroom | Drawer loads settings on demand; magnet load toasts | `web/src/lib/features/detail/commands.failure.test.ts` |  |
| #315 | H-zh | Missing Chinese messages filled | `web/scripts/messages.zh-complete.test.ts` |  |
| #316 | S-filters-2c | Block this… from result menu and drawer | `web/e2e/filters-block.keyboard.test.ts` |  |
| #317 | X-illustrations | Appearance toggle and Save-Data hide state illustrations | `web/e2e/illustrations.routes.test.ts` |  |
| #318 | S-updates-3 | Install now wired on Updates screen | `web/e2e/settings-updates.keyboard.test.ts` |  |
| #319 | X-settings-polish | Settings engine follow-ups | `web/src/lib/features/settings/shell/SettingsSection.test.ts` |  |
| #321 | S-send | Send to client in menu, drawer, `/t` and `s` key | `web/e2e/search-results.keyboard.test.ts`, `web/src/lib/features/detail/send.test.ts` |  |
| #322 | X-discover-headroom | Discover route headroom | `web/src/lib/features/discover/DiscoverPage.test.ts` |  |
| #324 | B-update-5b | Roll back from the API | goldens `crates/api/tests/golden/updates_rollback.txt` |  |
| #325 | X-flows-door-2 | Appearance and wizard routed through flows door | `web/src/lib/features/wizard/PackStep.door.test.ts` |  |
| #326 | S-updates-4 | Roll back on Updates screen; plain install-failure words | `web/e2e/settings-updates.keyboard.test.ts` |  |
| #327 | X-detail-polish | Block and Send follow-ups | `web/src/lib/features/detail/DetailActions.test.ts` |  |
| #328 | X-web-polish-1101b | a11y and correctness follow-ups | `web/src/lib/features/insights/overview/components.test.ts` |  |
| #330 | B-update-6 | Updater: fail-closed check, status cache, `restores_store` | goldens `crates/api/tests/golden/updates_*.txt` |  |
| #331 | X-web-polish-1101c | Theme-pick flicker fix, snapshot chart unit | `web/src/lib/features/appearance/boot.test.ts` |  |
| #332 | X-search-headroom-2 | `s` waits for the torrent; neutral flows error | `web/src/lib/features/detail/send.test.ts` |  |
| #333 | X-web-polish-1101d | Keep the place after Block | `web/e2e/filters-block.keyboard.test.ts` |  |
| #334 | S-updates-5 | Roll back dialog says whether data goes back too | `web/src/lib/features/settings/updates/updates.test.ts` |  |
| #335 | B-search-meta | Approximate suggestion counts, field catalogue | `crates/api/tests/suggest.rs`, goldens `search_fields_adult_{hidden,shown}.txt` |  |
| #336 | S-privacy-banner | Banner when identifying data is recorded | `web/e2e/privacy-banner.routes.test.ts`, `crates/api/tests/insights.rs` |  |
| #337 | F-suggest-adult-field | `/suggest` offers no `adult:` while Adult is hidden | `crates/api/tests/suggest.rs` | content safety |
| #338 | S-search-meta-web | Builder reads fields from server; "≈" on estimates | `web/src/lib/features/search/assist/fields.test.ts` |  |
| #339 | X-web-polish-1101e | Bulk bar closes stale manual-copy box | `web/src/lib/features/search/bulk/BulkBar.test.ts` |  |
| #341 | X-file-clause-cold | Cold broad `file:` clauses in saved-search percolation scan once, cancellable | `crates/search/src/percolate.rs` tests `a_cancelled_file_scan_stops_the_run`, `the_first_search_keeps_its_file_scan_whole` |  |
| #342 | FX-1 | Illustrated empty/error states follow the §10.19 layout | `web/e2e/states.test.ts` snapshots, `web/e2e/alerts.routes.test.ts`, `web/e2e/insights.routes.test.ts` |  |
| #343 | FX-2 | Search narrow-width and touch fixes (builder, bulk, facets) | `web/e2e/search-query.routes.test.ts`, `web/e2e/search-table.routes.test.ts` |  |
| #345 | FX-4 | Settings form and copy fixes | `web/e2e/settings-form.routes.test.ts`, `web/e2e/settings-form.fixture.routes.test.ts` |  |
| #346 | FX-3 | Keyboard preferences apply at once | `web/src/lib/features/shortcuts/KeyboardScreen.test.ts`, `web/e2e/shortcuts.keyboard.test.ts` |  |
| #347 | FX-5 | Detail page and shell fixes | `web/e2e/detail.routes.test.ts`, smoke snapshots | also fixed main's size-limit red |
| #348 | FX-6 | Insights and Logs consistency (charts, links, copy) | `web/e2e/insights-stages.routes.test.ts` |  |
| #350 | FX-6-I9 | Query printer shows IEC size bounds (stage 1) | `crates/search/tests/query_props.rs::byte_ranges_round_trip`, `crates/api/tests/golden/parse_natural.txt` | stage 2 (`i9-stage2`) not landed |
| #353 | X-appearance-undo | Appearance Undo applies to the latest change and keeps focus | `web/src/lib/features/appearance/AppearanceScreen.test.ts`, `prefs.test.ts` |  |
| #357 | F-filter-load-fail-closed | Unreadable stored filter rows fail closed (hold, never pass) | `crates/filter/tests/load_fail_closed.rs`, `crates/pipeline/tests/filter_load.rs` | **Already in v0.5.1.** Ported to release/v0.5.1 for rc.3 (#377) |
| #358 | X-web-followups-1101f | Discover keys notice; first-visit scene hides | `web/src/lib/features/discover/DiscoverPage.keys.test.ts`, `web/e2e/illustrations.routes.test.ts` |  |
| #361 | SRC-16 | Settings → Jobs screen: progress, pause/resume/cancel of background jobs | `web/e2e/settings-jobs.keyboard.test.ts` ("shows progress, rate and ETA, and passes axe", "pauses, resumes and cancels by keyboard") |  |
| #362 | B-subscriptions-api | Blocklist subscriptions API: connect, accept, pause, remove | `crates/api/tests/filters/subscriptions.rs::nothing_is_subscribed_by_default_and_listing_connects_nothing`, `::connecting_needs_no_password_and_shows_its_strictness` | nothing subscribed by default; list fetches ignore HTTPS_PROXY |
| #367 | F-filter-standin-undo | Stand-in holds are re-judged when failed filter rows read again | `crates/api/tests/standin_undo.rs::a_rules_stand_in_holds_are_judged_again_when_it_reads`, `::a_rejudge_runs_beside_apply` | not in v0.5.1 (rc.3/rc.4 stand-in gap; build-plan §0.2); adult stand-in holds stay held (§2.28) |
| #370 | S-blocklists | Settings → Blocklists screen and optional setup-wizard step | `web/e2e/settings-blocklists.keyboard.test.ts` ("connects nothing by default, and passes axe", "connecting shows the strictness first…", "removing asks for the password…") | nothing subscribed by default |
| #373 | X-strip-format-chars | Unicode Cf characters stripped from feed titles, display names and digests | `crates/metainfo/src/display.rs` tests (`table_is_sorted_and_disjoint`, `matches_the_reference`); `crates/sources-feeds/tests/rss_props.rs`, `torznab_props.rs` | metainfo display path applies to DHT torrents too |
| #379 | SRC-1 | Store migration V14: sources, claims, unverified items, snapshot scrub (one-way) | `crates/store/src/schema.rs::v14_upgrades_a_v13_store_with_data`, `::v14_on_a_fresh_store_matches_an_upgraded_one`, `::older_builds_refuse_a_newer_store_untouched`; `crates/store/tests/unverified.rs` | one-way: the upgrade snapshots to `<db>.pre-v<N>.db` (N = first pending version, so pre-v13 from a v0.5.1 V12 store) before migrating; the tables stay empty unless a source runs |
| #381 | F-normalize-invisible | Filter matching ignores invisible/format characters in names | `crates/filter/tests/normalize_reference.rs`, `crates/filter/tests/two_readings.rs::a_term_split_off_still_hits` | can change filter verdicts for names it now reads differently; no re-judge pass covers existing rows (§5.2 step 3) |
| #385 | X-egress-copy | Blocklists screen says list fetches ignore HTTPS_PROXY; user guide too | `web/src/lib/features/blocklists/BlocklistsScreen.test.ts` | copy |
| #386 | F-hits-two-readings | Filter hit reports agree across both name readings | `crates/filter/tests/hits_two_readings.rs::a_visible_mark_agrees_both_ways`, `::the_fuzz_crash_agrees` |  |
| #387 | F-visible-format-two-readings | Visible format marks hit both ways in filters | `crates/filter/tests/two_readings.rs::a_visible_mark_hits_both_ways`, `crates/filter/tests/hits_two_readings.rs` |  |
| #390 | X-overflow-links | Overflow menu drops dead Settings links; adds Logs and Syntax guide | `web/src/lib/shell/nav.test.ts`, `web/e2e/shell.routes.test.ts` |  |
| #391 | X-settings-overview | Settings Overview: needs-attention and changed-from-default | `web/src/lib/features/settings/overview/logic.test.ts`, `web/e2e/settings-overview.keyboard.test.ts` |  |
| #393 | X-polish-carry-1102 | Small polish: digest/insights copy, backup CLI text, logging messages | existing tests in `crates/api/tests/digest.rs`, `crates/api/tests/insights.rs` | minor copy |
| #402 | R-mem-triage | Fetch-pool lease leak fixed | `crates/pipeline/tests/pipeline.rs`; `bench/examples/mem-churn.rs` | **Already in v0.5.1.** Ported to release/v0.5.1 for rc.3 |
| #406 | X-mmap-cache-prune | Search mmap cache pruned; new cache gauge | `bench/tests/mmap_retention.rs` | **Already in v0.5.1.** Ported to release/v0.5.1 for rc.3 |
| #415 | SB-6 | Search schema v6 (new fields), blue/green rebuild on upgrade | `crates/search/tests/rebuild.rs::searches_keep_working_through_a_rebuild_and_the_swap`, `::an_interrupted_rebuild_resumes_from_its_watermarks`; `crates/search/tests/index.rs::an_index_from_before_population_tracking_backfills_every_produced_field` | one-way index rebuild (v5 → current) |
| #418 | SB-6 (b) | Rebuild swap never drains a population walk | `crates/search/tests/rebuild.rs::a_stopped_rebuild_resumes_at_the_next_start` |  |
| #420 | L4-followups | Compare (other versions) honours the request cancel token | `crates/api/src/compare.rs::a_cancel_during_the_candidates_stops_at_the_next`, `::other_versions_are_found_under_a_live_token` | rest of the PR is an audit record |
| #424 | RP-0 | Rule-pack versioning: pack versions recorded, off → on accepts current rules, SC-2 seeded | `crates/api/src/filters.rs::pack_versions_are_seeded_once_and_undo_leaves_them`; `crates/filter/src/packs/upgrade.rs::records_round_trip`; goldens `filters_overview_fresh.txt`, `filters_overview_full.txt` |  |
| #426 | SRC-3 | Search fields `verified:`, `source:`, `network:`; unverified hidden by default | `crates/search/tests/provenance.rs::every_torrent_is_verified_clearnet_and_carries_its_sources`, `crates/search/src/query.rs::unverified_items_are_hidden_unless_asked_for`, `provenance.rs` `::verified_no_finds_unverified_documents_only_when_asked` | v6 generation backfill on existing indexes; every DHT torrent is `verified:yes network:bt source:dht` |
| #429 | X-search-files-3 | Faster file-size searches (bucket terms, block-read join) | `crates/search/tests/files.rs::size_ranges_match_a_scan_at_every_bucket_edge`, `::nested_size_ranges_match_a_scan`; `bench/src/bin/search-bench.rs` |  |
| #432 | X-log-url-redact | URL credentials after a stray scheme separator scrubbed from logs | `crates/log/src/url.rs` unit + proptests | **Already in v0.5.1.** Ported to release/v0.5.1 (rc.4) |
| #433 | X-mem-gauges | `/metrics` gains SQLite, mimalloc, RSS and segment memory gauges | `crates/lodestar/src/memstats.rs`, `crates/store/src/memstats.rs` units; `docs/soak/tools/test_analyze_cr0.py` | **Already in v0.5.1.** Ported to release/v0.5.1 (rc.4) |
| #434 | X-thp-off | Worker disables transparent huge pages by default | `crates/lodestar/tests/thp.rs` | **Already in v0.5.1.** default on (THP off); ported to release/v0.5.1 |
| #442 | X-mem-gauges-2 | Negative SQLite counts absent from gauges; restart check matches gate | memstats units; `docs/soak/tools/test_analyze_cr0.py` | **Already in v0.5.1.** Ported to release/v0.5.1 |
| #447 | X-legal-docs | Legal Notice, operator guidance published; Unraid templates and Dockerfile labels updated | No automated check of the shipped text: P-v052-cut reviews the Unraid templates and Dockerfile labels in the release image; the notice text itself is tested in `crates/api/src/legal.rs` | template text change ships in packaging |
| #448 | X-mimalloc-override | One allocator (mimalloc) for Rust and C in release build | `.github/workflows/release.yml` nm check; soak r5 memory gate | **Already in v0.5.1.** Ported to release/v0.5.1 (rc.4) |
| #454 | RP-DEF | Fresh data directory starts with every pack on; wizard pack step lets user uncheck | `crates/pipeline/tests/pack_seed.rs` (`a_fresh_store_has_every_pack_on_at_the_first_filter_read`, `an_upgraded_store_keeps_every_packs_state`); `crates/filter/tests/pack_upgrade.rs`; `web/e2e/wizard.keyboard.test.ts` | upgraded stores keep pack state (offered, never switched); touches `store/src/schema.rs` |
| #455 | SRC-15a | Settings → Sources lists sources with status, toggles, priority, yield | `web/e2e/settings-sources.keyboard.test.ts`; theme snapshots | screen is on; every source row it lists is off by default |
| #460 | X-log-url-redact-2 | Remaining URL-credential leak classes closed in logs | `crates/log/src/url.rs` unit + proptests (`crates/log/proptest-regressions/url.txt`) | main only (release notes line pending on it) |
| #462 | SRC-17a | API carries provenance, `verified` and `source` fields and Source facet | `crates/api/tests/provenance.rs` (`a_feed_never_names_a_source`, `an_export_carries_no_provenance`); `crates/api/tests/facets.rs` goldens | fields present by default; with all sources off every row is verified/crawled |
| #465 | RP-2 | Pack-rule overrides bound to base version; weakening needs password | `crates/api/tests/pack_overrides.rs`; `crates/filter/tests/pack_overrides.rs`, `props.rs` | API only; no screen until RP-6 (#487) |
| #468 | X-search-files-4 | Torrent-level `file:(ext:…)` and a cheaper file scan | `crates/search/tests/files.rs`, `query.rs`; `docs/query-language.md` | uses v7 index terms; release-note "faster extension searches" |
| #469 | RP-UC | Unauthorised-copy pack (UC-1..3) and MW-6 cracked software | `crates/filter/tests/unauthorised_copy.rs` (`a_fresh_store_has_it_on_and_applied`, `an_upgraded_store_has_unauthorised_copy_off_until_turned_on`, `mw6_is_in_shadow_on_an_upgraded_store_with_malware_on`, `lawful_names_are_never_held`, `precision_on_the_parser_corpus`) | On for fresh stores. Upgraded store: pack off until turned on; MW-6 in shadow. Bumps classify (`crates/classify/src/junk.rs`) |
| #470 | D-sources-guide | User guide for Settings → Sources with screenshots | `web/e2e/docs/shots.docs.ts` | user guide only |
| #472 | LEGAL-1 | Legal Notice v3 as first wizard step; login gate `409 notice_required` until accepted | `crates/api/tests/legal.rs`; goldens `setup_notice_required.txt`, `auth_status_notice_required.txt`, `legal_accept_stale.txt`; `crates/api/src/legal.rs` units | main-side port of LEGAL-1r (#457 on release/v0.5.1): same notice gate and NoticeBox, but the notice is the wizard's first step (LEGAL-1r put it between token and password); behaviour, not commits |
| #474 | SRC-17 | Detail drawer provenance, Show unverified toggle and badge, Source facet | `web/src/lib/features/detail/provenance/Provenance.test.ts`; `web/src/lib/features/search/unverified/Unverified.test.ts`, `toggle.test.ts` | Empty in practice: Show unverified defaults off; unverified rows exist only when a source is on |
| #478 | RP-3 | Packs API: versions, changelogs, per-rule state, overrides, pending versions, corpus results | `crates/api/tests/filters_packs.rs`, `pack_overrides.rs`; goldens `filters_*.txt` | API only |
| #483 | X-log-url-redact-3 | Log-redaction fast path and narrowings | `crates/log/src/url.rs` units + proptests | perf of every log line |
| #485 | RP-1b | MW-2@2 and FK-2@2 guards added to malware/fakes packs | `crates/filter/tests/rp1b_guards.rs`, `pack_corpus.rs`, `pack_upgrade.rs` | pack version bump; upgraded stores get pending versions |
| #486 | X-filter-perf-1105 | Filter-budget headroom regained (classify/prefilter) | `crates/filter/tests/props.rs` budget | no behaviour change intended |
| #487 | RP-6 | Filters & Safety → Packs: versions, changelog, pending, override editor, corpus status | `web/e2e/filters-packs.keyboard.test.ts`; `web/src/lib/features/filters/screens/packs/packs.test.ts`, `flows/machine.test.ts` | supersedes "API only" line in release notes |
| #488 | X-log-url-redact-4 | Credential value after a name past a URL span hidden in logs | `crates/log/src/url.rs` units |  |
| #492 | X-log-url-redact-5 | Redaction reads look-alike separators and spaces | `crates/log/src/url.rs` + `proptest-regressions/url.txt` |  |
| #493 | SRC-13c | Exact-host consent for sources; pre-release consents switch off once | `crates/api/tests/ops.rs` goldens `ops_source_consent_{given,required,stale}.txt`; `crates/pipeline/tests/control.rs` | The consent model is on; the sources stay off. v0.5.1 had no such sources → no effect on upgrade from v0.5.1 |
| #494 | X-source-adult-2 | Always-adult mask on API correction path and imports | `crates/api/tests/source_adult.rs`; `crates/lodestar/src/cli/import/tests.rs`; `crates/sources-importers/src/driver/tests.rs` | import CLI path is reachable today; source path off |
| #495 | RP-4 | SP-3 flooding: bounded per-publisher near-duplicate rate in the writer | `crates/filter/tests/flood.rs`; `crates/lodestar/tests/sp3_live_feeds.rs`; `crates/filter/tests/pack_upgrade.rs` | spam pack; on for fresh stores (RP-DEF). The flood map stays empty while every source is off (`crates/pipeline/src/writer.rs:1831-1835`, `Flooding::count` at `:2093-2096`), so no memory load in v0.5.2 |
| #499 | LEGAL-2 | Crawler and sources paused on a fresh data directory until setup completes | `crates/lodestar/tests/setup_hold.rs` (`the_crawler_waits_for_setup_across_a_restart_then_starts`, `a_pause_the_user_chose_stays_after_setup`); `crates/pipeline/tests/setup_hold.rs` (`an_instance_already_set_up_starts_unheld`) | crawler lifecycle change → re-soak trigger; upgraded instances start unheld |
| #505 | X-search-files-5 | Torrent-level ext × size-bucket and size terms; search schema v7 | `crates/search/tests/files.rs`, `index.rs`, `rebuild.rs` | `SCHEMA_VERSION` 6 → 7: one rebuild v5 → v7 on upgrade |
| #507 | X-adult-catalogue | Import CLI's always-adult mask read from one manifest catalogue | `crates/sources-plugins/tests/manifests.rs` (`each_entry_is_its_plugins_own_manifest`); `crates/lodestar/src/cli/import/tests.rs` | refactor of #494's mask |
| #509 | B13b | Siblings for short no-year titles via title-identity key | `crates/api/tests/compare.rs` (`siblings_without_a_title_or_search`, `siblings_respect_hidden_and_adult`) | needs v7 index field (rebuild) |

## 3. Inactive and excluded features

### 3.1 What keeps each one off

No packaging file (`packaging/`, the Dockerfile, the Unraid templates) sets any of these switches. Release builds pass `--no-default-features --features web` (`.github/workflows/release.yml`, `packaging/docker/Dockerfile:84`), which leaves the `fixture` feature out.

**Registered sources.** `crates/lodestar/src/services.rs:235-275` registers exactly three plugins:
- the DHT (on);
- the torrent cache;
- the Sonarr/Radarr webhooks.

Every other plugin in `crates/sources-plugins` is compiled in (`catalogue.rs:28-40`) but never registered, so it can't appear or be turned on. A plugin seen for the first time starts on only if its manifest says so and it needs no consent (`crates/pipeline/src/lib.rs:1252` `first_state`). The DHT is the only one that does.

| Feature (slices) | Key or mechanism | Default | State | Test proving the default |
|---|---|---|---|---|
| Safe restart (CR-0d) | `dht.safe_restart`, `LODESTAR_DHT_SAFE_RESTART` (`crates/lodestar/src/config.rs:174, 219`) | `false` | inactive; **the off path still takes a lock file, see §6** | `config.rs::tests::safe_restart_is_off_unless_asked_for` |
| Coordinated admission (CR-ADM) | `dht.coordinated_admission` (`config.rs:183, 221`) | `false` | inactive | `config.rs::tests::coordinated_admission_is_off_unless_asked_for` |
| Responder hints (CR-1a-H) | `dht.responder_hints` (`config.rs:187, 222`) | `false` | inactive | `config.rs::tests::responder_hints_is_off_unless_asked_for` |
| Streaming peers (CR-1a-S) | `dht.streaming_peers` (`config.rs:191, 223`) | `false` | inactive | `config.rs::tests::streaming_peers_is_off_unless_asked_for` |
| 256-prefix targets (CR-1b) | `dht.target_prefixes` (`config.rs:195, 224`) | `false` | inactive | `config.rs::tests::target_prefixes_is_off_unless_asked_for`; `crates/dht/src/node/prefix_tests.rs::switch_off_draws_each_target_uniformly_as_before` |
| uTP fetches, IPv6 route back-off (CR-4) | `dht.utp`, `dht.route_backoff` (`config.rs:200-204, 225-226`) | `false` | inactive | `config.rs::tests::utp_and_route_backoff_are_off_unless_asked_for`; `crates/dht/tests/utp_demux.rs::without_utp_a_utp_datagram_is_malformed_krpc_as_before` |
| Settings → Transport uTP (S04) | `transport.utp` (`crates/lodestar/src/settings/transport.rs:20-25`) | `false` | inactive: stored and shown as "not active yet"; no effect at run time | `settings/tests.rs::the_default_sections_match_the_config_defaults` |
| Torrent cache (SRC-13, 13b, 13c) | manifest `enabled_by_default: false`, External consent per listed host. The host list, the consent and the switch are API-only: the consent dialogs and config forms are SRC-15b, which waits on `ext:owner-q1` | off | **can't be turned on in v0.5.2 once X-consent-gate-052 lands (§3.4):** no source that needs consent can be enabled until SRC-15b ships owner-approved consent text | `crates/sources-plugins/tests/torrent_cache.rs::manifest_is_external_consent_and_off_by_default`; `crates/pipeline/tests/torrent_cache.rs::no_feed_while_the_plugin_is_off`; `crates/lodestar/src/ops/tests.rs::a_config_written_through_the_api_turns_the_cache_off_until_acknowledged`; X-consent-gate-052's binary test |
| Sonarr/Radarr webhooks (SRC-11b, 11c) | manifest `enabled_by_default: false`, no consent needed; enabling and the token are API-only (the token is made with the password) | off | inactive: **advanced, API only** | `crates/sources-plugins/tests/arr.rs::caps_consent_and_endpoints`; `crates/api/tests/webhooks.rs::dropped_calls_say_why_and_unwired_webhooks_read_nothing`; `crates/lodestar/tests/webhooks_e2e.rs::a_grab_with_the_token_reaches_the_store_private_and_a_wrong_one_never_does` |
| Nyaa, Sukebei, Internet Archive, Academic Torrents, Prowlarr/Jackett, download-client lister, private-tracker full scrape (SRC-6..12b) | not registered | n/a | excluded | each manifest's `manifest_and_policy` or `caps_consent_and_endpoints` test asserts off by default. **Nothing tests that they stay unregistered** (§3.3, G1) |
| ThePornDB, StashDB (ENR-2b..5, X-enr-page-flag) | HOLD: not registered, can't be turned on | n/a | excluded | `crates/sources-plugins/tests/{tpdb,stashdb}.rs::manifest_and_policy` |
| Adult-enrichment runtime (ENR-1, 1b, 1c) | not wired: `pipeline::Config.adult_enrichers` defaults to empty (`crates/pipeline/src/lib.rs:164, 211`); its gate also needs Show adult content | n/a | excluded | eligibility: `crates/pipeline/tests/adult_enrich.rs`. **Nothing tests that it stays unwired** (G2) |
| Enrichment consumers (ENR-6, 7, 8) | need enrichment data that nothing produces | n/a | inactive | `crates/api/tests/adult_enrichment.rs::interest_is_offered_only_with_show_adult_content_on` |
| Verify unverified keys (SRC-2) | `sources.verify_unverified` (`crates/pipeline/src/lib.rs:151, 208`); no config or Settings key reaches it; the API's `SourcePatch` refuses it | `false` | inactive until C8 ends (build-plan §0.2, Track S) | `crates/api/src/ops.rs::tests::not_yet_editable_fields_are_named`. **The pipeline default has no test** (G3) |
| Unverified items, claims, promotion, purge, per-source settings (SRC-2, 2b, 4, 5, 14a, 14b, 14b-2, X-claim-*) | act only on claims from sources, and every source is off | n/a | inactive | `crates/pipeline/tests/control.rs::sources_that_arent_enabled_dont_run_and_consent_gates_enabling`, `::first_registration_is_one_write_so_a_crash_cant_leave_a_gated_source_on` |
| Show unverified (SRC-17) | toggle, off by default; no unverified rows exist while sources are off | off | enabled screen, empty in practice (§2, #474) | `web/src/lib/features/search/unverified/toggle.test.ts` |
| In-app updates on a schedule (B-update-5..6, S-updates-*) | `updates.mode` (`config.rs:459-472`) | `Off` | inactive. Install now and Roll back are user actions (§2) | `crates/lodestar/src/settings/updates.rs::tests::off_by_default` |
| Nightly channel | no embedded key (`crates/update/src/keys.rs:17`) | none | excluded | `keys.rs::tests::embedded_keys_match_packaging` |
| Updater on Windows | `#[cfg(unix)]` (`crates/lodestar/src/update/mod.rs:23-31`) | n/a | excluded | compile-time |
| Import through the running server (B-import-server-2) | not built (`ext:owner-import-server-surface`) | n/a | absent | n/a |
| Cluster, companions (`crates/cluster`) | not linked into `crates/lodestar` | n/a | absent | n/a |
| Blocklist subscriptions (B-subscriptions-api, S-blocklists) | nothing subscribed until the user connects one (owner §2.39 leaves them off) | none | enabled screen, nothing connected | `crates/api/tests/filters/subscriptions.rs::nothing_is_subscribed_by_default_and_listing_connects_nothing` |
| Rule packs on an upgraded store (RP-DEF, RP-UC) | an explicit off row for every pack without one, plus the `packs.defaults` marker | previous state | the user is offered packs, never switched (owner §2.39) | `crates/pipeline/tests/pack_seed.rs::an_upgraded_store_keeps_every_packs_state`; `crates/filter/tests/unauthorised_copy.rs::an_upgraded_store_has_unauthorised_copy_off_until_turned_on` |
| Not built: FK-3, SP-4 (`crates/filter/src/packs/mod.rs:11-13`) | absent | n/a | absent | n/a |
| New rules in a pack already on (RP-0, RP-1b, MW-6) | applied-version rows | shadow (reported, not enforced) until accepted | inactive until the user accepts | `crates/filter/tests/unauthorised_copy.rs::mw6_is_in_shadow_on_an_upgraded_store_with_malware_on` |
| Show adult content | `content.show_adult`, `LODESTAR_SHOW_ADULT` (`config.rs:432-436`) | `false` | unchanged from v0.5.1 | `crates/lodestar/src/settings/content.rs::tests::hidden_by_default`; `crates/api/tests/adult_visibility.rs::adult_is_hidden_by_default_in_results_and_counts` |

**Crawler switches stay off in v0.5.2.** None of them has passed C8 (CR-COMB hasn't run), so per build-plan §0.4 all seven `dht.*` switches stay at their defaults.

### 3.2 Each inactive or excluded PR

| PR | Slice | State | Feature | Kept off by | Test or proof it stays off |
|---|---|---|---|---|---|
| #208 | CR-0d (part 1) | inactive | Restart-proof file format, lock and validation | used only with `dht.safe_restart` (default `false`) | `crates/dht/src/restart/tests.rs` (+ proptest, fuzz `dht_restart`) |
| #216 | CR-0d-2a | inactive | DHT actor drives restart proof (resume sampling after clean restart) | `dht.safe_restart` = `false`. With the switch off a run still writes `dht-restart.lock` and an `off` marker (decisions 2026-10-30-CR-0d) — crawler-lifecycle change, re-soak trigger | `crates/lodestar/src/config.rs` `safe_restart_is_off_unless_asked_for`; `crates/dht/src/node/proof/tests.rs` |
| #227 | CR-0d-2b-1 | inactive | Packaged installs get restart witness dirs | Dockerfile/systemd set `LODESTAR_DHT_WITNESS_DIR`; used only with `dht.safe_restart` | `crates/lodestar/src/config.rs` `the_default_witness_dir_needs_a_home` |
| #228 | CR-ADM | inactive | Bounded admission of DHT peer requests | `dht.coordinated_admission` = `false` | `crates/lodestar/src/config.rs` `coordinated_admission_is_off_unless_asked_for`; `crates/sources/tests/admission.rs` |
| #231 | fix/cr-adm-polish | inactive | CR-ADM follow-ups | `dht.coordinated_admission` = `false` | `crates/sources/tests/admission.rs` |
| #246 | CR-1a-H | inactive | Responder hints for peer lookups | `dht.responder_hints` = `false` | `crates/lodestar/src/config.rs` `responder_hints_is_off_unless_asked_for`; `crates/dht/src/node/hint_tests.rs` |
| #251 | CR-1a-S (1) | inactive | Streaming peer batches to the fetch pool | `dht.streaming_peers` = `false` | `crates/lodestar/src/config.rs` `streaming_peers_is_off_unless_asked_for`; `crates/sources/tests/streaming.rs` |
| #252 | CR-1a-S (2) | inactive | Streaming peers: CR-SIM screen and metrics | `dht.streaming_peers` = `false` | `crates/dht/src/node/stream_tests.rs` |
| #359 | CR-1b | inactive | 256-prefix DHT lookup targets | `dht.target_prefixes` = false (config.toml / `LODESTAR_DHT_TARGET_PREFIXES`; not in UI); stays off unless it passes C8/CR-COMB. Adds `dht_target_novel_nodes_total` metric (always exported) | `crates/dht/src/node/prefix_tests.rs::switch_off_draws_each_target_uniformly_as_before`; `crates/lodestar/src/config.rs::target_prefixes_is_off_unless_asked_for` |
| #363 | SRC-6 | excluded | Torznab/Newznab XML parser | used only by the Prowlarr plugin, which isn't registered | `crates/sources-feeds/tests/torznab_golden.rs`, `torznab_limits.rs`, `torznab_props.rs`; fuzz `fuzz/fuzz_targets/torznab.rs` |
| #365 | CR-4 | inactive | uTP fetch transport and IPv6 route backoff | `dht.utp` = false, `dht.route_backoff` = false (config.toml / env; not in UI); off unless C8/CR-COMB passes. Touches DHT actor and fetch paths: delta re-soak trigger | `crates/lodestar/src/config.rs::utp_and_route_backoff_are_off_unless_asked_for`; `crates/dht/tests/utp_demux.rs::without_utp_a_utp_datagram_is_malformed_krpc_as_before` |
| #366 | SRC-8 | excluded | RSS 2.0/Atom parser with the `nyaa:` namespace | used only by Nyaa/Academic plugins, not registered | `crates/sources-feeds/tests/rss_golden.rs`, `rss_limits.rs`, `rss_props.rs`; fuzz `fuzz/fuzz_targets/rss.rs` |
| #380 | SRC-5 | inactive | Per-plugin egress (proxy, interface, kill switch) | only used by a registered HTTP source: torrent cache (off by default, `crates/sources-plugins/tests/torrent_cache.rs::manifest_is_external_consent_and_off_by_default`) | `crates/sources/src/egress_tests.rs::the_kill_switch_abandons_a_request_in_flight_and_refuses_until_up`, `::manifest_endpoints_are_public_only_and_limited_to_declared_hosts` |
| #382 | SRC-9 | excluded | Internet Archive JSON parser | plugin not registered | `crates/sources-feeds/tests/archive_org_golden.rs`, `archive_org_limits.rs`, `archive_org_props.rs`; fuzz `fuzz/fuzz_targets/archive_org.rs` |
| #383 | SRC-2 | inactive | Writer stores unverified items from source claims; verification cursor | claims come only from sources (all off); `sources.verify_unverified` false with no setting (`crates/pipeline/src/lib.rs:208`; API refuses it, `crates/api/src/ops.rs:1048`). Adds `JunkReason::ClaimMismatch` | `crates/pipeline/tests/unverified.rs::clean_claims_are_stored_as_unverified_items`, `::floor_and_core_claim_hits_hold_and_are_never_stored` |
| #388 | SRC-4 | inactive | Source consent classes, credentials, endpoint acknowledgement; start-time disable check | `/sources` API reachable; acts only on sources, all off by default. The start check runs every start but changes nothing when no source is enabled | `crates/api/tests/golden/ops_source_consent_*.txt` via `crates/api/tests/ops.rs` |
| #392 | SRC-2b | inactive | Percolator alerts on promoted (once-unverified) torrents | promotion needs source claims (off) | `crates/search/tests/promoted.rs::a_promoted_torrent_below_the_watermark_becomes_searchable`; `crates/lodestar/src/percolator.rs` (`a_promoted_id_below_the_watermark_alerts_once`) |
| #394 | SRC-14a | inactive | Per-source settings: egress, schedule, agents via `PATCH /sources/{id}` | applies only to sources (all off by default) | `crates/api/tests/ops.rs` with goldens `ops_source_egress_*.txt`, `ops_source_schedule_invalid.txt`, `ops_source_agents_zero.txt` |
| #395 | SRC-7 | excluded | Prowlarr/Jackett indexer plugin | not registered | `crates/sources-plugins/tests/prowlarr.rs` (fixtures `fixtures/prowlarr/*`) |
| #397 | SRC-10a | excluded | Nyaa RSS plugin | HOLD in its decisions file; not registered | `crates/sources-plugins/tests/nyaa.rs` |
| #399 | SRC-14b | inactive | Egress hardening: route delay after EgressDown, unit back-off | sources only (off) | `crates/sources/src/egress_tests.rs`, `crates/sources/tests/contract.rs` |
| #400 | SRC-14b-2 | inactive | Source purge job (tracked, scrubs snapshots) | no source enabled by default, so nothing to purge | `crates/api/tests/ops.rs` (`begin_purge`/`finish_purge` helpers), goldens `ops_source_*.txt` |
| #411 | SRC-10c | excluded | Internet Archive plugin | HOLD (Q2 terms check); not registered | `crates/sources-plugins/tests/archive_org.rs` |
| #413 | SRC-12 | excluded | Tracker full-scrape plugin and parser | not registered | `crates/tracker-scrape/tests/full.rs`, `full_memory.rs`; `crates/sources-plugins/tests/full_scrape.rs::a_private_tracker_is_never_scraped`; fuzz `full_scrape` |
| #414 | SRC-11 | excluded | Download-client lister/exporter (qBittorrent, Transmission, Deluge) | not registered; `DownloadClient::list/export` defaults to `Unsupported`, Send-to-client unchanged | `crates/sources-plugins/tests/clients.rs` |
| #419 | SRC-8b | excluded | RSS bare-infohash element (Academic Torrents) | parser for unregistered plugins | `crates/sources-feeds/tests/rss_bare_hash.rs`, `rss_props.rs` |
| #421 | SRC-12b | excluded | Streamed fetch bodies (`Fetch::fetch_stream`) for full scrapes | only the unregistered full-scrape plugin streams | `crates/sources/tests/fetch_stream.rs`, `crates/sources-plugins/tests/full_scrape.rs` |
| #423 | SRC-10d | excluded | Academic Torrents RSS plugin | not registered | `crates/sources-plugins/tests/academic.rs` |
| #436 | ENR-6 | inactive | Enrichment match counts as a strong adult signal in classify | needs an enrichment match; produced only when TPDB/StashDB are on (`enabled_by_default: false`) | `crates/classify/tests/adult_enrichment.rs` |
| #438 | ENR-3 | excluded | StashDB GraphQL response parser (bounded, fuzzed) | parser reached only by the StashDB plugin, which is on HOLD and not registered in `crates/lodestar/src/services.rs` | `crates/sources-feeds/tests/stashdb.rs`; `fuzz/fuzz_targets/stashdb.rs` |
| #440 | ENR-2b | excluded | ThePornDB response parser (bounded, fuzzed) | parser reached only by the ThePornDB plugin, which is on HOLD and not registered in `crates/lodestar/src/services.rs` | `crates/sources-feeds/tests/tpdb.rs`; `fuzz/fuzz_targets/tpdb.rs` |
| #444 | ENR-0 | inactive | Claim encoding v2 with optional adult-enrichment section (on-disk) | claims are written only by sources, all off; the v2 encoding is one-way for claims a v0.5.2 build writes | `crates/store/tests/unverified.rs`, `unverified_props.rs`; `fuzz/fuzz_targets/claims.rs` |
| #458 | SRC-13 | inactive | Torrent-cache enrichment, hash-checked, never private | `enabled_by_default: false`, External consent: `torrent_cache.rs::manifest_is_external_consent_and_off_by_default` | `crates/pipeline/tests/torrent_cache.rs`; `crates/sources-plugins/tests/torrent_cache.rs` |
| #459 | SRC-11b | inactive | Inbound Sonarr/Radarr webhooks (token auth, size cap, rate limit) | arr source `enabled_by_default: false` and no token until created (`crates/sources-plugins/tests/arr.rs::caps_consent_and_endpoints`); /security-review area | `crates/api/tests/webhooks.rs` (`dropped_calls_say_why_and_unwired_webhooks_read_nothing`, `a_good_token_delivers_and_every_bad_one_gets_the_same_401`) |
| #461 | SRC-10e | excluded | Sukebei variant of Nyaa plugin, Adult forced | not registered in `crates/lodestar/src/services.rs`; manifest `enabled_by_default: false` | `crates/sources-plugins/tests/sukebei.rs::manifest_and_policy` (asserts off by default); `crates/sources-feeds/tests/rss_golden.rs` |
| #464 | SRC-10c-2 | excluded | Internet Archive plugin follow-ups | not registered in `crates/lodestar/src/services.rs` (Internet Archive, HOLD) | `crates/sources-plugins/tests/archive_org.rs::manifest_and_policy`; `crates/sources-feeds/tests/archive_org_skipped.rs` |
| #467 | X-source-adult | inactive | Always-adult sources classified Is-adult on ingest | only Sukebei is always-adult and it's off | `crates/pipeline/tests/source_adult.rs`; `crates/api/tests/source_adult.rs`; `crates/sources-plugins/tests/manifests.rs::the_always_adult_plugins_are_exactly_sukebei` |
| #479 | SRC-13b | inactive | Torrent-cache host wiring: consent per configured host, Queue feeds | torrent_cache off by default; touches writer and `store/src/sources.rs` | `crates/pipeline/tests/torrent_cache.rs`; `crates/store/tests/store.rs` |
| #480 | ENR-1 | excluded | Adult enrichment runtime: eligibility gate, caches, filter before storage, re-classify | runtime not wired: nothing in `crates/lodestar` builds it; `adult_enrichers` defaults to empty (`crates/pipeline/src/lib.rs:164, 211`) | `crates/pipeline/tests/adult_enrich.rs` (`no_ineligible_item_is_ever_handed_to_a_plugin`, `no_request_carries_the_info_hash`, `a_held_swarm_hash_is_never_looked_up`) |
| #490 | SRC-11c | inactive | Sonarr/Radarr webhooks wired into the host; ingest path | arr source off by default + token required; release notes still say "not connected yet" (fix in P-v052-cut). Touches `pipeline/src/dht.rs`, `writer.rs`, `egress.rs` | `crates/lodestar/tests/webhooks_e2e.rs::a_grab_with_the_token_reaches_the_store_private_and_a_wrong_one_never_does`; `crates/api/tests/ops.rs` goldens |
| #497 | ENR-1b | excluded | Enrichment gate, generation race, NoQuery fixes | enrichment runtime not wired (see #480) | `crates/pipeline/tests/adult_enrich.rs` |
| #500 | X-claim-trackers | inactive | One host guard for tracker URLs from claims and caches | claims arrive only from sources (off) | `crates/pipeline/tests/claim_trackers.rs`; `crates/integrations/src/net/tracker.rs` proptests |
| #502 | ENR-1c | excluded | Enrichment polish | enrichment runtime not wired (see #480) | `crates/pipeline/tests/adult_enrich.rs` |
| #503 | ENR-4 | excluded | ThePornDB plugin (Account class) | HOLD: not registered in `crates/lodestar/src/services.rs`; manifest `enabled_by_default: false`, Account class | `crates/sources-plugins/tests/tpdb.rs::manifest_and_policy` (asserts `!enabled_by_default`) |
| #504 | ENR-7 | inactive | Detail API adult-enrichment section, only with Show adult content on | no data unless enrichment on | `crates/api/tests/adult_enrichment.rs` |
| #506 | ENR-5 | excluded | StashDB plugin (Account class) | HOLD: not registered in `crates/lodestar/src/services.rs`; manifest `enabled_by_default: false`, Account class | `crates/sources-plugins/tests/stashdb.rs::manifest_and_policy` |
| #508 | ENR-8 | inactive | Detail drawer adult-enrichment section | renders only when API sends the section (enrichment off) | `web/src/lib/features/detail/adult/AdultEnrichment.test.ts` |
| #511 | X-enr-page-flag | excluded | Enrichment parsers report what they saw (page flag) | parsers of the HOLD enrichment plugins; not a UI or page flag | `crates/sources-feeds/tests/stashdb.rs`, `tpdb.rs`; `crates/sources-plugins/tests/stashdb.rs`, `tpdb.rs` |
| #512 | X-claim-store | inactive | Claimed trackers stored canonical and guarded | claims only from sources (off) | `crates/pipeline/tests/claim_trackers.rs`; `fuzz/fuzz_targets/verified_claims.rs` |

### 3.3 Gaps: features with no test that proves they stay off
P-v052-cut should close these before the freeze (decisions file, D4). Each one is a small test, not a behaviour change:
- **G1:** the binary registers exactly the DHT, the torrent cache and the Sonarr/Radarr webhooks. One assert over the whole `/api/v1/sources` list in `crates/lodestar/tests/webhooks_e2e.rs`, which runs the real binary, closes it.
- **G2:** the binary wires no adult-enrichment source (`adult_enrichers` is empty).
- **G3:** `PipelineConfig::default().verify_unverified` is `false`, and no config or settings key sets it.
- **G4:** the release tarballs and the Windows binary leave out the `fixture` feature. The Docker image is already checked (`.github/workflows/release.yml:80-85`, "Image has no fixture backend"); the tarballs and the Windows build aren't. `packaging/windows/build-msi.ps1:6`'s manual build example uses default features, which include `fixture`, so it should pass `--no-default-features --features web` too.

### 3.4 A fix slice the cut needs: X-consent-gate-052 (runner's call)
On `main`, a source that needs consent can be accepted and turned on through `PATCH /sources/{id}`, with consent wording nobody sees in the UI and the owner hasn't approved. That isn't real consent, so v0.5.2 mustn't allow it (runner, 2026-11-06).
- **The slice:** `X-consent-gate-052` (Opus, `/security-review`). Until SRC-15b ships owner-approved consent text, `PATCH /sources/{id}` refuses to enable any source whose `consent::need` isn't None, with a clear error. It adds a test against the real binary.
- **Effect in v0.5.2:** the torrent cache can't be turned on. The Sonarr/Radarr webhooks need no consent, so they stay API-enable-able and opt-in.
- **Order:** it lands on `main` before P-v052-cut. The runner adds it to `runner/slices.yaml`; this slice doesn't edit that file.

## 4. Merges with no user-visible behaviour
Docs, plans, runner records, CI, flake and test fixes, tooling, benches and production-neutral refactors. Two of them still matter to the gate:
- #209 CR-SIM changes production socket code, and counts as a weak endpoint trigger (§6).
- #437 RP-1 runs the pack corpora in CI.

#207 CR-COL-2 (Soak analyzer names the run kind); #209 CR-SIM (Deterministic crawler replay harness); #212 (owner) freshness hardening (Harden the freshness re-sign CI job); #213 X-flake-logdelete (Log-delete test flake fix); #214 (runner) CR-0 acceptance (Records CR-0 acceptance and memory gate); #215 (runner) memory gate analyzer (Analyzer computes v0.5.1 memory gate); #218 (MSRV clippy 1) (Clippy fixes, semantically equivalent); #224 (MSRV clippy store/filter) (Clippy fixes); #226 (filter-budget gate) (Per-model skew in filter-budget calibration); #229 X-flake-supervisor-switch (Supervisor test reserves ports); #234 fix/cli-test-port (CLI tests get unhandable ports); #235 fix/canary-udp-port (Log canary DHT port); #237 fix/clippy-msrv-2b (Finish MSRV clippy cleanup); #238 fix/hash-mismatch-docs (Docs tidy for #236); #239 X-crawler-sim-crate (Simulator moves to its own crate); #242 CR-SIM-restart (Simulator scenario for restored restrictions); #244 (runner) cr-sim-tidy (CR-SIM tidy, pinned cooldowns); #255 (runner) insights DTO rename (OpenAPI DTO rename); #260 (runner) owner delegation (Records owner delegation); #261 (runner) alert limit record (Corrects a size record); #268 (glossary) (Crawled/Imported in UI glossary); #292 X-soak-docker-memrule (Docker stop rule judges cgroup anon); #295 X-form-remount (Focus/resync regression spec); #296 X-build-stable-name (Deterministic web build names); #311 X-analyzer-memview (Corpus-normalised memory view); #314 S-e2e (Closes §10.17 Playwright gaps); #320 S-e2e-2 (Snapshot every illustrated state); #323 X-e2e-fixture-ci (Nightly fixture specs against real server); #329 X-flake-security-test (Preload dialog chunk in test); #340 G-m5b (M5 design-review report); #344 G-m5a (Analytics-validation matrix test and reporting-overhead bench); #349 X-web-cleanup-1 (Token rename `text-label` → `text-small`; no visible change intended); #351 X-file-clause-cold (gate) (Bench reports the cold 4-at-once broad file clause); #352 G-m5 (Records the M5 exit); #354 S-doc-shots (Docs screenshot capture (`npm run docs:shots`)); #355 X-pw-local-chromium (`PW_CHROMIUM_PATH` local Playwright override); #356 P-v06-sources-plan (Slices the v0.6 Sources epic); #360 SRC-0 (Source plugin contract v2 (policy, caps, floors)); #364 X-size-deterministic (Bundle sizes measured with a fixed build version); #368 openapi-example-size (OpenAPI example fixed (`size:>=10GB`)); #369 P-master-plan-r2 (Master plan refresh, handoff guide); #371 (runner) owner-delegation-2 (Records owner's delegation of pending questions); #372 P-master-plan-r3 (Master plan r3); #374 status-connector (Roadmap dashboard connector (Cloudflare worker)); #375 CR-SPEC (Frozen crawler measurement spec); #376 status-connector-r2 (Dashboard connector r2, steward skill, CI deploy); #378 CR-COL-3 (Collector/analyzer keep and judge gate evidence); #384 X-required-web-checks (Always-reporting aggregate web/e2e CI checks); #396 G-import-smoke (Import pilot smoke (magnetico/bitmagnet) evidence); #398 (runner) build fix (Sets `max_agents` on Nyaa/Prowlarr policies (compile fix)); #401 (runner) slots-and-pause (Runner slots and pause record); #403 (runner) soak-r3-verdict (Records soak r3's verdict and rc.3 plan); #404 (runner) soak-r3 memory plan (Owner-approved memory-fix plan); #405 CR-COL-4 (Collector tracks search commits and mmap cache gauge); #409 (runner) plan update (Plan/dashboard update); #410 (runner) plan update (Plan/handoff update); #412 P-v06-rulepacks (Rule-pack plan for v0.6); #416 dashboard-slots (Dashboard slot tracking); #417 (runner) plan update (Plan update); #422 ENR-2a (Shared JSON reader factored out of the IA parser); #425 soak-memory-review (Code review records (soak memory, master plan)); #427 (runner) plan update (Plan update); #428 (runner) plan update (Owner decision record); #430 (runner) plan update (Plan/handoff update); #431 dashboard-slots-r2 (Roadmap dashboard shows over-limit slots as finishing (tooling)); #435 (runner) owner §2.37 (Records owner answers); #437 RP-1 (Pack corpora and precision corpus run in CI); #439 fix json_props lone surrogate (Test fix); #441 X-merge-policy (Merge-policy A/B harness, negative result, no change shipped); #443 fix/ci-test-disk-space (CI frees disk); #445 (runner) plan entries (adds slice and placement entries to `runner/*.yaml`); #446 X-dashboard-adapts (Dashboard tooling); #449 (runner) soak r4 verdict, §2.40 (Records soak r4 fail and rc.4 scope); #451 X-filter-budget-noise (Filter perf budget decided on third-fastest pass); #452 X-dashboard-idle-slots (Dashboard tooling); #453 (runner) owner §2.41 (records the owner's slot rule and its `runner/*.yaml` entries); #456 X-mimalloc-docs (Memstats attribute "other heap" by build; tighter malloc check); #466 X-flake-boot-shell (e2e flake fix); #471 (runner) plan entries (slice and dashboard entries in `runner/*.yaml`); #473 X-followups-1105 (Review follow-ups (log events, facet tests) for SRC-17a/RP-2); #475 D-v052-notes (Release-notes draft); #476 X-flakes-1105 (Test flake fixes (restart_wire deadline, magnetico)); #477 X-flake-search-keyboard (e2e flake fix (focus waits)); #481 X-test-hygiene-1105 (DHT test ports off the ephemeral range; magnetico checkpoint advice in docs; also rewords one CLI refusal message); #482 X-ci-disk (CI disk); #484 X-fuzz-1105 (Fuzz campaign seeds for this week's parsers); #489 X-flake-subs-stall (Flake fixes (stalled-download hang, uTP Windows ports)); #491 X-fuzz-cache-1105 (Fuzz target for torrent-cache parse and `Verified::claims`); #496 X-settings-trim (Settings chunk back under §10.1 cap); #498 X-flake-update-log (Update test log isolation); #501 X-settings-followups (Copy-link test, record fixes).

## 5. One-way migrations and format bumps

### 5.1 What a v0.5.1 data folder goes through

| Change | From → to | PR | One-way? | What v0.5.1 does with the result |
|---|---|---|---|---|
| Store migration V13: imports (`import_sources`, job table, cursors, candidates, quality, delete triggers) | V12 → V13 | #210 IMP-1 | yes | refuses to open it (`SchemaTooNew`) and changes nothing |
| Store migration V14: sources (`unverified_items`, `torrent_claims`, `source_state`, three delete triggers) | V13 → V14 | #379 SRC-1 | yes | same |
| Fresh-store marker `packs.fresh` (a `settings` row, not a migration) | written only when a store is created | #454 RP-DEF | n/a | an upgraded store never gets it |
| Search `SCHEMA_VERSION` | 5 → 7, one blue/green rebuild (v6 SB-6 #415/#418, v7 X-search-files-5 #505) | #415, #418, #505 | yes: the v5 index is deleted after the swap | can't read v7; after `lodestar rollback` it rebuilds v5 from the restored store |
| `CLASSIFY_VERSION` | 3 → 4: stored rows re-derived in the background, resumable, no migration | #469 RP-UC | no: the snapshot holds the v3 rows | n/a |
| Legal Notice `NOTICE_VERSION` (`crates/api/src/legal.rs:23`) | 2 → 3, stored in `auth.json` | #472 LEGAL-1 | `auth.json` isn't in the store snapshot | reads 3 as accepted and skips the notice: release/v0.5.1 `crates/api/src/auth.rs:266` asks only when the stored version is lower than its own (2), and `AuthFile` is unchanged (Q3) |
| Claims blob v2, pack overrides `FORMAT` "1", import contract v1, DHT restart proof v1 | new | #444, #465, #210, #208/#216 | written only by v0.5.2 | inside V13/V14 tables or `settings` rows, which the snapshot predates. The restart-proof files sit in the data folder; whether v0.5.1 ignores them is to confirm in the rollback check |

**The snapshot.** `migrate_creating` takes one snapshot before the first pending migration and names it after that version (`crates/store/src/schema.rs:1059-1063`, `take_snapshot` at `:1092`). A V12 store upgraded straight to V14 therefore writes one file, `<db>.pre-v13.db`. A repeat snapshot is named `pre-v13.<k>.db`. Snapshots are kept 90 days (`crates/store/src/snapshots.rs:57`).

**What's tested today:**
- `schema.rs::older_builds_refuse_a_newer_store_untouched` runs v0.5.1's code path (`MIGRATIONS[..12]`), not the v0.5.1 binary.
- `v13_reads_existing_imports_as_unknown_and_schedules_nothing`, `v14_upgrades_a_v13_store_with_data` and `v14_on_a_fresh_store_matches_an_upgraded_one`.
- The rebuild: `crates/search/tests/rebuild.rs::a_v5_index_serves_while_v7_is_built_resumed_and_swapped_in` and its neighbours (interrupted, stopped, failed, entries logged meanwhile).
- `lodestar rollback` (`crates/lodestar/src/cli/rollback.rs` tests): it never brings back a deleted torrent, keeps holds recorded since, survives an interruption, and picks by name or by `v<N>`.

**What isn't tested:**
- No V12 fixture is committed.
- No test upgrades a real v0.5.1 data folder.
- No test runs the v0.5.1 binary on a rolled-back store.

The gate's two checks below are new work for P-v052-cut.

### 5.2 The V12 → V14 upgrade smoke (P-v052-cut)
1. Take a copy of a v0.5.1 data folder that has run the rc.4 build, ideally the r5 soak's volume or a ≥ 1M-torrent folder. Record its store's schema version, `SELECT max(version) FROM schema_migrations` (12), the search index directory (`v5-*`), the torrent count and a search's result counts.
2. Start the v0.5.2 rc on the copy. Check, in order:
   - a. `<db>.pre-v13.db` exists before V13 runs, and its schema version (`max(version)` in `schema_migrations`) is 12;
   - b. the store's schema version reaches 14;
   - c. the old index serves read-only while v7 builds (new fields reported missing);
   - d. `search.swapped` is logged, with `disk_high_water_bytes`;
   - e. re-classification finishes, `CLASSIFY_VERSION` 4;
   - f. pack state matches step 1, every pack without a row reads off, and `unauthorised-copy` is off;
   - g. after login the Legal Notice v3 shows once (rc.4 accepted v2);
   - h. to give §5.3 something to test, delete one torrent and record one hold, and note both.
3. Pass: every check in step 2 holds, and the counts from step 1 match. The only allowed change is torrents that `CLASSIFY_VERSION` 3 → 4 re-classifies as adult (`reclassify.rs` re-judges only those; RP-UC is off on an upgraded store and MW-6 is shadow-only). Any other change in the counts is a failure.
4. Run it once on Docker and once on **Windows** with a real v0.5.1 data folder (runner's call). The Windows leg covers V14, the pre-v13 snapshot, deleting the old index on NTFS and `lodestar rollback` on Windows. The MSI check in `release.yml` runs the same binary twice, so it doesn't count.

### 5.3 The rollback-to-snapshot check (P-v052-cut)
1. Stop the v0.5.2 server on the upgraded copy.
2. Run `lodestar rollback v13 --yes` with the v0.5.2 binary. Check:
   - the live store is copied to `lodestar.before-rollback-<time>.db`;
   - the index is moved aside to `index.before-rollback-<time>`;
   - the restored store's schema version is 12.
3. Start the **v0.5.1 rc.4 binary** on the folder. It must start, rebuild its v5 index and serve search. Check:
   - it skips the notice, as the code predicts (Q3);
   - the DHT restart-proof files and V14-era files left in the folder don't stop it.
4. Pass: v0.5.1 serves the torrents counted in §5.2 step 1, less any deleted during the v0.5.2 run. Data crawled during the v0.5.2 run is lost, as `rollback` documents. The torrent deleted and the hold recorded in §5.2 step 2h are still in effect.
5. Why this is new evidence: the existing rollback tests use a V14 store named `pre-v13`, not a real V12 one, and none starts the v0.5.1 binary.

## 6. Delta-rule re-soak triggers

The delta rule (build-plan §0.4) adds a re-soak for changes to crawler lifecycle, the sampler or endpoint policy. Here is what the diff `c3f6341a..63a2c6df` holds, apart from what rc.4 already carries:

| Trigger | PR | Active in the default build? | Why it counts |
|---|---|---|---|
| **Crawler lifecycle:** restart proof on the off path | #208, #216 CR-0d | **yes**, even with `dht.safe_restart = false` | Every run takes `dht-restart.lock` for its whole life and overwrites a leftover marker as `off` before the first sample (`crates/dht/src/node/proof.rs:725-760`; `actor.rs:657-664`) |
| **Crawler lifecycle:** setup hold | #499 LEGAL-2 | **yes**, on a fresh data folder | The DHT (no bind, bootstrap, answer or announce), scrape and fetch wait for `POST /setup`. A soak's clock starts after setup |
| **Crawler lifecycle:** fetch-pool lookup generations | #223 | **yes** | A stale timer can no longer end a re-found key's lookup |
| **Crawler lifecycle:** start-time refetch | #267, #274, #297 | yes, but only with incomplete imports (a v0.5.1 store has none) | Refetches run at start and are kept out of the funnel |
| **Endpoint and transport code:** socket seam | #209 CR-SIM (+#239, #242, #244) | yes, production-neutral (`docs/decisions/2026-10-30-CR-SIM.md`) | The DHT's send and receive path goes through an enum, with UDP its only production variant. A weak trigger |
| **Endpoint policy:** uTP, route back-off | #365 CR-4 | no while off. Per-family attempt counters and `routes.unreachable` counting do run (`crates/peer-wire/src/fetch.rs`) | Stats only while off |
| **Sampler:** prefix targets | #359 CR-1b | no (only `offer`'s signature changes) | n/a |
| Admission, hints, streaming | #228, #231, #246, #251, #252 | no | n/a |
| **Fetch metrics:** hash mismatch split out, queued drops counted | #236, #232 | yes, metrics and log fields only | The analyzer and the collector must know the series (CR-COL-3) |
| **Writer and memory:** full v5 → v7 rebuild | #415, #505 | yes, on upgrade | About 1 h 55 min at 10M on 4 cores, and about 2× the index's disk while it runs |
| **Writer and memory:** re-classification | #469 | yes, on upgrade | Every stored row is re-derived in the background |

No change touches `crates/network-policy`. No allocator change follows #448, which rc.4 carries. The `rust-version` bump 1.85 → 1.89 (#208) is the minimum supported version only. Release builds still compile with `rust:1.97-bookworm` (`packaging/docker/Dockerfile:28`, unchanged since the cut), so it isn't a trigger.

**What this means for the gate (recommendation, Q5):** the delta rule is met, so v0.5.2 needs its own soaks, not rc.4's evidence:
- the 24 h Linux Docker soak as the binding memory gate, on an **upgraded** v0.5.1 folder, so that it runs V14, the rebuild and the re-classification;
- the ≥ 6 h Windows service run, on a **fresh** folder, so that it runs LEGAL-2's hold and packs on;
- the Windows upgrade and rollback leg in §5.2 step 4 (runner's call).

**Before each clock starts:**
- finish setup (LEGAL-2);
- on the upgraded folder, accept Legal Notice v3. Until then every session route answers `409 notice_required`, which `docs/soak/tools/linux/collect.py` doesn't handle. Recommendation: the collector fails loudly on a 409 rather than recording gaps;
- on the upgraded Docker folder, turn every pack on after the upgrade (runner's call), and record the pack state in the run record.

The SP-3 flood map (#495) isn't a reason for the fresh leg: it stays empty while every source is off.

## 7. Release-note corrections found
For P-v052-cut, which finishes `docs/releases/v0.5.2.md`:
- **Pending markers that have landed:**
  - RP-UC (#469), SRC-17 (#474), LEGAL-1 (#472), #468, X-log-url-redact-2 (#460) and the classifier bump 3 → 4 (#469) are merged.
  - The supervisor check for in-app updates is still the cut's own work.
- **"Pack-rule overrides: API only; the screen comes later"** is out of date: RP-6 (#487) added Filters & Safety → Packs with the override editor.
- **"Sonarr and Radarr webhooks are built but not connected yet"** stays (runner's call). SRC-11c (#490) wired them into the host, but they have no screen: they're off by default and can be turned on through the API only, with a token. Add "advanced: API only" to the note.
- **"Internet Archive, Sukebei and torrent-cache sources are built but stay off until their checks pass"** mixes two classes:
  - Internet Archive and Sukebei aren't registered (excluded);
  - the torrent cache is registered, but X-consent-gate-052 stops it being turned on until SRC-15b (§3.4).

  Keep the current wording, "built but … stay off … come later" (runner's call).
- **Not in the draft:** tracker scraping is on by default (`scrape.enabled = true`, as in v0.5.1); say so plainly.
- **Not in the draft:** the setup hold (LEGAL-2): on a new install, Lodestar doesn't contact the network until setup is done.

## 8. Open questions

| # | For | Question | Recommendation |
|---|---|---|---|
| Q1 | runner | The freeze point: this manifest is at `63a2c6df`, and the cut is later | Cut from `main` at the v0.5.1 tag. P-v052-cut classifies every later merge in its delta report, and anything default-on, a migration or a re-soak trigger gets an addendum here before the cut (D2). Hold new migrations (V15 is reserved for CR-3) until the branch exists |
| Q2 | runner | Close G1–G4 (§3.3) before the cut? | Yes: one small Opus test-only slice, or part of P-v052-cut. G1 matters most, because a plugin added to `services.rs` would otherwise reach the release unnoticed |
| Q3 | runner | Does v0.5.1 accept an `auth.json` notice acknowledgement of version 3 after rollback? | **Answered by the code:** yes. release/v0.5.1 `crates/api/src/auth.rs:266` asks for the notice only when the stored version is lower than its own (2), and `AuthFile` has the same fields on both branches, so v0.5.1 starts and skips the notice. §5.3 step 3 confirms it on the real binary |
| Q4 | runner | Torrent cache and Sonarr/Radarr webhooks in v0.5.2 | **Decided by the runner:** no source that needs consent can be turned on in v0.5.2 (X-consent-gate-052, §3.4), so the torrent cache stays off. The webhooks need no consent and stay opt-in, advanced, API only. The release notes keep "built but not connected yet / come later" and add "advanced: API only" for the webhooks |
| Q5 | runner | Soak plan under the delta rule | §6: 24 h Docker soak on an upgraded folder (binding), every pack turned on after the upgrade; ≥ 6 h Windows run on a fresh folder; a Windows upgrade and rollback leg (§5.2 step 4). Each clock starts after setup, and on an upgraded folder after Legal Notice v3 is accepted. The collector should fail loudly on `409 notice_required`. The rebuild runs in the first hours, mostly before the memory window (hours 12–24); report its disk high-water mark |
| Q6 | owner | None blocks the manifest | Packs on for new installs, the unauthorised-copy pack and the v0.5.2 placement are owner decisions already (§2.39, §2.40). Crawler switches staying off is build-plan §0.4. Nothing here reverses or loosens an owner decision |
