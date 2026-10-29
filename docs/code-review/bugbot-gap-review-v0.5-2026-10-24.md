# Lodestar v0.5 Bugbot gap review

Date: 2026-10-24 (America/Denver)\
Reviewed commit: `ad0ec5a84555b8b36e7ae94ed515b84ad6d46e02`\
Branch: `docs/ui-ux-audit-2026-10-24`

**Six new findings: two high, four medium.** Five were reproduced through compiled production APIs; the sixth is a source-verified Unix updater interleaving whose runtime was not exercised on Windows. All remain open. No application fixes, commits, pushes or GitHub comments were made.

Used the installed `review-bugbot` skill with exactly one read-only reviewer subagent and independent parent verification. This environment exposes a generic reviewer subagent, not a separate native Bugbot service. The review is a Bugbot-workflow source audit, not an external Bugbot service result.

## Scope and validation

The owner requested previously untested code and anything missed. The repository already has test infrastructure in all 20 Rust crates; absence of an inline test in an individual file does not establish missing coverage. No repository-wide line/branch coverage measurement was generated. This review instead targeted **previously unreviewed implementations and missing failure scenarios**, then compared candidates with existing tests.

The reviewer computed the branch diff first. It contains documentation/assets, so reviewing that diff alone would not answer the owner's codebase request. The source audit concentrated on Search compilation, file clauses, cancellation, caches, leases/cursors, store upgrades, filtering/snapshot retention, pipeline commits and startup. Parent inspection added settings persistence/import, API jobs/events, peer-wire/uTP and Unix update state writes. Previous parser/auth/filter, UI, packaging and design findings were checked and not duplicated. Deferred installer functionality and other planned features were not treated as bugs merely because they are unfinished.

Focused tests completed on Windows:

- `cargo test --locked --offline --release -p lodestar-store -p lodestar-search -p lodestar-pipeline`: **463 passed, 0 failed, 3 ignored**. Store: 185 passed/2 timing tests ignored; Search: 206 passed/1 timing test ignored; Pipeline: 72 passed. Doctest groups were empty.
- `cargo test --locked --offline -p lodestar-peer-wire -p lodestar-utp -p lodestar-update`: **112 passed, 0 failed**. This covers peer transports and the pure update trust core, not the Unix supervisor/engine.
- `cargo test --release --locked --offline -p lodestar-api --lib jobs::`: **8 passed**.
- `cargo test --release --locked --offline -p lodestar-api --lib events::`: **22 passed**, including matching API log-event tests.
- **Total: 605 passed, 0 failed, 3 ignored** in the existing suites. Separately, three storage regression assertions and one cache/cancellation assertion failed as expected, reproducing the reported defects. The parent independently reran both probe executables and inspected their source and production call chains.

[Reproduction instructions and sources](evidence/bugbot-v0.5-2026-10-24/README.md) include observed probe output and the parent API test logs. No live crawl, production-data operation, full-workspace test run, fuzz campaign, 10-million-torrent allocation or OOM experiment was performed. Settings source inspection was not a complete settings integration test run. Unix updater runtime was not tested on this Windows host.

| Severity | Location (file:line) | Finding |
|---|---|---|
| High | `crates/pipeline/src/writer.rs:252` | Quarantine expiry can erase the evidence needed to scrub an older snapshot. |
| High | `crates/search/src/files.rs:483` | File-query ID caches have an entry cap but no byte budget. |
| Medium | `crates/search/src/lease.rs:133` | Cold file-query compilation completes before checking cancellation. |
| Medium | `crates/store/src/lib.rs:1153` | Verified partial-import upgrades retain the imported name and encoding. |
| Medium | `crates/store/src/lib.rs:1185` | Verified partial-import upgrades omit the enabled raw archive. |
| Medium | `crates/lodestar/src/update/engine.rs:247` | Concurrent updater saves can persist an older state over a successful pause. |

## 1. Quarantine expiry loses the evidence needed to purge an older snapshot

Severity: high\
Location: `crates/pipeline/src/writer.rs:252`; held-hash lookup in `crates/store/src/snapshots.rs:379`.

**Input / scenario:** A migration snapshot contains a stored torrent. The live torrent is later refiled into ordinary quarantine, deleting its live metadata. The server remains stopped through the 30-day quarantine expiry, or snapshot cleanup fails until then. This is reachable through reclassification; `pipeline/tests/corrections.rs:294` exercises newer-parser adult detection whose quarantine deliberately is not a permanent `adult_hold`.

**Expected:** Snapshot copies follow live deletion and retention purges. `snapshots.rs` explicitly promises to remove torrents deleted after the snapshot, using held hashes as the evidence; owner decisions §2.21 extend purge/expiry behavior to migration snapshots, alongside §2.17's deletion guarantee.

