# X-facet-group-case decisions (#PR)

Runner brief, 2026-11-21Z, from the "Release-group case" follow-up in `docs/decisions/2026-11-21-X-labels.md`. Architecture §8 (facets), §10.4; budgets §3 and §10.1.

## The problem
- **The index keeps tag terms lower-cased** (`crates/search/src/doc.rs`, `DocEnrichment::new`), and the facets read those terms back. So the group facet listed `rarbg`, and no client formatter can recover "RARBG".
- **Matching stays on the lower-cased term.** `group:rarbg` and `group:RARBG` find the same torrents, and a facet click still sends the value.

## The option chosen: the stored names, read when the facets are computed
- **Where the spelling comes from.** The torrent index stores `name` (schema.rs), and the release parser takes `group` from the name's own tokens (`release-parser` `dash_group` and `bracket_group`). So the stored name nearly always holds the group as written.
- **How (`facets::group_labels`).** For each `group` value the facet lists:
  - it takes up to 8 of the term's documents in the snapshot, spread evenly over the whole posting list: across segments by document frequency, then across each segment's doc ids, skipping deleted documents;
  - it reads each one's stored name and finds the value there as a word of its own, the last such, else the last match at all (`spelling_in`);
  - the spelling with the most votes wins, ties by byte order, so "RARBG" comes before "Rarbg" (`choose_spelling`);
  - the result is `label` on the facet value. It's set only when it differs from `value`, and only for `group`.
- **Why not the alternatives.**
  - **Display terms written at index time** (`gdisp:<Original>` in the `tag` field) would add several bytes per torrent. The torrent index is at about 496 B against its 512 B budget (GS-2b). `tag` isn't a field the population contract tracks, so existing indexes would also need a new tracked entry and a backfill walk.
  - **A side table per segment** needs its own lifecycle across merges, rebuilds and deletes, because Tantivy has no custom segment component that follows a merge. It's the most code and the most risk, and it gains nothing over the stored names.
- **No `SCHEMA_VERSION` bump, no backfill, 0 index bytes.** Existing indexes show labels at once.
- **The same snapshot gives the same label in every search:** the samples come from the term's postings, not the search's matches. Facets stay a pure function of (snapshot, AST, parameters), so the result cache (B5c) still holds them.

## Resisting junk and hostile names
- **One vote per torrent, over the whole posting list.** No single record's latest write decides. A spelling has to be on a large share of a group's torrents to win.
- **Tests:**
  - one odd record among 40 doesn't take over;
  - a burst of 10 odd records written last doesn't either;
  - deleting the majority spelling's torrents moves the label to the live one.
- **What may show (runner call, §2.26).** Names are untrusted, so a spelling shows only when it passes `fit_label`:
  - each char lower-cases to exactly the value's char in its place, so it has the same chars and the same count, only re-cased;
  - it's NFKC-stable (`label == nfkc(label)`).
  - So compatibility characters never show: the Kelvin sign (U+212A, which lower-cases to `k`), the Angstrom sign (U+212B), ligatures and full-width forms. Nor does a char whose lower case is two chars (`İ`).
  - A name with no fit spelling gives no label, and the value shows as sent.
  - **A known gap:** a group ending in a Greek capital sigma gets no label. The index lower-cases it to a final `ς`, and the char-for-char rule lower-cases it to `σ`.

## The read cap (runner call, §2.26)
- **At most 256 stored names per request, whatever the limit; at least 3 per value, at most 8.**
  - At the default limit of 8, each value reads up to 8 names (64 reads).
  - At limit 100, each value reads 3, and values are labelled in list order (the top counts first) until the reads run out: the top 85 get labels, the rest show as sent.
  - Tested at both limits (`a_long_list_reads_fewer_names_per_group`).

## The file-tree exception
- **An ambiguous name resolved from the file tree** (`classify` `effective.rs`) can take its group from a file or folder name the torrent's name doesn't hold. Then no sampled name holds it, and the value shows as sent. No extra work: the fallback covers it (runner).

## API and web
- **`FacetValue.label`** (`crates/api/src/dto/facets.rs`): optional, absent unless the server has a spelling that differs from `value`. A per-value field rather than a response-level map (runner). `openapi.json` and `schema.d.ts` are regenerated with the repo's tooling.
- **The facet rail:**
  - `groupLabel(value, label)` shows `label` when it's present, else the value;
  - `valueLabel` passes it through for `group` only;
  - `TermGroup` hands each value's `label` to `labelOf`, so "Find" matches the label or the code;
  - a click still sends `value` (`TermGroup.test.ts`).

## Budgets
- **Index size:** unchanged, 0 B per torrent.
- **`/facets` latency (§3, gate p95 ≤ 100 ms):** search-bench at 200,010 synthetic torrents, `--queries 1500`. Three runs each of main (6d117c17) and this branch, interleaved on one dataset. The median of the three runs' p95, in ms:

| Class | main, limit 8 | branch, limit 8 | main, group limit 100 | branch, group limit 100 |
|---|---|---|---|---|
| broad word | 6.19 | 6.17 | 6.60 | 6.04 |
| word + selections | 9.05 | 8.55 | 9.11 | 8.56 |
| browse everything | 35.78 | 27.03 | 29.28 | 32.73 |
| browse + selections | 27.19 | 21.38 | — | — |

  - The p50 of "broad word" is 2.06 ms on main and 1.99 ms on the branch.
  - The differences are within run-to-run noise: the same binary's p95 for "browse everything" ranged 32.0–36.6 ms on main. The largest rise, "browse" at group limit 100 (+3.5 ms), is under the runner's 5 ms line.
  - The group-limit-100 rows came from a local, uncommitted copy of `facets_path` that also runs `limit.group=100`; the committed bench is unchanged.
  - The reads land in the searcher's document-store block cache.
- **Web size:** see the PR for the size-limit numbers.
