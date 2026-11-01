# R-relevance decisions (#94)

Date: 2026-10-22. Slice: R-relevance. Spec: architecture §8 ("a relevance judgment suite … with an NDCG gate in CI"), §14 M8, §17; build plan P7.

## Decisions

1. **The `relevance` bin lives in `lodestar-bench`, not `crates/search`.** §17 names `cargo run -p search --bin relevance`; the brief puts it with the other gated bins, which already build stores and indexes and share the CI pattern. The command is `cargo run --release -p lodestar-bench --bin relevance -- --gate`. `crates/search` is unchanged.

2. **The corpus is hand-written fixtures, not the synthetic generator.** The generator's names draw on real group names and a shared word list, so its torrents would match judged queries without being judged. A fixed corpus of about 140 invented torrents keeps every match a judged or deliberately unjudged one. Unjudged results count as grade 0.

3. **Queries run as `/search` runs them.** That means a `Lease` and its first page of 50, with the API's default visibility: `without_hidden()` and `-type:adult`, joined with `lodestar_search::with_filters` exactly as `lodestar_api::search::base_query` joins them. Only the `-type:adult` clause is restated, which avoids adding `lodestar-api` as a dependency of the bench crate. Every query must match fewer than 50 torrents, so the page holds every result and the forbidden check sees them all; the bin stops with an error otherwise.

4. **Measures.**
   - **NDCG@10:** exponential gain, log2 discount, with the ideal taken from every judged torrent.
   - **MRR:** the first result graded 2 or more (1 when nothing is graded higher), within 50.
   - **Recall@50:** over grades 1–3.
   - A query with only `x` judgments scores NDCG 1 when nothing forbidden shows, else 0. It's left out of the MRR and recall means.

5. **The gate fails when:**
   - overall NDCG@10 falls more than 0.001 below the committed baseline. The suite is deterministic, so the tolerance only covers the baseline's 4-decimal rounding (runner review: 0.01 let about two whole queries fall to 0 unnoticed);
   - any `x` ("never shown") torrent appears in any query's results;
   - the corpus or judgments changed since the baseline was written. `baseline.txt` stores a hash of both, with line endings normalised.

   A forbidden result fails the gate on its own, whatever the average, because hidden Adult and negation are correctness rules, not ranking quality.

6. **Judgments describe the ideal ranking, not today's.** Two families score below 1 on purpose until phase 2 lands:
   - Swarm health: copies that differ only in seeders, with the dead copy inserted first, so ties don't flatter today's ranking. The bin writes their scrape results to the store, so the priors read them once they exist.
   - Junk: fakes with the real release's words.

   Hard cases the ranking misses today are kept too: "2" for "II", "season 2" for S02, "dlc" for "DLCs", and H.264 against x264.

7. **Determinism.**
   - The index is built by one writer thread and one commit, so it's one segment and ties break the same way every run.
   - The corpus clock is fixed, and nothing reads the real clock or the network.
   - Two runs print identical output.

## How to update the baseline

`cargo run --release -p lodestar-bench --bin relevance -- --write-baseline`, committed in the PR that causes the change. That PR states:

- **why the baseline moves:** a ranking change, or new or corrected judgments;
- **the before and after:** overall NDCG@10, MRR and recall@50, and the worst-10 list.

The rules:

- **A ranking improvement raises the baseline**, so later changes can't quietly give the gain back. The bin prints a WARN line when NDCG@10 beats the baseline by more than the tolerance.
- **A deliberate drop** needs its reason and the per-query losses in the PR, and the reviewer accepts it explicitly.
- **Judgments are written from the ideal,** each with its one-line rationale. Never edit a grade to match what the ranking returns.
- **Never raise the tolerance** to get a PR through. A change to it is a decision of its own, recorded here.

## Baseline at introduction (main's ranking)

| Measure   | Value        |
| --------- | ------------ |
| NDCG@10   | 0.9186       |
| MRR       | 0.9247       |
| Recall@50 | 0.9278       |
| Queries   | 189          |
| Corpus    | 142 torrents |
