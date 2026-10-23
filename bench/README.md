# Benchmarks and quality gates

`lodestar-bench` holds the synthetic dataset generator and the bins CI
gates on (architecture §3, §8, §17). Run each in release:

| Bin | What it checks | CI job |
|---|---|---|
| `size-report` | Bytes per torrent against the §3 size budget | `size budget (§3)` |
| `search-bench` | Search latency p95 per query class, count estimates | `search latency (§3)` |
| `relevance` | Ranking quality: NDCG@10, MRR, recall@50 | `relevance (§8)` |

## Relevance

```sh
cargo run --release -p lodestar-bench --bin relevance            # report
cargo run --release -p lodestar-bench --bin relevance -- --gate  # as CI runs it
cargo run --release -p lodestar-bench --bin relevance -- --all   # every query
```

The suite builds a store and index from a fixed corpus the way the
server does (insert, swarm scrape, enrichment, indexing), runs each
judged query through the path `/search` takes (a searcher lease and its
first page of 50, with filter-hidden torrents and Adult left out), and
scores the results. It takes a few seconds and reads nothing from the
network or the clock, so two runs print the same thing.

### The files

`bench/relevance/` holds:

- **`corpus.txt`:** about 140 fixture torrents in families that each
  test a hard case: release-name noise, seasons and episodes, years in
  titles, CJK, plurals and stems, compound tokens (`x264`, `WEB-DL`,
  `DD+5.1`), title against path matches, junk, swarm health, Adult
  beside mainstream titles that share its words, and hash-named folders.
  Every title, person, group and site is invented; no real torrent name
  is used. The format is in the file's header.
- **`judgments.txt`:** about 190 queries, each with a one-line rationale
  and grades: 3 exactly what's asked, 2 a right answer, 1 related, 0 not
  relevant, and `x` for a torrent that must never be shown (hidden
  Adult, a negated word, a field the query scoped away).
- **`baseline.txt`:** the committed figures the gate compares against,
  with a hash of the corpus and judgments.

Judgments describe the ideal ranking, not today's. The swarm-health and
junk families score below 1 until the phase-2 priors and junk penalty
land (§8); the worst-queries list shows what each family still lacks.

### The measures

- **NDCG@10:** gain `2^grade − 1`, discount `log2(rank + 1)`, divided by
  the ideal order of every judged torrent. A query with nothing to find
  (only `x` judgments) scores 1 if nothing forbidden shows, else 0.
- **MRR:** one over the rank of the first result graded 2 or 3 (or 1,
  when nothing is graded higher), within the top 50.
- **Recall@50:** the share of torrents graded 1–3 in the top 50.

Unjudged results count as 0. MRR and recall average over the queries
that have something to find.

### The gate

`--gate` fails when:

- overall NDCG@10 falls more than `--tolerance` (default 0.001, enough
  for the baseline's rounding) below the baseline;
- any `x` torrent is in any query's results (every query must match
  fewer than 50 torrents, so one page holds them all);
- the corpus or judgments changed since the baseline was written.

### Updating the baseline

When NDCG@10 beats the baseline by more than the tolerance, the run
prints a WARN line: raise the baseline in that PR.

Update it deliberately, in the PR that causes the change, and say why
in the PR description (`docs/decisions/2026-10-22-R-relevance.md`):

- **A ranking change that improves NDCG@10:** raise the baseline, so
  the gain is kept.
- **A ranking change that lowers it on purpose:** only with the reason,
  and the worst-queries list before and after, in the PR.
- **New or corrected judgments:** judge from the ideal, never from what
  the ranking returns today, then rewrite the baseline.

```sh
cargo run --release -p lodestar-bench --bin relevance -- --write-baseline
```
