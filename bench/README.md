# Benchmarks and quality gates

`lodestar-bench` holds the synthetic dataset generator and the bins CI
gates on (architecture §3, §8, §17). Run each in release:

| Bin | What it checks | CI job |
|---|---|---|
| `size-report` | Bytes per torrent against the §3 size budget | `size budget (§3)` |
| `search-bench` | Search latency p95 per query class, count estimates | `search latency (§3)` |
| `relevance` | Ranking quality: NDCG@10, MRR, recall@50 | `relevance (§8)` |
| `reporting-overhead` (an example: it needs the API crate, a dev-dependency) | Insights' cost: search p95 and ingest with Insights open and the digest running, rollup bytes per year | `reporting overhead (§3)`, nightly |

## Reporting overhead

```sh
cargo run --release -p lodestar-bench --example reporting-overhead -- --gate   # as the nightly runs it
```

About 17 minutes: a 100,000-torrent corpus with a year of rollups, then
six base/loaded pairs of 30 s search phases and six of 10,000-torrent
ingest phases. The load is an open Insights tab at the UI's rate (the
overview and the Fetch page refetched every 30 s) plus a digest preview
each round. The operative gate is a round's share of the 30 s period (at most 1%); the timed deltas are mostly run-to-run noise at that rate and gate only gross regressions. The gates, the noise allowance and the measured numbers are
in `docs/decisions/2026-11-01-G-m5a.md`. It is too long for a pull
request's CI, so it runs nightly and is a local gate for changes to
Insights, the digest or the store reader.

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

## Fixture backend

`lodestar serve --fixture [DIR]` runs the real server over a data
directory with known contents (build plan §1.3 seam 2). Screens, e2e
tests, the v0.5 exit and the user-doc screenshots use it instead of the
W6 mock. `lodestar_bench::fixture` builds it; the server only adds
`auth.json` and `seal.key`, as on any first start. It's in the
binary through the default-on `fixture` feature of `lodestar` (about
690 KB stripped); `--no-default-features` builds leave it out.

```sh
# A temporary fixture, removed when the server stops cleanly:
LODESTAR_FIXTURE_PASSWORD='correct horse battery' lodestar serve --fixture

# A kept one: built on the first run, reused after that.
lodestar serve --fixture /tmp/lodestar-fixture
```

- **Offline.** The DHT is off and locked off, and blocklist files,
  subscriptions and their refresher are off. With no DHT there's no peer
  finder, so nothing is fetched.
- **Local.** It binds `127.0.0.1:8090` unless `--config` or
  `LODESTAR_HTTP_BIND` names another address, and warns if that address
  isn't loopback. `data/config.toml` and `$LODESTAR_DATA_DIR` are never
  read.
- **Safe with `DIR`.** It builds in an empty or new folder and reuses
  one that holds a `fixture.json` of the current format. It refuses
  anything else with exit status 3 and writes nothing, so it can't touch
  a real install. Delete the folder to rebuild after `FORMAT` changes. A reused folder
  keeps what it was built with, even after the generator or classifier
  changes; rebuild for screenshots.
- **Sign-in.** The setup token is printed as on any first start. With
  `LODESTAR_FIXTURE_PASSWORD` set, that password claims the instance
  first. There's no built-in password.

### What it holds

- **About 200 hand-specified torrents.** Twenty films in four versions
  each (The Lantern Keeper has six, for siblings and Compare), series
  packs and episodes, albums, ebooks, audiobooks, software, games, photo
  sets and comics.
- **2,000 generated torrents.** These come from `synth::Generator` with
  a fixed seed. The generator's real release-group and tracker names are
  replaced with invented ones (`fixture::sanitize`).
- **Six imports** from a bitmagnet source.
- **Edge cases, each a known count:** Adult (detected, and one by an Is
  adult correction), filter-hidden, private-source, pure v2 and hybrid
  with padding files, a GBK name, a 1,200-file pack, and two ambiguous
  names resolved by their files.
- **Swarm health:** never scraped, scraped zero, every seeders and
  trackers bucket, DHT-only, a suspect swarm, an unanswered first scrape
  and 10–14 days of history.
- **Insights over 35 days:** `rollup_hour`, `cohort_hour` (final and
  incomplete cohorts, every loss reason), `events` (start, stop,
  intensity, pause, resume) and `index_watermark_hour`.
- **User data:** three saved searches (alerting, RSS-only, paused) with
  read and unread alerts, user items and two ordered collections. There
  are no API keys.

Every name is invented. Every timestamp is an offset from the anchor,
the build time rounded down to the hour. The crawler is quiet for six
hours after the 24 h, 72 h and 7 d marks, so "24 h", "7 d",
`discovered:<7d` and cohort finality match the manifest for six hours
after the build.

### The manifest

`fixture.json` sits beside the database. The builder computes it from
its own specification, never by asking the index or the API, so tests
can compare the API against it. Content types come from the stored
classification, which is the index's input. It holds:

- `format`, `anchor`, `seed`;
- `counts`: total, stored, searchable, visible, and each edge case;
- `queries`: totals, plus every `type` and `ext` facet value, for
  browse, `lantern`, `type:movie` and `ext:mkv`, each with Adult hidden
  and shown, and for browse with `hidden=true`. Facets are disjunctive:
  each ignores the query's own clause on it;
- `discover`: Newest per tab, with its total and first five hashes;
- `swarm`: never-scraped and populated counts, and the buckets over
  browse;
- `cohorts`: per hour and source, the stages, losses, `in_progress`
  and `final`;
- `rollups`, `events`, `saved_searches`, `user_items`, `collections`;
- `torrents`: every hand-specified torrent with its id, hash, name,
  size, files, type, flags and swarm values (`null` when never scraped).

`bench/tests/fixture.rs` checks the builder: two builds give the same
data, the counts match `COUNT(*)`, and every edge case is there.
`crates/lodestar/tests/fixture.rs` checks the real binary against the
manifest.

### Pointing the web e2e at it

Build the UI (`npm run build` in `web/`), then `cargo build -p
lodestar`, which embeds `web/build`. Then start `lodestar serve
--fixture` on a free port with `LODESTAR_HTTP_BIND` and
`LODESTAR_FIXTURE_PASSWORD`. Make that Playwright's `webServer.command`,
with `baseURL` the same address, and don't install the OpenAPI mock for
those projects. Tests read expected numbers and hashes from the
manifest, never from the code.
