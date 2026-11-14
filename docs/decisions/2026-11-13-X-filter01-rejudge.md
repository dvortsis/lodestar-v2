# X-filter01-rejudge decisions

The re-judge that X-filter01-fix (#843) left as a follow-up, the store's hold precedence for kept holds, and the review nits from X-filter01-hardening (#855). Every hold stays fail-closed. The detail of the fix stays held under `docs/security/v1/README.md` §5.

## Calls
- **The core has a version** (§20.5): `CORE_VERSION` in `lodestar-filter`, now 2. It goes up whenever a change to the core could judge a stored torrent differently, so a later core fix reuses it. 1 covers every core before X-filter01-fix (#843): the released v0.5.1 core, which read text one way, and the later pre-#843 `main`, which read it two ways. A store that records no version was judged by one of them. Runner, by message.
- **The store records what its torrents were judged by** in `settings` rows, with no schema change and no migration, so update rollback keeps opening the store (runner):
  - `filter.core.judged` holds the version.
  - `filter.core.seen` holds the highest torrent id a build that keeps the marker has seen judged. A torrent above it was stored by a build that doesn't (an older one), so it's judged again on the next start. This build moves it up on the writer's flush tick and at a clean stop, but only once no pass is under way, so it never covers what an older build stored. After a crash the overlap is judged again, which holds nothing twice.
  - A pass under way records its version, its phase, where it started, its place and its totals.
  - A store marked by a newer core (a downgrade) has its marker lowered to today's on start, and what was stored above `seen` is judged again; an upgrade later judges everything again.
  - An empty store is marked as judged by today's core, so a fresh server logs no pass: by the writer before its first batch, and by `lodestar import` before it stores anything (one call in `crates/lodestar/src/cli/import.rs`, which also records what the import judged when it ends).
  - Accepted limit: the high-water id can't see a row an older build rewrote in place while downgraded, only rows it added.
- **Every stored torrent is judged again, in two phases** behind the one marker, each resumable and idempotent (runner, replacing the earlier shortcut that let some judgments stand). The first phase takes the torrents `rejudge_first` picks: those the earlier core didn't read whole (`read_whole_by_v1`, a cheap check with the real tokenizer), or whose name or a path reads differently in the earlier reading. Any torrent that won't read is held in the first phase too. The second phase takes every other torrent. Nothing is left under an older core's judgment, so no compile-time premise is needed.
- **Torrents stay visible under their earlier judgment until the pass reaches them** (owner, 2026-11-14T00:55Z). There is no pending gate. The first phase runs right after start, so the torrents most likely to be judged differently, and those that won't read, go first.
  - The pass runs before the enrichment backfill, the import-candidate refile and the unverified sweep. Those still run while it waits out a store failure, so a failing store doesn't stall them.
  - The first real per-million timing, from a real store, is reported to the owner when one is measured. The synthetic figure is under Numbers.
- **The writer runs the re-judge in the background** (`crate::core_rejudge`), one batch per turn between ingest batches: up to 256 stored torrents in id order, and no longer than 250 ms past the first, since one torrent can take up to the core's budget to judge.
  - Its place is the last torrent it finished, written after each batch's holds commit, so a restart resumes there. A restart between the two judges that batch again, which holds nothing twice.
  - A batch whose classification rules changed under it is judged again.
  - A store failure waits out the backoff and logs `pipeline.core_rejudge_failed`. After three in a row, it judges one torrent at a time.
  - A missing compression dictionary is a store failure, not the torrent's. A batch in which more than half the torrents, and more than 16, won't read is treated as one too: it holds nothing, keeps its place and only waits (it doesn't go one at a time, which would hold what the store can't read).
  - A failure to record the high-water id logs `pipeline.store_write_failed`.
- **What it applies:** each stored torrent is judged as ingest judges it.
  - It reads detection's own classification, so a Not adult correction or rule never takes it out of the Adult-only core checks.
  - It reads it as Adult too where an Is adult correction, rule or always-adult source says so, and keeps the stricter verdict. When only that reading holds it, the hold is an adult hold.
  - A decision that holds the torrent back is applied whatever rule made it, and so is any core decision. The packs' and your rules' marks have their own re-evaluation (§20.3).
  - A held torrent is deleted, with its hashes quarantined or tombstoned in the same transaction, marked for intake first, as the backfill's holds are.
- **Counts are logged, never names or hashes.**
  - `pipeline.core_rejudge_started` fires when the pass starts or resumes on a run, with its totals so far.
  - `pipeline.core_rejudge_done` fires at the end: the torrents judged again, how many are now held, and how many of those couldn't be judged at all.
  - A batch's totals are written just after its holds commit. A restart between the two judges the batch again but undercounts it (its held torrents are gone). A torrent whose hashes won't read either is deleted with no hold and counted as held, since it's no longer shown.
  - The totals are kept with the pass's place, so they add up across restarts.
  - The events have names and help in the logging screen, in English and Chinese (`web/src/lib/features/settings/logging/`, generated by `npm run event-names`).
- **A rollback or restore judges its torrents again** when the copy's marker is older. A rollback never carries the store's marker into the copy, so the pass runs on the next start.
- **Fail-closed:** a torrent that can't be judged is held under CS-0a by the hashes as stored, and deleted, never left shown. That covers a row or file list that won't decode, a missing file list, judging that panics anywhere in it, and a file list bigger than the core reads.
  - The pass reads file lists with `Store::files_bounded`, which checks the list's shape before decoding it and caps what it decompresses; `Store::files` is unchanged.
  - Only a store failure, not a row's own, stops a batch.
- **A CS-0a hold is never judged again from its sealed name.** The re-judge reads stored torrents only, never quarantine rows. The stand-in re-judge, which does read sealed names, counts a kept row as kept and never releases it.
- **A kept hold ranks just under the floor** (§20.7):
  - In a rollback's merge: the floor's own rules, then a worker's floor report, then a kept hold, then an adult hold, then any other.
  - A quarantine write replaces a floor row only with another floor hold, and a kept row only with a floor or kept hold.
  - So no `+adult` or stand-in hold can take a CS-0a row's place, and no release by rule can then free it. Runner, by message.
- **The meter's second read is a strict bound** (§20.8): a path read twice is charged its own first read plus every word and character it holds as if the caches were cold. So the core's second read can never cost more than the meter counted. Runner, by message.

## Numbers
- **`budget_worst_case`:** 28 shapes, worst 12–15.7 ms scaled on the build VM this run, against 9–11 ms for the same shapes on `main` in the same session. The difference is the host's noise, not this change: the costliest shape reads the same file count and units on both. The bound (15 ms) and the CI budget limit are unchanged, and CI's `filter budget (§20.8)` job decides.
- **The re-judge (synthetic):** about 143 s per million stored in memory and 169 s on disk (reopened before the run; the OS cache couldn't be dropped on the build VM), in release, on 20,000 made-up torrents, three in ten of them packs of 20 to 180 files (29 files a torrent on average), 2% of them in the first phase (`rejudge_rate`). An extrapolation, not a real store; ingest shares the writer, so it takes longer on a busy server.
- **Held in the repo's corpora:** still 0 of 515 (198 pack-corpus torrents, 317 release names), now counted with `too_large`, as intake checks it.
