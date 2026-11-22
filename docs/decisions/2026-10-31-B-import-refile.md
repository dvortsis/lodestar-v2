# B-import-refile decisions: waiting import candidates are judged again when the rules change

Closes IMP-1's open item "Candidates aren't refiled when filter or classification rules change" (`2026-10-30-IMP-1.md`, Known follow-ups; runner, review important 4). It tightens content safety and loosens nothing. No schema change, no API change, no new metric or log event.

## Inventory: where a candidate's name or fields can be read

This decided how urgent each step was. As of `main` on 2026-10-31:

- **Names:** read only inside the store, by `commit_candidate` (folding a record into a waiting row) and now by the refile sweep. No API route, CLI command, export, digest, Insights series or log line reads a candidate's name. The snapshot purge reads candidates by hash only (`snapshots.rs`, `CANDIDATE_BY_HASH`).
- **Counts:** `ImportJob.counters.candidates` and `open_candidates` (the live count). They reach `lodestar import status` (text and `--json`, `waiting_candidates`), the removal report (`candidates_deleted`), and the job's `complete_with_issues`.
- **Hashes:** `import_candidates.hash`/`hash_v2`, read by the import commit (`store_hold`, folding) and the schema's hold triggers.
- **Snapshots** (v13+) hold candidates, names included, at rest. The snapshot purge drops a candidate whose hash the live store holds, through `snapshot_deletions`, which the hold triggers fill. So a candidate the sweep holds back leaves the snapshots at the next purge, as one held at import would.
- **So today the exposure is counts and the plaintext name at rest,** not a screen. The name at rest is what §20.3 forbids for a held item ("only the hash, rule id and a sealed name are kept"), so the sweep removes rather than marks, and scrubs the WAL after.

## What a rule change does to waiting candidates

- **Removed, not marked held** (the brief's choice). A held candidate is written exactly as an import would have held it: a filter hit, and the hashes tombstoned or quarantined (with the name sealed, when there's a seal key). The schema's existing `import_candidate_tombstoned` / `_quarantined` triggers delete the row and journal the hash for the snapshot purge. Reasons:
  - It's the import-time outcome: a record a rule holds at import never becomes a candidate, so the store has one state for "held", not two.
  - Marking would keep the name in the clear next to a hold, against §20.3's seal rule, and would need a column (a migration) and a filter on every reader.
  - Releasing the rule releases the tombstone or quarantine like any other (§20.7), and the torrent can be fetched normally then. The candidate's partial fields are gone, as they would be for a record held at import.
- **Only holds act.** Hide, demote and flag change nothing on a candidate: it has no `flags`, and it's judged again in full if it's ever stored. Shadow decisions aren't written: the sweep runs at every start, so writing them would add the same `filter_hits` row each time.
- **Hash holds were already covered:** a hash added to `cs_hashes`, `tombstones` or `quarantine` deletes the candidate through the schema's triggers. The sweep adds what the triggers can't see: your rules, packs and the core's name terms, subscribed lists' hashes (held by the engine, not a store table), and your classification rules (an Is adult rule adds the Adult-only checks, with an adult hold as at import).

## The same engine as the server

- The sweep runs in the pipeline's writer on `PipelineConfig::filter`, the engine the server loads through B-import-server-1's shared loader. No second engine is built.
- **The judgement mirrors the import driver's `judge_candidate`** (`sources-importers/src/driver.rs`): the floor's and dropping lists' hashes first (`Engine::blocked_hash`), then `Engine::judge_classified` on the name with no files, facts from its size and source date.
  - **One difference, stricter:** the sweep judges a hybrid candidate's truncated v2 hash too. The driver judges the primary hash only; the store's own hash checks at commit cover both, but a list hash the engine holds (not a store table) would only be seen on the primary. That gap in the driver is part of the follow-up below. (Closed by `2026-10-31-X-import-refile-2.md`.)
  - It's re-stated in `pipeline::enrich::judge_candidate` because `pipeline` doesn't depend on `sources-importers` and that crate isn't this slice's. Moving both onto one function in `lodestar_filter::reclassify` (judging both hashes) is a follow-up for a slice that owns `sources-importers`. (Done in `2026-10-31-X-import-refile-2.md`.)

## When it runs, and how much at once

