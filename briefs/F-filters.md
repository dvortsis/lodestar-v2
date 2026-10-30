# F-filters: three content-filter semantics fixes (A3-1..3, release blocker, content safety)
**Model:** Opus. **Content-safety:** run `/security-review`. These restore architecture §20 as written (they tighten filtering); no owner decision is loosened. If any fix would loosen protection anywhere, stop and ask the runner.

## Base and PR timing (read first)
- Branch `slice/F-filters` from `origin/next` (the rewritten history that replaces `main`; owner-decisions §2.25 on `origin/claude/busy-allen-aptli2`). Never base on, merge or pull the old `origin/main`: its history is being replaced, and a PR head containing the old root `15620683` or old tip `2d5e761d` will be refused.
- Until the owner force-pushes `main` (the runner tells you), GitHub cannot open a PR between the two histories. Build, test, self-review and push your branch, then message the runner "ready, waiting for the swap". Open the PR to `main` only when the runner says the swap is done.
- The review report is on another branch: read it with `git show origin/fix/dht-cr0:<path>`.

**Spec:** `docs/code-review/codex-review-v0.5-area-3-results.md` (on fix/dht-cr0; includes reproduction source), architecture §20.6, §20.7, §20.9, §20.12, `docs/owner-decisions.md` §1.8.

## Fix
1. **Per-list exceptions** (`crates/filter/src/engine.rs` ~313, 362–367): `listed()` collapses matching lists with `max_by_key` before exceptions apply. Evaluate exceptions per matching list, then take the strongest surviving action. Keep prefetch conservative: `blocked_hash()` must not permit a fetch that another un-excepted list still blocks; after fetch, evaluate every applicable list.
2. **Stored reclassification hashes** (`crates/filter/src/reclassify.rs` ~154): `rule_says()` builds `Item { hashes: &[] }`. Populate with `row_hashes(row)` including the hybrid swarm hash. Positive and negated hash predicates, ingest parity, per-torrent correction precedence.
3. **Neutralize inside phrases and NEAR** (`crates/filter/src/matcher.rs` ~879/882): apply neutralization per required word in phrase and proximity evaluation, keeping positions; cover negation and `unless`.

## Tests
The four probes in the report as regressions through the full engine; property tests: (a) adding an exception for list X never changes the verdict contributed by any other list; (b) stored reclassification equals ingest classification for the same item and rules; (c) a neutralized word never supplies evidence in any expression form. Keep the prefilter/matcher equivalence tests green and re-run the filter budget bench (50 µs per torrent at 10k rules) to show no regression.

## Own
`crates/filter/**`, `docs/decisions/<date>-F-filters.md`. API or pipeline changes: describe in the PR, don't make them.

## Done when
fmt, clippy `-D warnings`, `cargo test --workspace` pass; filter bench within budget; `/security-review` clean; fresh-reviewer self-review done.
