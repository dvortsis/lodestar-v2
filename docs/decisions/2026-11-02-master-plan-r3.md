# Master-plan update r3 decisions (2026-11-02)

An external review of the merged r2 plan (#369), done against `e528c369`, found three P1 and five P2 gaps. Each was checked against the code before it was taken: the Linux collector's allowlist (`docs/soak/tools/linux/collect.py`), the rebuild path (`crates/search/src/rebuild.rs`), the workflow job names (`web.yml`, `e2e.yml`), the runner's launch limits (`runner/README.md` step 7) and CR-4's route-backoff gate (`docs/decisions/2026-11-02-CR-4.md`). All held. This update applies them on top of §2.28 (#371). Docs only.

## Owner answer in this session
- **#357 ships in v0.5.1 as rc.3** (`docs/owner-decisions.md` §2.29). It replaces §2.28's "rc.3 only if the soak must be re-run anyway".
- **#367 is the owner's to add.** rc.3 is exactly rc.2 + #357, as §2.29 says (the runner's ruling on #372). Without F-filter-standin-undo (#367), nothing undoes what #357's stand-ins hold, and rc.3 doesn't write the `filter.stand_ins` record a later upgrade needs to re-judge them. The delta report states that gap. The runner recommends adding #367 if it has merged by the cut and puts the question in the owner's daily summary; #367 rides along only on the owner's yes.

## Runner refinements (flagged under §2.19 and §2.26)
None of these loosens an owner decision, weakens a security or content-safety decision, or changes an owner-set number. Most tighten.
- **CR-COL-3 before any live crawler run.** The Linux collector keeps only CR-0's series, so a completed run would lose `searchable_new_total`, the latency histogram and its dropped counter, and CR-4's family-labelled, connect-timeout, no-route and uTP series (only `fetch_attempts_total` and `fetch_success_total` survive, by `transport`): the gate's own outcome and the proof a treatment was active. The review reproduced this with `sanitize_snapshot`. Every CR-AB gate now depends on CR-COL-3 and a rehearsal.
- **CR-SPEC, a measurement specification frozen before collection, then confirmed by the owner.** §2.25 makes the exact measurement rules the owner's to confirm before the first acceptance analysis, so the first booked run (CR-AB-COMP) and CR-COMB depend on `ext:owner-cr-spec`, and the spec is confirmed before any data exists, and CR-COL-3, which implements the rules, depends on CR-SPEC. It fixes the window's start boundary (also for the fresh-directory 24 h soak, whose 6 h suppression would otherwise change the mean and the worst hour by accident), consecutive 60-minute intervals counted from the window start (not wall-clock aligned, not rolling), carry-over between arms, the cohort, mechanism checks and the analysis. **Latency coverage:** below a stated coverage the p95 is "not measured", never a pass, and dropped samples count as above the threshold when bounding it. `docs/metrics.md` lets bounded queues drop latency samples while throughput still counts, and loss concentrates in congested periods.
- **Checkpoint 1 reports a projection with uncertainty,** not a measured ceiling. Isolated gains needn't combine additively or multiplicatively, and 6 h arms don't bound a 24 h worst hour. The owner's choice of Checkpoint 1 (§2.27) is unchanged.
- **Route backoff keeps CR-4's own gate:** its A/B runs only if a baseline shows `fetch_no_route_total{family="ipv6"} > 0`; otherwise the skip is recorded.
- **Ordering moved from prose into the graph:**
  - CR-AB-ADM depends on the comparator;
  - CR-5 completes by tuning or by a recorded reasoned skip, and CR-COMB depends on it;
  - host externals hold for one exclusive booking per run, so disjoint `owns` can't put two gates on one host.
- **The four-slice cap is in the launch procedure.** It's `limits.phase_total: 4` in `runner/slices.yaml`, and `runner/README.md` step 7 enforces it, docs included. Step 7 also defers to the build plan's ladder when choosing between ready slices.
- **SB-6's population contract.** A field SB-6 adds is empty on records indexed before its producer ships. `rebuild.rs` opens a current-version index without rebuilding, and the live indexer visits only new ids and `reindex_log` entries. So every producer ships:
  - a per-field population generation;
  - a resumable backfill through `reindex_log`;
  - completion criteria;
  - truthful availability while the backfill runs: a prefilter never reads an unpopulated field as "no match".

  r2's "the 50M corpus is indexed once" is withdrawn. Producers that land after 50M have their backfill cost measured at 10M and budgeted, and the 50M approval names which fields were populated.
- **G-rebuild-10M is separate from G-import-10M.** Importing into a v6 index isn't a v5 → v6 rebuild. The rebuild gate builds a 10M v5 corpus with a pinned pre-SB-6 binary, upgrades a copy, and measures:
  - the rebuild itself;
  - serving while it runs;
  - catch-up;
  - the disk high-water mark;
  - an interrupted resume.
- **The import ladder is offline-only.** The 1,000/s online line, writer-share fairness and search latency under import belong to B-import-server-2 (still deferred under §2.28), now in the graph. Import speed is measured to job completion, durably written and indexed, and reported as write time plus index drain. The relevance fixture is a regression check only; NDCG on real data waits for P7's judgment set.
- **The required web checks need X-required-web-checks first.** This refines §2.28's "Required checks" item, whose names (`Web / web`, `E2E / run`) don't exist as checks. It adds always-reporting aggregate checks with explicit applicability. The jobs today are `check + test + build + size` and `playwright (core, matrix, sample, motion, keyboard)`, in path-filtered workflows that would stall unrelated PRs if required. The owner's ruleset change waits for it.
- **The handoff needs an explicit handover.** A stale `updated_at` no longer authorizes Path B, because the watchdog restarts the runner after 4 h. The handover works like this:
  - the owner disables the runner's tasks;
  - `HANDOVER.md` on `runner-state` records the holder and each claim;
  - claimed branches open draft PRs early;
  - both paths re-check `HANDOVER.md` before starting and before pushing;
  - a returning runner adopts the claims (`runner/README.md` step 1), and neither the runner's re-reads nor the watchdog act while `HANDOVER.md` exists;
  - slices the runner launched before the handover are listed as not to touch.
- **The v0.5.2 cut freezes a feature manifest first,** and its upgrade smoke pins the v0.5.1 schema and the `<db>.pre-v13.db` snapshot.
- **The experiment calendar is stated:** about 8–9 measured host-days, plus each arm's suppression and warmup and a ~30 % retry budget, so roughly three weeks of exclusive host time.
- **D-egress-note and X-egress-copy:** §2.28 accepted that blocklist fetches ignore `HTTPS_PROXY`. D-egress-note will say so in the user guide, and X-egress-copy, a follow-up web slice after S-blocklists (#370), in Settings → Blocklists.
- **X-strip-format-chars is a recorded exception.** The runner launched it while #367, which also owns `crates/api/src/digest.rs`, was merge-on-green. #367 merges first and X-strip-format-chars brings `main` in, so its dependency on #367 is dropped from the graph rather than left contradicting the running state.

## New slices
- CR-COL-3, CR-SPEC, G-rebuild-10M, X-required-web-checks, D-egress-note (the user guide), X-egress-copy (the in-app Blocklists copy, after S-blocklists).
- A new external, `owner-cr-spec`.
- B-import-server-2, recorded as deferred.
- CR-4 recorded as merged (#365).