- **At every start, and whenever the engine's version changes** (any rule, pack, list, core switch, exception or classification-rule change bumps `Engine::version`). The writer checks the version on its flush tick and starts the sweep over from the first candidate.
- **Keyset batches of 512 by candidate id,** one short `BEGIN IMMEDIATE` transaction each, on the writer's own connection. The writer's `select!` is unbiased, so ingest batches, touches and the flush interleave with it the way they do with background enrichment. It never runs in one long transaction and never blocks crawling.
- **A rule change mid-batch writes nothing:** the batch is judged only when the engine is still at the sweep's version, and `Store::refile_import_candidates` asks again inside its transaction. A refused batch restarts the sweep under the new rules. A release that follows a rule change takes the write lock after the engine moved, so it always sees (and releases) what a batch wrote before it.
- **Cost at the 10M/50M pilots:** one name judgement per candidate per start or rule change, in the background. Background enrichment has the same shape.
- **A rule change during a running offline import** is the CLI's problem (IMP-4 fixes the engine for a run): the server isn't running then.
- **Follow-up for B-import-server-2:** online imports, judged under an engine fixed for the run, could commit candidates after the sweep finished; those would wait for the next rule change or start. That slice should restart the sweep when an online job reaches `indexing`.

## Counters stay conserved and truthful

- **A removed candidate moves from `candidates` to `filtered` (a tombstone) or `quarantined`** on its job, in the same transaction. `read = new + merged + candidates + filtered + quarantined + invalid` still holds, and the reason is the one an import uses for a record a rule held. No new counter column: that would be a migration for a number the existing two already state.
  - This moves a finished job's counters, which IMP-1 otherwise leaves alone. The change is what the job's records are now, which is the truthful reading the brief asks for. `revision` isn't bumped for counters (IMP-1: only phase and control changes bump it).
  - A candidate removed by a hash trigger (a CS-1 entry, a tombstone written elsewhere) still leaves the counters as IMP-1 left them. Unchanged here.
- **What `candidates` means now:** records the job counted as candidates, minus those a rule later held back (which this sweep moves to `filtered` or `quarantined`). It's neither IMP-1's purely historical count nor the live one: a candidate later stored, or held by a hash trigger, still counts. This refines IMP-1's "historical `candidates` counter" and "advanced only inside the batch transaction"; both lines there now point here.
- **Only the sweep moves counters or flips `complete_with_issues`.** A hash-trigger removal (a CS-1 hash, a tombstone) and a candidate later stored as a torrent leave both as they were (`2026-10-31-X-import-refile-2.md`).
- **`open_candidates` is the live count** and so follows by itself.
- **`complete_with_issues` becomes `complete`** when the sweep removes a job's last waiting candidate and it has no invalid records or dropped file rows: the outcome `set_import_phase` would pick now. That's a phase change, so it bumps `revision`.

## Seal and logs

- **No candidate name reaches a log line or an error.** `WaitingCandidate`'s `Debug` gives the name's length only. A failed batch logs the existing `pipeline.enrichment_failed` with the store error, which carries no values. An operator can't tell it from a failed enrichment pass; a dedicated event (it needs the log catalogue's golden file, outside this slice) is a follow-up. A quarantine keeps only the sealed name; without a seal key it keeps none (§20.7).
- **The WAL is scrubbed** after a batch that held anything, as `set_enrichments_at` does after deleting stored torrents.

## Tests

- `crates/pipeline/tests/import_refile.rs`: a drop rule added after import removes the matching candidate and keeps the counters conserved, the non-matching one stays, and a second pass changes nothing; a core name term (quarantined, name sealed) and a CS-1 hash added only to the engine remove too; the last waiting candidate removed completes the job; 1,200 candidates in 12 batches of 100; a stale engine version writes nothing; a waiting candidate's `Debug` has no name.
- `import::tests::a_stale_refile_writes_nothing` (store): a refile whose rules changed before it commits writes no hold, hit or counter; a current one holds the candidate once, and a second call finds it gone.
- `writer::tests::the_writer_refiles_waiting_import_candidates`: the writer's own sweep holds a candidate back and intake's held set refuses its hash, and only for candidates the store actually held; a rule added after the sweep finished restarts it on the flush tick's check; a batch under stale rules restarts it under the current ones. A capturing log sink, rendering every field unmasked, sees no candidate's name. The error arm is driven since `2026-10-31-X-import-refile-2.md`.
- No waits: every test is synchronous on the store.
