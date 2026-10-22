# R-relevance: Relevance judgment set, `relevance` bin and NDCG@10 gate
**Model:** Opus (search quality measurement). **Branch:** `slice/R-relevance`.

**Spec:**
- `docs/architecture.md` §8 (ranking: phase 1 BM25, phase 2 bonuses and priors), §14 (M8: "relevance judgment set and `relevance` bin, with an NDCG@10 gate"), §17 (verification);
- `docs/build-plan.md` P7;
- `docs/query-language.md` (merged or open #82, for query shapes).
- The code:
  - `bench/` (the `search-bench` and `size-report` bins, the corpus generator);
  - `crates/search`, read only.

## Own
- `bench/src/bin/relevance.rs` (new), `bench/relevance/**` (judgments, queries, corpus spec), `bench/README.md` (a section).
- A CI job in `.github/workflows/ci.yml`: one new job gated on the `bench` output. Copy the pattern of the existing gates.
- **Don't edit** `crates/search` or the existing bench bins (B6 touched `search-bench`).

## Build
1. **A deterministic, license-clean judgment corpus:** synthetic or fixture torrent names covering the hard cases. That means release-name noise, seasons and episodes, years, CJK, plurals and stems, compound tokens (`x264`, `WEB-DL`), title vs path matches, junk-score demotion, swarm priors once M4's fields exist (null today), and Adult hidden.
2. **About 150 queries with graded judgments** (0–3), each with a one-line rationale. Real third-party torrent names are never used.
3. **The `relevance` bin** builds the index from the corpus, runs every query through the real search path and computes NDCG@10, MRR and recall@50 per query and overall. It reports the worst queries.
4. **The gate:** `--gate` fails if overall NDCG@10 drops below a committed baseline minus a tolerance (e.g. 0.01). Commit the baseline, and record a decisions file explaining how to update it deliberately.

## Common rules
- **Bar and precedence:** `runner/standards.md` is the bar. Read CLAUDE.md, `docs/owner-decisions.md`, and the architecture sections cited.
- **Decisions** go in a new `docs/decisions/<YYYY-MM-DD>-<slice-id>.md` (see `docs/decisions/README.md`), not in §16.
- **Before pushing,** merge `origin/main`; never rebase. Run every check CLAUDE.md lists for the files you touch.
- **Scope:** stay inside **Own**. Shared-file edits needed elsewhere go in the PR description.
- **PRs** stay at or under about 1,500 changed lines, or state the reason in the description.

## Done when
- The bin runs in CI under the `bench` output, and the gate passes on main's ranking.
- Workspace checks pass.
- The PR is titled "R-relevance: Judgment set, relevance bin and NDCG@10 gate", with the baseline numbers and the worst-10 list in the description.
