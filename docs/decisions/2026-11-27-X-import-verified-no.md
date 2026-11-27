# X-import-verified-no decisions

Imported torrents are never indexed as verified. The Magnetico import test (10M torrents, on the test VM) found every import indexed as `verified:yes`: `DocProvenance::stored_torrent()` hard-coded `Verified::Yes` for every row in `torrents`. IMP-1 and IMP-2 store imports from another indexer's database as unverified (`2026-10-30-IMP-1.md`, `2026-10-30-IMP-2.md` "Quality"), so that claim had nothing behind it. Decided by the runner under `docs/owner-decisions.md` §2.26 and §2.28, flagged to the owner under §2.19.

## A third `verified` value (amends architecture §18.6)
- **`verified:` takes `yes`, `no` and `imported`.**
  - `yes`: the info dict was hashed to the key here, by a network fetch or with a full `.torrent` (a torrent cache's, a refetch's).
  - `no`: the unverified tier (claims only). Hidden by default, as before.
  - `imported`: a stored torrent whose metadata came from another indexer's database and was never hashed here. **Shown by default**, as §18.5 has imports land in `torrents`, visible, and not in the hidden tier. Indexing them `no` would have hidden a whole imported library behind "Show unverified".
- **The hidden-by-default clause is now `verified:yes,imported`** (`Verified::SHOWN`, `Node::without_unverified`). It still hides only `verified:no`.
- **When a network fetch verifies an import, it's `yes` at its next index write.** `upgrade_partial` writes the import record as verified and logs the torrent for reindexing (IMP-1). IMP-06's promotion will do the same.

## Read from the row, never assumed (`doc.rs`, `index.rs`)
- **`stored_verified(flags, info_size, record)` decides it,** with no default:
  - not `IMPORTED`: `yes` (the crawler's write path, unchanged and free);
  - `IMPORTED` with an import record: `yes` only when the record says `verified`; `unknown`, `unverified` and `conflicting` are `imported`;
  - `IMPORTED` with no record read: IMP-1's read rule. An info dict (`info_size` > 0) is `yes`, none is `imported`.
- **`DocProvenance::stored_torrent(row)` applies the read rule from the row alone,** so `TorrentDoc::new` can't come out `yes` by omission.
- **The indexer's loader (`index.rs` `torrent_doc`) reads the import record** with `Store::import_quality`, for `IMPORTED` rows only. That's one primary-key lookup per import, at index time; crawler rows cost nothing. So an adapter that one day stores an info dict it didn't verify is still `imported`, because its record says `unverified`. No store migration and no store change.
- **The flag combinations are tested as a table** (`tests/import_verified.rs`): imported or not, five other flag sets, three `info_size` values, and no record or each of the four verifications.

## Compilation (`compile.rs`; refines `2026-11-04-SRC-3.md` "Complete while backfilling")
- **Any `verified:` list that takes `yes` is "everything but the values it leaves out",** like `network:` with `bt`. So it stays one flat intersection of `MustNot` terms, with no score change and no new driving clause.
  - `verified:yes` leaves out `no` and `imported`: two `MustNot` terms, where SRC-3 had one.
  - `verified:yes,imported` (the default) leaves out `no`: the same single term as before, so a default search costs what it did.
  - `verified:yes,no` leaves out `imported`.
  - `verified:yes,imported,no` is everything.
- **A list without `yes` matches its own terms** (`verified:imported`, `verified:no`).
- **The Verified facet keeps its one-small-pass shortcut** for any single top-level `verified:` clause that takes `yes`. The shared pass also counts the Verified terms (`imported` among the selected values). The facet's own pass then counts only the values left out, usually `no`, a few documents. Before, it applied to `verified:yes` alone.

## Correcting existing indexes: the `verified` generation goes to 2 (`population.rs`)
- **Every existing index backfills `verified`** through the population walk (SB-6). That's the cheapest correct way:
  - it's the existing path, with bounded batches, the cursor committed with the documents, and a resume across restarts;
  - a walk over imported ids only would need a new store helper (`crates/store` isn't this slice's) and a second resume path, to save rewriting the crawler's documents once;
  - a full rebuild would take the index out of service.
- **While the walk runs, a document it hasn't reached says `yes`, even for an import.**
  - The default clause, `verified:no`, and any list with both `yes` and `imported` (or neither) are still exact.
  - A clause with one of the two and not the other (`verified:yes`, `verified:imported`) can be off until the walk finishes. `Node::population_fields` now names `verified` for such a clause, so the search's "fields incomplete" notice says so.
  - The Verified facet's `partial` flag stays for `source` only. During the walk its `imported` count is low and its `yes` count high by the same amount.

## The API and the UI (runner's grant, 2026-11-27)
- **A search hit is honest too.** An import returns `verified: false` and `imported: true`; a hashed row `verified: true` with no `imported`. Both come from the same rule as the index (`api/search.rs` `is_import`, through `stored_verified` and `Store::import_quality`), so an API client never reads `verified: true` for an import.
- **`AstVerified` gains `imported`,** and the OpenAPI document and `schema.d.ts` are regenerated.
- **Consumers of the hit's `verified`:** only the web result row and card read it. Torznab, saved-search alerts (the percolator), live search and Discover use the default clause, `verified:yes,imported`, so imports behave there exactly as before.
- **The badge:** `hit.imported` shows "Imported" in place of "Unverified", never both, with the help text "From another indexer's database; the file list wasn't checked against the torrent's hash." It's a variant of the Unverified badge with the same icon in the neutral colour, so no new icon enters the size-limited chunks.
- **"Show unverified" sends `verified:yes,imported,no`,** so imports stay visible beside unverified items. An older `verified:yes,no` URL still reads as shown, and switching the toggle replaces it with all three values.
- **Facet golden responses** (`api/tests/golden/facets_*.txt`) now list `imported` as selected, with its count, because it's part of the default selection.
- **Not changed here:** the torrent detail's `provenance.verified` (`api/dto/torrents.rs`) is still always `true`. The UI doesn't show it. It's left to a follow-up slice the runner places.

## The query language and its docs
- `verified:imported` parses, `imported` joins the catalogue's enum values, and the parse error names all three values.
- `docs/user/05-use-lodestar.md` §5.1 gains one line for `verified:imported`.
