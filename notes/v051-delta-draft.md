# v0.5.1 delta draft: soaked build 744b84d6 to origin/main

Draft for docs/decisions/2026-10-30-master-plan.md Part D2. Read-only analysis; no repo files changed.

## Basis

- Soaked build: `744b84d68c7609d8220430ffc9938c05c6d6a527` (CR-0 frozen build).
- Cutoff candidate: `origin/main` = `1199430f343862b19dce59f155589acd4c5e4120` (after `git fetch -q origin main`).
- `git merge-base --is-ancestor 744b84d6 origin/main` succeeded: 744b84d6 is an ancestor.
- Range `744b84d6..origin/main`: 99 commits, 22 first-parent merge commits, 358 files changed repo-wide (+36152 / -558, mostly docs, tests, goldens, fuzz seeds).
- Scoped paths (dht, sources, pipeline, peer-wire, network-policy, store, search, lodestar/src, filter, metainfo, api): 73 files, +4682 / -360, of which 31 are non-test source files (listed below). Net line counts are from `git diff 744b84d6..origin/main`; they include inline `#[cfg(test)]` code where noted.
- Not yet in main, added at the cut: #198 F-updater, #200 CR-0f, #196 F-filters-xcpu, #199 CR-gate-metrics. This draft does not cover them.
- PR numbers come from the first-parent merge that brought each commit in (`git log --first-parent`, `git rev-list M^1..M^2`). Landing PRs (#180 land/L2, #184 land/L5) carry commits written in other slices.
- Commit-to-file attribution uses `git log --no-merges -- <file>` and `git show --numstat`; the per-commit line counts below are from `--numstat`.

## Paths with NO source change in the range

- `crates/sources`: no change to src. One test-comment change (`crates/sources/tests/pool.rs`, +3, dd515f8d, #188 port/X-sleeps).
- `crates/network-policy`: no change at all.
- `crates/pipeline`: no change to src. `Cargo.toml` only: unused `tracing` dependency removed (F5).
- `crates/dht`: no production-code change. Only a `mod tests` edit in `node/actor.rs` and log-catalogue text (A1, A2).
- Out of the requested paths, noted for completeness: `crates/log/src/url.rs` (inline scrub-linearity test only, 24e7cf59), `crates/integrations/tests/*`, `crates/lodestar/tests/{log_canary,shutdown,windows_service}.rs`.

## Summary

### Counts per class (20 changes, each one class)

| Class | Count | Changes |
|---|---|---|
| A crawler lifecycle / sampler / endpoint policy | 2 | A1, A2 (both trivial) |
| B store / schema / migrations | 3 | B1, B2, B3 |
| C search / index | 3 | C1, C2, C3 |
| D filters / metainfo parsing | 4 | D1, D2, D3, D4 |
| E API / auth / updater | 3 | E1, E2, E3 |
| F other | 5 | F1, F2, F3, F4, F5 |

### Re-soak list (class A, non-trivial)

None. Both class A changes are trivial:

- A1 `dht/src/node/actor.rs`: every changed line is inside `mod tests` (two tests now return early when `[::1]` can't be bound). No production code changed.
- A2 `dht/src/log_events.rs`: doc comment and the message string of `dht.harvest_summary`. The code that writes the summary (`node/actor.rs`, `harvest_summary` writer near line 662) is unchanged.

No change touches the DHT bootstrap, sample suppression, BEP 51 sampling, pause/resume, `crates/network-policy` or `crates/sources` pool admission.

Items a reviewer may still want to weigh against the "crawler lifecycle" wording (not class A by the definition given, so listed here, not as re-soak items):

- F1 `peer-wire/src/fetch.rs` runs on every metadata fetch. The change moves failure counting to one place (`counted()`); the returned `Result` is untouched (metrics only).
- F3 `lodestar/src/services.rs` is the app shutdown bound for the HTTP server (`bounded()`), moved out of an inline `select!` and left unchanged in behaviour. It is not the crawler's pause/resume.
- B1/B2 change the live store: V12 migration SQL edited in place plus a one-time repair run at open; a delete trigger on `torrents` now writes a timestamped row. See below. The soak data directory is one of the stores the repair targets (467a10c4 message).
- D1 changes how `Info::parse` reads fetched metadata, which `pipeline/src/worker.rs:203` calls on every fetched torrent. D2-D4 change what the content filter decides in `pipeline/src/worker.rs:236` and `writer.rs:319,575` (`judge_classified`). Neither is crawler lifecycle; both change which torrents are stored or how they read, so a soak on the cutoff artifact would exercise them.

### NO TARGETED TEST FOUND

1. B2 `store/src/snapshots.rs` (d4a7aac4): the branch where `std::fs::metadata` on a snapshot fails with an error other than NotFound (warning "reading the snapshot ... failed", `blocked = true`, snapshot skipped), at about line 515. `git grep 'reading the snapshot'` finds only the source line.
2. E1 `api/src/auth.rs` (a2c15c9e): the new `if state.password.is_some() { return Err(AuthError::SetupDone) }` in `setup`. The commit message says it is defensive, unreachable today. No test reaches it.
3. A2 `dht/src/log_events.rs` (8ae7def2): the new `dht.harvest_summary` message string. The golden `logs_catalog.txt` carries ids, levels and label keys, not message text. No test checks the text.
4. F2 `peer-wire/src/log_events.rs` (8ae7def2): `#[repr(align(128))]` on `Summary` (layout only). Existing tests in the file do not assert layout.
5. F5 `pipeline/Cargo.toml`, `search/Cargo.toml`: removal of the `tracing` dependency. Only a compile check applies. `git grep tracing origin/main -- crates/pipeline crates/search` returns nothing.
6. B3 `store/src/insights_days.rs` (d4a7aac4, dbd79bb0): the whole change is an `#[ignore]`d bench (`trigger_cost`). It is test code, with nothing to test.

Test-only changes (A1, B3, C2 seams, E2, F4) count as their own tests and are not listed above except B3.

## Commit and PR key

| PR | Merge | Commits touching scoped paths |
|---|---|---|
| #177 slice/F-metainfo | a84c3681 | e0fc29ae Fix metainfo parsing of BEP 47 padding, symlinks and path.utf-8; bd131a8d Check the encoding guess against path.utf-8 pairs; 207866e8 Re-check path limits after resolving encodings; c95f7ef5 Keep golden and seed files byte-exact on Windows (tests only) |
| #178 slice/F-auth | 0537889f | fe2a6fdb Keep auth state unchanged when saving auth.json fails; 585dfca4 Never reuse the auth temp file; test cleanup after a failed rename |
| #179 slice/F-filters | f898813b | fa0c3956 Fix three content-filter semantics from the Area 3 review; 8c5a0b91 Neutralize by token, so joined words can't borrow the word; 2b3d6dab Keep neutralizing from widening Always allow; bd8d2a43 Key neutralized words by their parts, and let phrases use whole forms; 03c6d164 Hold the floor against every release, and record the runner's calls (tests only) |
| #180 land/L2 | c6aa1925 | 213548e2 Skip L2's dual-stack actor tests on hosts without IPv6 |
| #181 fix/ci-health | 42779e4a | 032799cd Guard phrase seek_danger so negated phrases don't panic; d2d94d55 Keep empty phrase scorers prunable and unit-test the guard |
| #184 land/L5 | 5e352be2 | dbd79bb0 Bound the V12 snapshot deletion journal and test it; d4a7aac4 Redate future snapshots and skip unstat-able ones; 467a10c4 Repair stores left at V12's pre-release journal shape; 66152cee Fix the v4 migration fixture and an MSRV lint in landing range L4 (net zero on src: `search/src/files.rs` and `store/tests/swarm.rs` are unchanged in the net diff) |
| #187 port/B-log1b-4b | ae1f8b32 | 8ae7def2 Add the canary's hash control and the runner's part 4 follow-ups; ff535a2e Wait for the canary's hash switches instead of a fixed time |
| #188 port/X-sleeps | fcc1d378 | dd515f8d Replace fixed sleeps in log_canary with settle waits (tests only) |
| #190 slice/F-auth-lock | 476689c9 | f2b9f873 Keep auth saves from stalling every request; e3d768bd Tighten lock-test isolation and decision wording; a2c15c9e Refuse setup if a password is already set |
| #191 fix/shutdown-stall | fafe2f89 | a16542dd Hold the shutdown bound every run and test the forced path directly |
| #192 slice/F-filters-neutral-perf | 8d7a24f6 | 8bc2373a Fit Neutralize terms in the 50 µs filter budget; d9ef8fa8 Keep the neutralized budget case reported until CPUs agree (tests only); c9c9a7ee Cover pathless files in the paths test; fix NFKD wording (tests only) |
| #193 fix/flake-shutdown-percolator | 18c1f324 | 4625e807 Time the percolator test from the commit, not the store |

Decision files for the above are in `docs/decisions/` (2026-10-24-B-log1b-r6, 2026-10-30-F-auth, F-auth-lock, F-filters, F-filters-neutral-perf, F-metainfo, L3-L5-landing, X-ci-health, B-log1b-shutdown-stall, X-flake-shutdown-percolator; 2026-10-26-soak-retention and soak-import-upgrade modified).

## Class A: crawler lifecycle / sampler / endpoint policy

### A1 `crates/dht/src/node/actor.rs` (+8 / -2), trivial

- Commit: 213548e2 (#180 land/L2).
- Change: in `mod tests`, `StdUdpSocket::bind("[::1]:0").unwrap()` becomes `let Ok(v6) = ... else { return; }` in two tests, so they skip on hosts with no IPv6.
- Class A trivial: test code only, no production line changed. The hunks sit under `mod tests`.
- Targeted tests: the two tests themselves, `dual_stack_completion_and_deadline_drain_both_family_orders` and `dual_stack_cancellation_cleans_shared_rpc_ownership` (same file, `mod tests`).

### A2 `crates/dht/src/log_events.rs` (+4 / -3), trivial

- Commit: 8ae7def2 (#187 port/B-log1b-4b).
- Change: `HARVEST_SUMMARY` doc comment and message go from "DHT harvest in the last minute" to "DHT harvest since the last summary". Id, level, component and fields are unchanged.
- Class A trivial: log text only; the summary writer in `node/actor.rs` is untouched.
- Targeted tests: NO TARGETED TEST FOUND for the message text. The log catalogue is checked by `crates/lodestar/src/settings/logging_tests.rs` ("logs_catalog" golden, line 348), which does not include message text; the golden's `dht.harvest_summary` entry (id, level, label key) is unchanged.

## Class B: store / schema / migrations

### B1 `crates/store/src/schema.rs` (+76 / -10)

- Commits: dbd79bb0 (+29 / -9) and 467a10c4 (+47 / -1), both #184 land/L5.
- Change (dbd79bb0): migration V12 (`snapshot_deletions`) is edited in place. Rows now carry `at INTEGER NOT NULL`. The backfill inserts the later of the quarantine/tombstone times and skips hashes that are live again (v1 hash or v2 alias). The `snapshot_torrent_deleted` trigger on `torrents` now upserts `(hash, at)` with `unixepoch()`, for the v1 hash and the truncated v2 hash.
- Change (467a10c4): adds `V12_REPAIR` and `repair_v12`, called from `migrate()` after `migrate_with`. If `snapshot_deletions` has `hash` but no `at`, one write transaction adds the column, dates rows now, drops rows for live hashes, and replaces the trigger. This is for stores migrated by builds from a7c6d5c8 until the fix. a7c6d5c8 is an ancestor of 744b84d6, so the soaked build ran that shape.
- Class B. Hot-path note: the trigger runs on every `DELETE FROM torrents`, and now does an upsert instead of `INSERT OR IGNORE`. The `trigger_cost` bench (ignored test) gained a "both triggers" arm; the numbers are recorded in `docs/decisions/2026-10-26-soak-retention.md`, not re-measured here.
- Targeted tests (all in `crates/store/src/snapshots.rs` `mod tests`):
  - `migrating_v11_journals_holds_at_their_time_except_live_hashes` (V12 backfill and live-hash skip)
  - `a_hybrid_delete_journals_both_hashes_and_a_repeat_moves_the_time` (trigger)
  - `opening_a_pre_release_v12_store_repairs_its_journal` (V12_REPAIR and `repair_v12`, including the live-hash row and trigger replacement)
  - `crates/store/tests/swarm.rs` (the v4 migration fixture, 66152cee, net zero)
  - The existing migration tests that run `migrate()` on every `Store::open`.

### B2 `crates/store/src/snapshots.rs` (+445 / -46, includes about 300 inline test lines)

- Commits: dbd79bb0 (+328 / -43), d4a7aac4 (+52 / -13), 467a10c4 (+75 / -0, the `repair_v12` test), all #184.
- Change (dbd79bb0): `Live.deleted` becomes `(hash, at)`; `purge_snapshots` drops journal rows older than `JOURNAL_SECS` (91 days) on every pass; a deleted hash matches only snapshots taken up to a day after its `at`; acknowledgement deletes per `(hash, at)` and uses `begin_write`; an unknown-schema snapshot no longer blocks acknowledgement; a snapshot with no readable mtime is deleted with a warning (`blocked` flag replaces `warnings.is_empty()`).
- Change (d4a7aac4): a snapshot whose mtime is ahead of the clock is redated to now (`set_modified`) and fully looked up that pass; a snapshot whose `metadata()` fails (other than NotFound) is skipped with a warning and blocks acknowledgement.
- Class B. Runs in the store's purge pass, not in the crawler loop.
- Targeted tests (same file, `mod tests`):
  - `with_no_snapshots_left_the_journal_clears_only_after_a_clean_pass`
  - `an_unknown_schema_neither_holds_the_journal_nor_takes_a_mark`
  - `a_deletion_matches_only_snapshots_taken_up_to_a_day_after_it` (also holds the d4a7aac4 future-dated redate case)
  - `rows_older_than_the_window_go_on_every_pass`
  - `a_snapshot_with_an_unreadable_time_is_deleted`
  - the existing purge tests, adjusted to `stamp()` journal rows (`journal`, `stamp`, `journal_row` helpers)
  - NO TARGETED TEST FOUND: the `metadata()` failure branch (about line 515).

### B3 `crates/store/src/insights_days.rs` (+29 / -13), test only

- Commits: dbd79bb0 (+21 / -14), d4a7aac4 (+16 / -7), #184.
- Change: the `#[ignore]`d `trigger_cost` bench takes arms (no trigger, Insights' trigger, both) and interleaves runs. No production line.
- Targeted tests: it is the test (`trigger_cost`, `--ignored`).

## Class C: search / index

### C1 `crates/search/src/seek_guard.rs` (new, +245), `compile.rs` (+4 / -2), `lib.rs` (+1)

- Commits: 032799cd (+96 new file, compile.rs +4/-2, lib.rs +1) and d2d94d55 (seek_guard.rs +159 / -10), both #181 fix/ci-health.
- Change: `parts_query` wraps every positional `PhraseQuery` in `SeekGuard`. The guard's scorer answers `seek_danger` with a target behind its current doc as the default implementation does (`SeekLowerBound`), because tantivy 0.27's `PhraseScorer::seek_danger` debug-asserts `target >= doc()`. An `EmptyScorer` passes through unwrapped so `BooleanWeight` can still prune it.
- Class C. This is in the live query path: every phrase query from `compile.rs` (lines 509, 526, 764 call sites via `parts_query`) and `files.rs` (lines 309, 324). Per the file's own header, release builds already answered correctly; the guard makes debug and release take the same path.
- Targeted tests:
  - `crates/search/src/seek_guard.rs` `mod tests`: `a_target_behind_the_phrase_is_a_lower_bound`, `a_phrase_that_never_occurs_is_terminated_from_the_start`, `a_missing_term_stays_an_empty_scorer`, `seek_danger_agrees_with_the_matches`, `an_excluded_phrase_removes_exactly_its_matches`
  - `crates/search/tests/suggest.rs`: `a_negated_phrase_suggests_without_panicking`, `a_negated_phrase_excludes_its_matches`
  - seeds: `crates/search/tests/suggest.proptest-regressions` (+1 case)
  - existing phrase and query tests in `crates/search/tests/query.rs` and `index.rs` go through the same wrapper.

### C2 `crates/search/src/index.rs` (+14 / -1), `testing.rs` (+27), cfg(test) seams

- Commit: dbd79bb0 (#184).
- Change: `raw_files()` and `FILE_QUERY` thread-local, both `#[cfg(test)]`; `compile_cancelled` takes the swapped query only under `cfg(test)`. `testing.rs` adds `append_files`.
- Class C trivial: nothing exists in a non-test build.
- Targeted tests: `crates/search/src/lease/tests.rs` `a_file_scan_cancelled_partway_fails_the_compile_and_caches_nothing` (about 42 lines changed in the range; uses `append_files` and `FILE_QUERY`).

### C3 `crates/search/src/log_events.rs` (+4 / -2)

- Commit: 8ae7def2 (#187).
- Change: `search.enrichment_unreadable` goes from `Debug` to `Trace` with `default_on: false`. The emitting call is in `crates/search/src/index.rs`; unchanged.
- Class C (log level on a search-index path).
- Targeted tests: the `logs_catalog` golden (`crates/lodestar/src/settings/golden/logs_catalog.txt`, changed in the same commit: level debug to trace) via `crates/lodestar/src/settings/logging_tests.rs:348`.

## Class D: filters / metainfo parsing

### D1 `crates/metainfo/src/info.rs` (+221 / -51), `text.rs` (+45), `Cargo.toml` (+2, dev-dependency only)

- Commits: e0fc29ae (info.rs +128/-50, text.rs +23, Cargo.toml +2), bd131a8d (info.rs +67/-5, text.rs +14), 207866e8 (info.rs +32/-2, text.rs +17/-9), all #177 slice/F-metainfo. (c95f7ef5 is tests only.)
- Change:
  - Each file uses its own valid `path.utf-8` instead of all-or-nothing (`resolve_paths`, `map_paths`).
  - Symlink targets feed encoding detection and are parsed for v2 file trees (`symlink_target()`).
  - Symlinks with no `length` get size 0 (`file_size`).
  - Padding files with no `path` get `.pad/padding` (`PADDING_PATH`).
  - The encoding guess is checked against files that carry both a legacy path and `path.utf-8` (`legacy_encoding`, `MAX_ENCODING_PAIRS = 16`).
  - Resolved paths are re-checked against `max_path_bytes` (`enforce_path_limits`): an over-limit path rejects the torrent, an over-limit symlink target is dropped.
  - Hints that name UTF-16 or placeholder encodings are ignored.
  - `text.rs` adds `legacy_candidates`, `is_name_encoding`, `encode_exact`.
- Class D. Untrusted-input parser. Called from `crates/pipeline/src/worker.rs:203` (`Info::parse(&fk.fetched.info)`) on every fetched torrent. Behaviour change: some torrents that were rejected parse now, and file path bytes can differ for mixed legacy/UTF-8 torrents (the decisions file flags this as a loosening of the raw-bytes rule).
- Targeted tests:
  - `crates/metainfo/tests/info.rs`: `bep47_padding_and_symlink_shapes`, `v2_symlink_targets_are_bounded`, `utf8_overrides_correct_a_wrong_encoding_guess`, `resolved_paths_respect_the_byte_limit`, `non_charset_hints_are_ignored`
  - `crates/metainfo/tests/golden.rs` `golden_corpus` with 12 `a1-`/`a2-`/`a3-`/`a4-` cases in `crates/metainfo/tests/golden/`
  - `crates/metainfo/tests/bep47_props.rs` (proptests `v1_mixed_shapes`, `v2_symlinks_keep_their_targets`, with `bep47_props.proptest-regressions`; also a store blob codec round trip through the dev-dependency on `lodestar-store`)
  - `crates/metainfo/src/text.rs` `encode_exact_rejects_lossy_targets`
  - `fuzz/fuzz_targets/metainfo.rs` (+20 / -3) and new seeds under `fuzz/seeds/` (not run by `cargo test`)
  - existing ingest tests in `crates/pipeline/tests/pipeline.rs` that build info dicts and run the worker.

### D2 `crates/filter/src/engine.rs`, `matcher.rs`, `matcher_reference.rs`, `reclassify.rs` (fa0c3956): list exceptions and stored classification

- Commit: fa0c3956 (#179 slice/F-filters): engine.rs +28/-18, matcher.rs +105/-22, matcher_reference.rs +17/-5, reclassify.rs +3/-1.
- Change:
  - Each subscribed list is excepted on its own, then the strongest remaining action wins. Before: lists were reduced to the strongest first, so an exception to one list lifted the others. `listed()` now returns an iterator and a new `released()` helper computes per-list release. Also applies to the pre-fetch check.
  - Stored reclassification (`reclassify.rs`) reads the swarm hashes ingest uses, including the hybrid's truncated v2 hash, so `hash:` classification rules match.
- Class D (content filter). Content-safety relevant (spec §20.6, §20.9, §20.12); the runner marked the token-level neutralize changes as owner-decision §2.19 content-safety refinements flagged to the owner (docs/decisions/2026-10-30-F-filters.md, 03c6d164).
- Targeted tests in `crates/filter/tests/semantics.rs`: `an_exception_to_one_list_leaves_the_others`, `prefetch_stays_blocked_by_a_list_that_isnt_excepted`, `a_list_exception_leaves_packs_rules_and_the_core_alone`, `stored_hash_rules_match_ingest`; proptests `a_list_exception_is_the_list_without_the_torrent`, `stored_classification_equals_ingest`; `the_floor_is_untouched` (03c6d164, checks CS-1a/CS-1b/CS-8a through `judge`, `judge_classified` and pre-fetch).

### D3 `crates/filter/src/matcher.rs`, `matcher_reference.rs`, `engine.rs`, `rules.rs` (neutralize semantics)

- Commits, all #179 slice/F-filters: fa0c3956 (neutralized words compared word by word, in words, phrases and NEAR), 8c5a0b91 (matcher.rs +140/-35, matcher_reference.rs +23/-5: neutralize by token), bd8d2a43 (matcher.rs +137/-52, matcher_reference.rs +66/-16, rules.rs +7/-8: key neutral words by their parts, phrases use whole forms), 2b3d6dab (engine.rs +8/-5, matcher.rs +1/-1: Always allow released only when it matches with and without neutralized words).
- Change: a torrent token that is a neutralized word supplies no evidence anywhere; words compare by one key (parts joined); a phrase with a neutralized part can still match through a live whole or compound form; an Always-allow query never releases more because of neutralizing.
- Class D, content-safety relevant (same references as D2).
- Targeted tests (`crates/filter/tests/semantics.rs`): `a_neutralized_word_never_completes_a_clause`, `other_evidence_still_counts`, `a_word_of_several_parts_needs_its_own_token`, `negation_and_unless_with_a_neutralized_word`, `neutralizing_never_widens_always_allow`, proptest `a_neutralized_word_is_no_evidence`; `crates/filter/tests/props.rs` (+20 lines for the matcher/reference equivalence) with `crates/filter/proptest-regressions/matcher.txt` (+7).

### D4 `crates/filter/src/engine.rs`, `rules.rs`, `core.rs`, `normalize.rs`, `matcher.rs`, `lib.rs` (8bc2373a): performance restructure

- Commit: 8bc2373a (#192 slice/F-filters-neutral-perf): engine.rs +236/-16, rules.rs +96/-18, normalize.rs +126/-4, matcher.rs +23/-19, core.rs +9/-14, lib.rs +2/-2. (d9ef8fa8 and c9c9a7ee are tests only.)
- Change: one per-thread tokenizer shared by the core and keyword entries; paths read in one pass per folder; keyword tokens and neutral keys prepared once per list in the new `OverrideList` (clears on any mutation, `Deref`/`DerefMut` to `Vec<Override>`). The commit states semantics are unchanged; the 50 µs budget is unchanged.
- Class D. Hot path: runs per torrent in ingest.
- Targeted tests: proptest `released_entries_match_the_reference` (engine.rs `mod tests`, compares prepared entries to the old algorithm), `crates/filter/tests/normalize_reference.rs` `paths_read_as_their_joined_text` (folder pass vs reference tokenizer; c9c9a7ee adds pathless files), `crates/filter/tests/engine.rs` (12 lines changed), `props.rs`; seeds `proptest-regressions/engine.txt`. The budget case is a bench gate, reported not held (d9ef8fa8; waits on the open F-filters-xcpu PR #196).

## Class E: API / auth / updater

### E1 `crates/api/src/auth.rs` (+260 / -46)

- Commits, all in `auth.rs`: fe2a6fdb (+53/-21, #178), 585dfca4 (+45/-2, #178), f2b9f873 (+171/-43, #190), e3d768bd (+6/-3, #190), a2c15c9e (+7/-1, #190).
- Change:
  - fe2a6fdb: each saving method builds the next state, saves, then publishes under the write lock; setup spends its token only after the save.
  - 585dfca4: the temp file is created with `create_new`, the directory is fsynced after rename on Unix (best effort), and a failed write removes the temp file.
  - f2b9f873: changes serialise on a separate save lock and take the state write lock only for the swap, so `verify()` (every request) no longer waits on disk; Argon2 runs with no lock held; a failed directory fsync logs `auth.dir_sync_failed` once per run.
  - a2c15c9e: `setup` refuses when a password is already set.
- Class E (auth). No updater code in range (F-updater #198 is not yet merged).
- Targeted tests:
  - `crates/api/tests/auth_failed_save.rs` (new): `every_failed_save_leaves_the_state_unchanged`, `successful_saves_change_what_they_should`, `failed_password_save_preserves_active_credentials`, `failed_setup_save_leaves_setup_pending`, `a_failed_rename_removes_the_temporary_file`
  - `crates/api/src/auth/lock_tests.rs` (new, `cfg(test)`): `readers_never_wait_on_a_save`, `racing_password_changes_serialise`, `racing_setups_serialise`, `a_failed_directory_fsync_warns_once`
  - NO TARGETED TEST FOUND: the `state.password.is_some()` guard from a2c15c9e.

### E2 `crates/api/src/auth/hooks.rs` (new, +48)

- Commits: f2b9f873, e3d768bd (#190).
- Change: test seams (hold a save mid-way, fail the directory fsync). Declared `#[cfg(test)] mod hooks;` in `auth.rs`; the call sites are `#[cfg(test)]`.
- Class E trivial: absent from non-test builds.
- Targeted tests: used by `lock_tests.rs` above.

### E3 `crates/api/src/auth/log_events.rs` (+10)

- Commit: f2b9f873 (#190).
- Change: new catalogue event `auth.dir_sync_failed` (Warn). The golden `logs_catalog.txt` is updated in the same commit.
- Class E.
- Targeted tests: `a_failed_directory_fsync_warns_once` (lock_tests.rs) and the `logs_catalog` golden (`logging_tests.rs:348`).

## Class F: other

### F1 `crates/peer-wire/src/fetch.rs` (+38 / -5, includes about 25 inline test lines)

- Commits: 8ae7def2 (+30/-5), ff535a2e (+8, test only), #187 port/B-log1b-4b.
- Change: `fetch()` counts the failure kind once per fetch through new `counted(&Result<Fetched, Error>) -> Option<FailureKind>`: `Denied` and `Ok` count nothing; other errors count their kind. The old code counted every `Err` except `Denied`; the same result set is counted. Return value unchanged. Only the `fetch.failed_summary` counters are affected.
- Class F (fetch metrics). Runs on every metadata fetch but no lifecycle, sampler or admission logic is touched.
- Targeted tests: `a_fetch_counts_by_its_final_result` (same file, `mod tests`; Ok, Denied, Timeout, Rejected); `crates/peer-wire/tests/fetch.rs` calls `fetch()` unchanged for the transports.

### F2 `crates/peer-wire/src/log_events.rs` (+3 / -1)

- Commit: 8ae7def2 (#187).
- Change: `#[repr(align(128))]` on the `Summary` static so its counters get their own cache lines; doc comment.
- Class F. NO TARGETED TEST FOUND (layout only).

### F3 `crates/lodestar/src/services.rs` (+74 / -10, includes about 50 inline test lines)

- Commit: a16542dd (#191 fix/shutdown-stall).
- Change: the shutdown bound on the HTTP server (`SHUTDOWN_GRACE`, 10 s) moves from an inline `tokio::select!` into `bounded(serve, stopping, grace)`; body and the `app.shutdown_forced` log call are unchanged, and the caller uses `.await.unwrap_or(Ok(()))`.
- Class F (app shutdown). Not the crawler pause/resume.
- Targeted tests: new `mod bounded_tests` in the same file: `a_serve_that_never_ends_is_dropped_after_the_bound`, `a_serve_that_ends_returns_its_result`, `without_the_signal_the_bound_never_runs`; `crates/lodestar/tests/shutdown.rs` `a_stalled_download_cannot_hold_the_shutdown` (+32/-11, asserts exit within the bound and which path was taken).

### F4 `crates/lodestar/src/percolator.rs` (+26 / -7), test only

- Commit: 4625e807 (#193 fix/flake-shutdown-percolator).
- Change: only in `mod tests`: `the_task_turns_commits_into_alerts_quickly` now times commit-to-alert from a commit hook and reports store-to-commit without asserting it.
- Class F trivial.
- Targeted tests: the test itself.

### F5 `crates/pipeline/Cargo.toml` (-1), `crates/search/Cargo.toml` (-1)

- Commit: 8ae7def2 (#187). Also `Cargo.lock` (-2) and `fuzz/Cargo.lock`.
- Change: unused `tracing = "0.1.44"` dependency removed.
- Class F. NO TARGETED TEST FOUND (compile-level only; `git grep tracing origin/main -- crates/pipeline crates/search` is empty).

## Observations for the delta's author (facts, not recommendations)

- Class A has no non-trivial change, so by the rule quoted in D2 nothing here triggers a re-soak on its own. B1/B2 (V12 edited in place plus a repair that runs at open on the soak data directory) and D1-D4 (what gets stored and which torrents are hidden or released) are the changes that alter behaviour for data the soaked build wrote or ingests.
- The plan's rule names crawler lifecycle, the sampler and endpoint policy. Whether the V12 repair and the D1-D4 changes need another soak is not decided by that rule; that is the plan owner's call.
- D2/D3 touch content-safety behaviour; the F-filters decision file records the runner's flag to the owner (§2.19).
- Open PRs #198, #200, #196, #199 are not covered; #196 changes the same neutralize budget case as D4.