**Actual:** The hourly writer executes `store.purge_filter(now, hits_before)` before `store.purge_snapshots(now, hits_before)`. The first call deletes expired ordinary quarantine rows, including the last live hash for this deletion. `Live::read()` then finds no hash in `cs_hashes`, `tombstones` or `quarantine`; the older snapshot's plaintext torrent is left intact. The snapshot can retain the metadata until its own 90-day expiry and make it recoverable through rollback. No purge warning identifies this omission.

**Evidence:** The public-store probe calls `set_enrichments_at()` → `purge_filter()` → `purge_snapshots()` and observes `live_count=0`, `snapshot_count=1`, `purged_torrents=0`, `warnings=[]`. It uses a synthetic `VACUUM INTO` copy with a recognized snapshot filename and an injected later purge time; it does not wait 31 days. Existing snapshot tests at `snapshots.rs:962`/`:1028` cover persistent holds and quarantine already present in the snapshot, not this deletion/expiry ordering.

Confidence: high.\
Suggested fix: Keep deletion evidence independently of temporary fetch holds until all relevant snapshots are scrubbed or expired. Simply reversing the two calls does not handle a failed snapshot write or a hold released before successful cleanup.

## 2. File-query ID caches can exceed the memory budget after a few broad queries

Severity: high\
Location: `crates/search/src/files.rs:483`; cache declaration/cap at `:426–437`.

**Input / scenario:** Submit distinct broad file clauses such as `file:(size:>0)`, `file:(size:>1)` and so on, each matching almost every torrent. Each clause/segment combination has a separate cache key. This is ordinary accepted query syntax; no oversized request is necessary.

**Expected:** Architecture §3 targets steady-state RSS below 1 GB at 10 million torrents with default caches. Search cache storage must have a useful memory bound, including the file-ID sets retained below the outer result cache.

**Actual:** `FileSetCache` retains `Arc<Vec<u64>>` results and clears only when its map reaches 4,096 entries. It does not count vector capacity or bytes. Skipping file queries in the separate 8 MiB `ResultCache` does not disable this lower-level cache. A result covering 10 million torrent IDs needs at least 80,000,000 bytes for elements alone; fourteen such results exceed 1 GiB, before vector spare capacity and the rest of the process. This is a scaling calculation, not an observed 10-million-row run.

**Evidence:** With 10,000 one-file synthetic torrents, 100 distinct already-cancelled file queries retained **13,123,004 bytes**. `SearchIndex::clear_cache()` released **13,115,692 bytes**. The parent reproduced the same figures. The probe's 8 MiB assertion is a diagnostic ceiling, not a claim that an existing setting limits `FileSetCache` to 8 MiB. Current byte-accounting tests (`search/tests/cache.rs:222`, `search/src/cache/tests.rs:490`) concern the outer cache.

Confidence: high.\
Suggested fix: Account for capacities, keys and entries under a byte budget, evict appropriately and decline oversized entries. Bound the retained file-ID cache separately or include it in the shared cache accounting.

## 3. Cancelled cold file queries still enumerate, sort and cache their matches

Severity: medium\
Location: `crates/search/src/lease.rs:133`; compilation at `crates/search/src/index.rs:954`, scan at `crates/search/src/files.rs:470`.

**Input / scenario:** A broad file query starts on a cold cache, then its HTTP client disconnects or a newer query supersedes it. The API's cancellation guard fires while the blocking search task is preparing file clauses.

**Expected:** Architecture §8 and `cancel.rs` require cancellation checks between segments and during scans so abandoned requests stop early.

**Actual:** `/search` constructs `Lease::new_at()`, which calls `index.query_at()` before `count::total()` checks the token. Compilation calls `files::torrent_ids()` with no cancellation parameter. Its complete file enumeration, vector sorting, deduplication and cache insertion therefore happen before the request can return `Cancelled`. This is independent of finding 2: a byte cap would not stop abandoned scans, and adding cancellation would not bound valid searches' cache growth.

**Evidence:** Every `Lease::new()` in the allocation probe receives an already-cancelled token. All 100 calls return errors, but only after creating the file-cache allocations described above. Existing cancellation tests in `search/src/lease/tests.rs:198` inject an already-compiled query and bypass this stage.

Confidence: high.\
Suggested fix: Thread the token through compilation/file enumeration, check before expensive work and periodically while scanning, and propagate cancellation distinctly. Do not route cancellation through the current compilation fallback that logs a file-query failure and substitutes an empty ID set.

## 4. A verified upgrade leaves a partial import's stale name in storage

Severity: medium\
Location: `crates/store/src/lib.rs:1153`.

**Input / scenario:** A bitmagnet import creates a `FILES_PARTIAL` row with a name or encoding that differs from the verified info dictionary fetched later. Startup queues partial hashes for metadata refetch (`pipeline/src/lib.rs:893`), and the worker commits the verified metadata.

**Expected:** `upgrade_partial()` promises to replace the partial row's file list and info-derived columns from verified metadata. The stored name and its encoding should agree with that metadata, while preserving intended import provenance.

