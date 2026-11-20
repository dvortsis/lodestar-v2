# CR-LEDGER: the lookup ledger (counters only)

**Model:** Opus (DHT actor, concurrency). **Branch:** `slice/CR-LEDGER`. **PR title:** `CR-LEDGER: <title>`.

## Why
The owner adopted r5 "Path to 100" on 2026-11-20 (PR #970: `docs/build-plan.md` §0.5 "Path to 100", `docs/decisions/2026-11-20-path-to-100.md`, `docs/reviews/2026-11-20-path-to-100-review.md`). CR-0's counters put the crawler's limit in lookup cost: 32 slots held about 24 s each (`W`), up to about 59 DHT queries per lookup (`q`), and 0.16 stored per admitted lookup (`y`). Those numbers are derived, not read. CR-LEDGER measures them directly, and every later stage (CR-LOOKUP-END, CR-HINT-FIRST, CR-1c, the sampler split) is judged and mechanism-checked from its series. If #970 isn't merged when you start, read those files on its branch `docs/dht-throughput-plans-20261119`.

**It's first on the critical path.** Land it before stage 1's host build (CR-AB-ADM) freezes if possible, so it's identical in both arms. Stage 1 doesn't wait for it.

## What to build
Counters and histograms only. **No behaviour change**: with this PR merged, every packet, timer and decision is the same as before.
1. **Slot residence by end reason**: total seconds and count of lookups ending, labelled by why they ended (values found and closest set answered, closest set answered without values, deadline, query cap, cancelled, shutdown — use the reasons the code actually has, named in an enum).
2. **Queries by purpose**: DHT queries sent, labelled by purpose (peer lookup, maintenance/refresh, sample_infohashes, hint, announce, ping — again from the code's real purposes).
3. **Time to first values**: a histogram (fixed buckets in a const) of seconds from admission to the first `values` reply, and the count of lookups that never got values.
4. **Values per lookup**: a histogram of how many peer values each finished lookup returned (bounded buckets).
5. **Hint outcome**: for hinted lookups (CR-1a-H's existing path), counts of answered-with-values, answered-without, unanswered, and guarded-skip.
6. **Slot occupancy**: a gauge of occupied lookup slots and the configured cap, so `L` is read, not inferred.
7. Keep `dht_peer_lookup_seconds_total` and `dht_peer_lookups_answered_total` exactly as they are.
8. Rows in `docs/metrics.md` for each new series, with the exact name, type, labels and meaning, and a short note on how `W = residence_seconds / lookups_ended`, `q = peer_lookup_queries / lookups_ended` and `y = stored / lookups_ended` come out of them.

## Bounds (not loosened)
- **Labels are fixed small enums.** Never an info-hash, IP, port, node id or any other per-peer value as a label (cardinality and privacy).
- No new allocation per packet beyond atomic increments; no locks on the hot path beyond what exists.
- The actor's query rate, outstanding cap, `LOOKUP_DEADLINE` (25 s), the 64-query cap and `ALPHA` don't change.

## Tests
- Paused-clock unit tests against the real actor (existing `testing.rs` / `hint_tests.rs` patterns): each end reason increments exactly once per lookup on every path (values, closest set, deadline, cap, cancel, shutdown); late and duplicate replies don't double count; queries-by-purpose sums to the existing total sent counter.
- A test that the ledger's identities hold on a scripted run (residence total equals the sum of per-lookup lifetimes on the paused clock).
- CR-SIM goldens stay byte-identical (`crates/crawler-sim`): run the sim suite and show it.
- Metrics render test in `crates/lodestar` for the new names.
- No fixed sleeps; condition waits need deadlines of at least 10 s.

## Rules
- Owns: `crates/dht/src/node/mod.rs`, `crates/dht/src/node/actor.rs`, `crates/dht/src/node/lookup.rs` (counting hooks only), `crates/dht/src/stats.rs` or wherever `Stats` lives, `crates/lodestar/src/metrics.rs`, `docs/metrics.md`, and `docs/decisions/2026-11-20-CR-LEDGER.md`. Ask the runner before touching anything else. **Not `docs/soak/tools/**`**: the collector side is CR-COL-5 (running in parallel); give it your exact metric names by messaging the runner as soon as they're fixed.
- Running alongside you: X-crawler-sim-calibrate (`crates/crawler-sim/**`), CR-COL-5 (`docs/soak/tools/**`), GS-3 (search/api). Don't edit their files.
- Before the PR: fmt, clippy `-D warnings` (Linux and `x86_64-pc-windows-gnu`), `cargo test -p lodestar-dht -p lodestar -p lodestar-crawler-sim` (use the real package names).
- Your `runner/slices.yaml` entry comes with #970; add nothing else there.
- ≲ 800 changed lines.
- Use the steward skill. Report `ready <sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) when CI is green.
