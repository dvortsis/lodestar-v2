# R-rc3-backport decisions

rc.3 is rc.2 (`6be95525`) plus F-filter-load-fail-closed (#357, merge `736a3028` on `main`), exactly (`docs/owner-decisions.md` §2.29). Branch `release/rc3-357`, cherry-picked with `-m 1`. The design is #357's own, in `2026-11-01-F-filter-load-fail-closed.md`, which this branch carries unchanged. This file records only how the port fits v0.5.1's code. No post-cut feature came in: no pending re-queue list, no held-set chunking, no new store API.

## Conflicts and how they were resolved
- **`crates/api/src/filters.rs`, removing a classification rule.** On `main`, #357 changed `requeue_text`, which is part of the pending re-queue machinery (`push_pending_requeue`, `drop_pending_requeue`, `class_gen`) that came after the cut. v0.5.1 removes the row and re-queues what a readable query matches; an unparseable query re-queued nothing. **Resolved** with v0.5.1's flow plus a smallest equivalent: `remove_class_rule` reads the row's Is adult flag from `class_rules()` before removing it, and a new private `requeue_text(text, adult, now)` re-queues a readable query as before, everything (`Node::All`) for an unreadable Is adult rule, and nothing for an unreadable Not adult rule. **Equivalent** to #357: the same three outcomes, picked by the same test (`parse` fails, and the `adult` flag). The pending-list bookkeeping is dropped because v0.5.1 doesn't have the list. Without a search index, v0.5.1 re-queues nothing now, for any rule (its existing behaviour), and `main` would leave a pending entry for a later start instead. Either way the torrents keep their Adult label until they're next re-enriched, which is stricter, never looser.
- **`crates/api/src/filters.rs`, updating a rule** applied cleanly: fixing a rule whose stored form was unreadable releases its stand-in's quarantine, through v0.5.1's existing `release_rules`.
- **`docs/metrics.md`:** v0.5.1's text, plus #357's additions: `rows?: string[]` and the `filter_rows_unreadable` kind in the `exceptions[]` row, the `rows` → `formatCount` unit, and the `filter_rows_unreadable{kind}` gauge section. `main`'s unrelated changes to the `previous` row (`new_searchable_imported`) stay out.
- **`web/.size-limit.mjs`:** v0.5.1's limits, unchanged. #357's only change there raised `main`'s `/t/[hash]` limit from 21.14 to 21.20 KB for build-SHA noise. v0.5.1's limit is 22.4 KB, and rc.3 measures 22,184 B, so there's nothing to change. The measured sizes are in the delta report.
- **Insights messages, en and zh:** v0.5.1's keys, plus #357's one new key `insights_overview_exc_filter_rows_unreadable`, with #357's text. `main`'s other zh keys and its "hashes" → "keys" wording stay out.

## Forced by v0.5.1's code (applied cleanly, didn't build)
- **`crates/pipeline/tests/filter_load.rs`** imports `lodestar_pipeline::{StoreFilter, load_filter}`, the shared start loader `main` added after the cut. v0.5.1 starts the engine inline in `Pipeline::start`: the CS-1 floor from `filter_state()`, then `Engine::load_rules` and `Engine::load_class_rules` over the stored rows, which are the same loaders #357 changed. **Adapted** to replay those steps with the same public calls. The assertions are #357's, unchanged.

## Added
- **`removing_an_unreadable_class_rule_requeues_by_kind`** (`crates/api/tests/filters.rs`) covers the adapted `requeue_text` with a search index: removing an unreadable Not adult rule queues nothing, and removing an unreadable Is adult rule queues every stored torrent. #357's tests don't cover that path on either branch.

## Not changed
- **No version bump.** v0.5.1's release candidates don't carry rc versions in `Cargo.toml` or `openapi.json`'s `info.version` (rc.2, #222, didn't), so rc.3 stays `0.5.1`. The rc is the tag.
- `openapi.json` and `schema.d.ts` were regenerated with `cargo run -p lodestar --bin openapi` and `npm run api:types`. They match #357's change byte for byte.
