# X-commit-rejudge-reuse decisions

Runner brief 2026-11-06T04:45Z, the follow-up to RP-G (#526, `docs/decisions/2026-11-06-RP-G.md`); architecture §20.1, §20.4, §20.6, §20.8, §20.9. No verdict changes, no migration, no owner decision, security or content-safety decision, or owner-set number changes. One figure goes to the runner ("For the runner", under Measured).

## What was repeated
- **A flooded or cloned commit** was judged twice in full: prepare's `Engine::judge`, then the writer's `Engine::judge_at_commit` with SP-3 or SP-4 matching. The second differs from the first only in the pack layer, and there only in SP-3 and SP-4: they're the only rules that read the writer's state (`Ctx::with_flood`, `Ctx::with_clone`), and without it neither matches.
- **An always-adult source's find** was read twice in full, as found and as Adult (`judge_at_commit(.., adult: true)`, `reclassify::judge_source_adult`), each on a torrent prepared afresh. The two readings differ only where the classification is read: the core's content type, the packs, and the enrichment's first tags (`type:`, then `adult:`). Everything read from the name and paths is the same text.

## The design
- **The trail (`engine::Trail`).** A judgment with facts can keep what each reading found above the pack layer (`Engine::judge_classified_with`): the reading settled there (an Override entry, a core hit, *Always allow* or one of your rules) with its decision and shadow decisions, or it reached the pack layer, with what your stand-ins held, the pack layer's decisions after exceptions in order, and the pending versions' shadow decisions. The worker keeps it in `Results` (`source_adult::judge_kept`), and the writer's re-judge replaces it (`writer::rejudge`). A torrent cache's file keeps none, so its commit judges in full as before.
- **The writer's second judgment from it (`Engine::judge_at_commit_reusing`).** The floor is judged again, on both readings. SP-3 and SP-4 are judged with `at`, through the same code the full path uses (`Engine::pack_rule`, `Engine::follows_original`), with their exceptions, switches, overrides and pending versions, and set in their places among the kept decisions; the strongest action wins, the last of equals, as in the full layer. Where a reading settled above the pack layer, its verdict is the kept one, as the full path never reaches the packs there. The two readings (`Reading`) and the two classification readings combine as before (`both_readings`, `stricter`).
- **When the trail is read.** Only when all of these hold; anywhere else the commit is `judge_at_commit` in full, exactly as before:
  - the same engine: the same `version` and a new private `generation`, stamped by `Engine::new`, every clone, `Handle::new`, `Handle::update_engine`, `set_list` and `retain_lists`. Any rule, pack, switch, override, version, list or core change made through the handle is a new generation, so the floor and the child-safety core are never reused across a change of rules;
  - the same torrent: a fingerprint of the hashes, name, file count, content type and the facts the layers read (size, piece length, flags, classification);
  - the same time: every instant `now` was compared with (an expiry, the end of a shadow period, noted by `Clock`) falls on the same side of the commit's `now`, and where a rule read a date or a time of day (`Doc::read_time`), the facts' `now` and `discovered_at` are the same;
  - the trail has every reading the commit needs (an Adult reading for an always-adult source or an Is adult rule).
- **The Adult reading shares the reading as found's work (`engine::Adult`).**
  - The torrent is prepared once: the Adult reading's `Doc` (`Doc::as_adult`) shares the text caches (tokens, keys, paths, extensions, contents) and the enrichment, with its own first tags (`type:adult`, the adult kind) and their keys (`adult_tags`). Debug builds check them against a fresh `DocEnrichment::new`.
  - The core judges the reading as found's signals as Adult (`Core::pattern_signals`, `Core::judge_signals`): the signals don't read the content type, and the existing `shared_folders_read_as_whole_paths` property proves the split.
  - Your rules: both readings' candidates come from one walk of the rule index (`RuleList::candidates_pair`), which answers every gate but a tag gate once and each reading's tags on its own. A rule whose query and `unless` read no tags meets both readings or neither and matches the same, so its match is the reading as found's; any rule that reads a tag is matched again. Exceptions, shadow periods and stand-ins are judged in each reading.
  - The pack layer runs in full on the Adult reading, from what the reading as found read from the name and files (`Ctx::with_text`).
  - All of it is reused only where both readings neutralize the same words; otherwise the Adult reading is judged on its own.
- **The precedence in `judge_at_commit`'s doc comment is unchanged**: floor, CSX-5 Override and *Always allow*, a core hit, your rules, then the pack layer, strongest action wins. Pending versions stay in shadow. The code paths that decide are the same functions, split at the pack layer, not copies.

## Why the verdicts are the same
- **The trail.** A judgment is a function of the engine, the torrent, the facts and the time. The trail is read only for the same engine (generation and version) and torrent (fingerprint), so what differs is the time and `at`. Time enters only through the comparisons `Clock` notes and the date leaves `Doc::read_time` marks; where each comparison falls the same way and no date was read (or the times are equal), every layer above the packs takes the same path. `at` enters only through SP-3 and SP-4, which without it match nothing, so the kept pack decisions are the full layer's minus those two, and adding them in order gives the full layer's result.
- **The Adult reading.** `Classification::as_adult` changes the content type, adult kind and confidence. The matcher reads the classification only through the enrichment, whose only fields that change are the first tags; so a rule that reads no tags reads the same in both, and so does every gate but a tag gate. The core's signals don't read the content type. The floor doesn't either, but it's judged again anyway.
- **Never weaker.** Both paths are the full path's verdict, not a bound on it, so nothing is weaker than today's `judge_at_commit`.

## Tests
- **The differential property** (`crates/filter/tests/props.rs`, `reuse::the_second_judgment_is_the_full_one`, 3,000 cases): prepare's judgment as the worker makes it, then the commit's from its trail, against `judge_at_commit_unshared`, the full judgment as it was, kept callable as the oracle. It draws flood levels (none, flag, demote), clones of SP-2 and SP-3 originals at demote and hide, an always-adult source at prepare and at commit apart, Is adult and Not adult rules, torrents already Adult, fresh and upgraded installs (versions pending in shadow), pack overrides with and without an `unless`, exceptions to SP-2, SP-3, SP-4, MW-6, CS-4a, a list and your rules, your rules with tags, dates, sizes, negations, shadow periods and stand-ins, CSX-5 entries with expiries and shadow periods, *Always allow*, a hiding list, the floor's hash, `now` and `discovered_at` on either side of those instants, and a change of rules between prepare and commit, after which the trail must not be read. Each first judgment (`judge_classified_with`, `judge_source_adult_with`) and the shared `judge_at_commit` are checked against their unshared oracles in the same cases.
- `reuse::the_property_reads_trails`: the trail is read in most cases (all but the floor's hash, a date read or `now` across an instant), and those include SP-3 and SP-4 deciding and the Adult reading replayed, so the property tests the path it's about.
- `reuse::a_trail_is_never_read_for_another_torrent`: another name, other hashes, flags, size or classification don't read the trail, and the commit judges in full.
- `reuse::each_reading_neutralizes_its_own_words`: a Neutralize entry scoped to the reading as found; the Adult reading meets your rule the found one doesn't, as the full judgment does.
- `reuse::the_floor_is_judged_again_under_a_trail`: with a trail that fits (the engine edited in place, which no running engine is), a hash put on the floor after prepare decides at commit, as found and as Adult.
- **In `budget`**, before timing: on the budget's own 512 torrents, the trail holds for every one and the commit from it equals the full judgment, as found and from an always-adult source.
- **RP-G's guardrail** `the_core_and_the_adult_floor_hold_under_every_pack_state` passes unchanged.
- **RP-G's leftover** (`crates/pipeline/tests/accept_holds.rs`): real stored holds. Through the pipeline, on an install an earlier build set up, MW-2@1 hides a stored torrent and FK-2@1 flags another (precision items 23 and 107, which version 2 lets through), with their filter hits, and a rule of yours sends a third to the review queue. Version 2 of both is accepted as the API accepts it, and the pipeline runs again under it. Every review row, tombstone, filter hit, exception and stored flag is as it was, while a new copy of the MW-2 item is judged by version 2 and stored unhidden.

### Mutations
Each run against the new path, then reverted (the reuse tests above, and RP-G's core and adult-floor guardrail):

| Mutation | Caught by |
|---|---|
| The floor skipped on reuse | `the_floor_is_judged_again_under_a_trail` |
| Reused across a rule change (generation and version unchecked) | the property (`holds_for` after a change) |
| `now` across an expiry or a shadow period's end | the property |
| A date read at another time | the property |
| A rule that reads tags reused in the Adult reading | the property |
| The Adult reading's candidates from the found reading's tags | the property |
| SP-3 not judged again at commit | the property |
| The Adult reading's core judged as found | the property, and RP-G's guardrail |
| SP-3's and SP-4's pending shadows dropped at commit | the property |
| Exceptions to SP-3 and SP-4 ignored at commit | the property |
| Neutralize words not compared before reuse | `each_reading_neutralizes_its_own_words` |
| The fingerprint unchecked | `a_trail_is_never_read_for_another_torrent` |
| The Adult reading's floor skipped on reuse | none: an equivalent mutant. The floor doesn't read the content type, so the Adult reading's floor is the found reading's, judged just before; it stays judged again as defence in depth. |

The first run of the property built its trails before the engine went into a `Handle`, which stamps a new generation, so no trail was read and seven mutations passed. That's fixed: prepare and commit share the handle's engine, and the property now asserts the trail holds wherever nothing should stop it.

## Measured
**In this session's container** (an Intel Xeon @ 2.30 GHz, scaled by the calibration, 0.68–0.70× the reference; it reads 7–10% above the reference runner, as RP-G found), release build, reference-runner µs per torrent at 10,000 rules, mixed:

| Case | Before (RP-G) | After |
|---|---|---|
| at commit, one judgment (held) | 47.3–47.9 | 47.4–49.4 (unchanged code path) |
| a flooded or cloned commit's two judgments (held) | 86.2–88.4 | 48.8–49.9 |
| at commit, from an always-adult source, from prepare's trail (held, new label) | — | 0.9–1.0 |
| an always-adult source's find read as found and as Adult (reported; RP-G's "at commit, from an always-adult source", measured the same way) | 88.4–89.1 | 51.9–53.4 |
| an always-adult source's flooded or cloned find's two judgments (reported) | — | 54.1–55.2 |

Every held case passed in this container. In instructions (callgrind), the two judgments cost 1.02–1.05× one, and the two-way reading 1.075–1.09× one, from 1.96×.

**From CI** (`filter budget` job 127755868647 on #532 at `fcbe86e9`). It ran on an Intel Xeon Platinum 8370C, not the EPYC 7763, so the times are scaled to the reference: the calibration × the 8370C's skew, 1.033×, which the job marks provisional (from 4 jobs). Reference-runner µs per torrent at 10,000 rules, mixed:

| Case | Before (RP-G, EPYC 7763) | After |
|---|---|---|
| at commit, one judgment (held) | 44.0 | 42.2 |
| a flooded or cloned commit's two judgments (held) | 81.3–82.0 | 43.3 |
| at commit, from an always-adult source, from prepare's trail (held, new label) | — | 0.9 |
| an always-adult source's find read as found and as Adult (reported) | 82.3 | 46.8 |
| an always-adult source's flooded or cloned find's two judgments (reported) | — | 48.2 |

Every held case is under 50 µs; the tightest of the brief's two, the two judgments, at 43.3 µs. A run on the EPYC 7763 itself is still wanted to confirm.

### For the runner
- **The always-adult case is measured as the writer now judges it, under a new label.** RP-G's 82.3 µs case was `judge_at_commit(.., adult: true)` in full. The writer now makes that judgment from prepare's trail wherever one holds (every commit but a torrent cache's file, or one after a rule change or a date read), and that is the held case. The full two-way reading is still measured, under its own label, reported, not held: it is prepare's judgment of an always-adult find (and the commit's where no trail holds), one judgment read twice. It came down from 82.3 to 46.8 µs (CI, scaled from an 8370C), under the budget but by less than 7%, so it's reported rather than held. The runner may want to flag it to the owner under §2.19, or hold it once CI shows a margin.

## Outside the brief's listed paths
The brief owns `engine.rs`, the writer's commit judgment calls, `props.rs`, `packs_gate.rs`, new pipeline tests and this file. The design needs small changes in the judgment path beside them, each only what it states:
- `crates/filter/src/matcher.rs`: `Doc`'s text caches split out so the Adult reading shares them (`Doc::as_adult`, `adult_tags`), the date leaves noting `read_time`, `Compiled::reads_tags`, `Doc::candidates_pair`, and `Reading: Default`.
- `crates/filter/src/prefilter.rs`: `RuleList::candidates_pair` and the per-rule `reads_tags` the index keeps. `RuleList::candidates` is unchanged.
- `crates/filter/src/core.rs`: `patterns` split into `pattern_signals` and `judge_signals`, the same code; `pattern_hits_read` folded into them.
- `crates/filter/src/packs/mod.rs`: `Ctx::into_text`, `Ctx::with_text` and `Ctx::for_state` (for SP-3 and SP-4 at commit; debug builds check it against `Ctx::new`).
- `crates/filter/src/reclassify.rs`: `judge_source_adult` through `Engine::judge_classified_with`, `judge_source_adult_with`, and the old one kept as `judge_source_adult_unshared`, the oracle.
- `crates/filter/src/lib.rs`: exports `Trail`.
- `crates/pipeline/src/worker.rs` and `source_adult.rs`: `Results` carries the trail, and `judge_kept` keeps it. `writer.rs`: the re-judge keeps one, the commit's second judgment reads it, and the cache's record and a test's carry an empty one.
- `runner/slices.yaml` and `runner/dashboard.yaml`: this slice's entry and placement.
