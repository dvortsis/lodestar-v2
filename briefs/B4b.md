# B4b: The SSE `torrents` count leaves out Adult while it's hidden
**Model:** Opus (core: search indexing, the SSE hub, content visibility). **Branch:** `slice/B4b`.

**Why:** B17's reviewer found that B4's `torrents` event counts every newly searchable torrent, Adult included. With "Show adult content" off (the default, owner decision §1.8.12), the UI's "N new" banner would count torrents the user can't see. §20.9 says Adult is left out entirely while hidden.

**Spec:**
- `runner/standards.md`.
- `docs/architecture.md` §20.9 and §10.4 (the "N new" banner).
- §16's B4 and B17 entries.
- The code:
  - `crates/search/src/live.rs` (`Committed`, the hook);
  - `crates/search/src/index.rs` (`catch_up_counted`);
  - `crates/api/src/events.rs` (`report`/`take`, `TorrentsEvent`);
  - `crates/api/src/dto/events.rs`;
  - `crates/api/src/search.rs` (`AdultVisibility`, `base_query`);
  - `crates/lodestar/src/services.rs` (how the hub and the visibility are wired).

## Own
- In `crates/search/src/live.rs`: `Committed` and the publish site.
- In `crates/search/src/index.rs`: `catch_up_counted` only. B6 (#71) touches 3 other lines of index.rs; if it merges first, merge `main`.
- `crates/api/src/events.rs`, `crates/api/src/dto/events.rs`.
- The services.rs wiring line(s).
- Tests in `crates/api/tests/events.rs` and `crates/search/tests/*` (new cases only).
- The regenerated `openapi.json` and `schema.d.ts`, and a §16 entry.
- **Don't edit:** `facets.rs`, `compile.rs`, or anything else in `crates/search` beyond the two items above.

## Build
1. **Counting.** `catch_up_counted` also returns how many of the newly added docs are Adult. That means the effective content type the index stores: the same `type:adult` tag that B17's `-type:adult` clause excludes, so the count and search agree by construction.
   - `Committed` gains `adult_added: u64`.
   - Reindexed docs still don't count.
2. **The hub.**
   - `report` accumulates both totals.
   - At `take`, the `torrents` event's `added` is `docs_added`, minus `adult_added` while Adult is hidden. Read the visibility live through the same `AdultVisibility` the API state holds; no second copy of the setting.
   - An event whose visible count is 0 because everything new was hidden Adult isn't sent, unless `max_id` must move on for clients. Pick one, justify it in §16, and test it.
   - **Check what the client does with `max_id`.** If it's used to fetch "new since", the search applies B17's clause anyway; say so.
3. **No leak through the DTO.** Don't add an `adult_added` field to the public event while hidden. Adding it at all is unnecessary; keep the public shape unless the UI needs it.

## Tests
- With Adult hidden, a commit of 3 non-adult and 2 adult docs sends `added = 3`.
- With Adult shown, the same commit sends `added = 5`.
- Toggle the setting between commits: each event uses the setting live at `take`.
- An all-Adult commit while hidden follows the rule chosen in Build 2.
- The search-crate test: `catch_up_counted`'s adult count matches the number of docs that `type:adult` finds.

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass after merging `origin/main`.
- `cargo run -p lodestar --bin openapi` + `cd web && npm run api:types` leave no diff.
- A PR titled "B4b: SSE torrents count leaves out hidden Adult" that states the empty-event rule and the `max_id` reasoning.