**Actual:** The upgrade UPDATE replaces size, piece/file counts, flags, metadata time and an optional v2 hash, but omits `name`, `name_raw` and `name_enc`. The imported display/search name survives while enrichment can describe the fetched name. API detail and magnet naming read the stored name.

**Evidence:** The public-store probe imports `Old Imported Name`, then upgrades the same hash with a parsed info dictionary named `Real Name.mkv`. The result remains `Old Imported Name`. Existing upgrade fixtures at `store/tests/store.rs:575`/`:689` and `search/tests/index.rs:237` use the same name for the partial and full inputs, so their successful results do not establish this behavior.

Confidence: high.\
Suggested fix: Update all authoritative name columns during verified upgrade, including clearing obsolete raw-name/encoding values when the fetched name is plain UTF-8. Cover differing imported/fetched names and encodings.

## 5. A partial-import upgrade skips the enabled raw metadata archive

Severity: medium\
Location: `crates/store/src/lib.rs:1185`.

**Input / scenario:** Enable `archive_raw`, import a partial row without raw bytes, then fetch and commit the complete verified info dictionary.

**Expected:** The configured raw archive retains the fetched info dictionary minus `pieces`, as it does for a new torrent. This is part of the same verified-upgrade transaction.

**Actual:** Preparation correctly produces `p.raw`. New insertion writes it to `torrent_raw` at `lib.rs:1095`; `upgrade_partial()` never writes it. Once the upgrade clears `FILES_PARTIAL`, later duplicate inserts skip the upgrade altogether, so this path does not repair the missing archive.

**Evidence:** The public-store probe shows that the file list upgrades successfully (`file_count=1`) while `raw_info(id)` returns `None` with `archive_raw=true`. Existing archiving coverage at `store/tests/store.rs:221` exercises new insertion; the partial-upgrade tests use the default disabled archive setting.

Confidence: high.\
Suggested fix: Upsert the prepared raw archive during the verified upgrade when it is present. Test this independently from file-list completion and the name update in finding 4.

## 6. Concurrent Unix updater saves can overwrite a successful pause

Severity: medium\
Location: `crates/lodestar/src/update/engine.rs:247`; pause at `:665`/`:840`, atomic writer at `crates/lodestar/src/update/layout.rs:282`.

**Input / scenario:** A manual or scheduled update check finishes while an administrator calls `/updates/pause`. The check completion and pause both call `save_updater()`; only checks share the separate one-check-at-a-time mutex. Pause runs in a blocking task and can overlap the check's save.

**Expected:** The persisted `updater.json` should reflect the latest serialized state mutation. A successfully acknowledged pause must survive a restart unless explicitly changed.

**Actual / interleaving:** `save_updater()` mutates and clones state under `inner.updater`, releases that mutex, then writes the clone to disk. Check A can clone state with no pause and be descheduled. Pause B then sets a future `paused_until`, writes it and succeeds. A subsequently writes its older clone over B's file. Memory still shows the pause, but a restart loads the unpaused file. The fixed `.updater.json.tmp` filename also permits overlapping writers to remove each other's temporary file; I/O errors are ignored here.

**Validation limit:** Parent and reviewer independently inspected the concurrent callers and mutex lifetime. This finding is supported by the concrete source interleaving; no Unix execution or deterministic concurrent-file probe was run. The engine is `cfg(unix)`, and Windows serves an unavailable update engine. This does not claim that the deferred automatic installer was exercised or that it installed anything.

Confidence: high for the source race; runtime reproduction pending.\
Suggested fix: Serialize persistence with mutation, or queue ordered immutable states to one writer. Unique temporary filenames alone do not prevent a stale state from overwriting a newer one. Add a barrier-controlled concurrent check-completion/pause regression on Unix.

## Checked paths and remaining limits

The reviewer fully read `search/src/handle.rs`, `search/src/cursor.rs`, `search/src/sorted.rs`, `pipeline/src/coordinator.rs`, `pipeline/src/have.rs` and `pipeline/src/enrich.rs` without another actionable finding. `search/src/files.rs` was fully read and yielded findings 2/3. Relevant production sections and tests were inspected in Search index/query/leases/cache/live indexing, store insertion/upgrades/filtering/snapshots and pipeline writer/startup.

Parent inspection sampled settings registry transaction/watch behavior and TOML import/export, API job control and event delivery, peer metadata framing/assembly and uTP socket dispatch, and updater check/state-write paths. Passing focused tests do not establish that every branch in those modules is clean. The complete Unix supervisor, rollback journal and all settings integrations remain outside this run's executed coverage.

Highest priority: preserve snapshot-deletion evidence across quarantine expiry and enforce a byte budget for file-ID caches. Then address cancellation, complete the verified-upgrade transaction and serialize updater persistence. These are review results, not completed fixes.
