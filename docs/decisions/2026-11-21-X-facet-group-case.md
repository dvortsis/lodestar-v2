# X-facet-group-case decisions (#1033)

Runner brief, 2026-11-21Z, from the "Release-group case" follow-up in `docs/decisions/2026-11-21-X-labels.md`. Architecture §8 (facets), §10.4; budgets §3 and §10.1.

## The problem
- **The index keeps tag terms lower-cased** (`crates/search/src/doc.rs`, `DocEnrichment::new`), and the facets read those terms back. So the group facet listed `rarbg`, and no client formatter can recover "RARBG".
- **Matching stays on the lower-cased term.** `group:rarbg` and `group:RARBG` find the same torrents, and a facet click still sends the value.

## The option chosen: the stored names, read when the facets are computed
- **Where the spelling comes from.** The torrent index stores `name` (schema.rs), and the release parser takes `group` from the name's own tokens (`release-parser` `dash_group` and `bracket_group`). So the stored name nearly always holds the group as written.
- **How (`facets::group_labels`).** For each `group` value the facet lists:
  - it takes up to 8 of the term's live documents in the snapshot, picked by **rank** along its posting lists (every segment's, in order), at the van der Corput points 1/2, 1/4, 3/4, 1/8, … of the term's documents (`sample_ranks`, `sample_docs`). A rank on a deleted document takes the next live one. The walk visits the postings up to the last rank; each torrent has one group, so all the listed values' walks together visit each group-tagged torrent at most once;
  - it reads each one's stored name and finds the value there as a word of its own, the last such (`spelling_in`). A match inside another word ("flux" in "INFLUX") doesn't vote. The name is lower-cased once, char for char, and searched in one pass, so the cost is linear in the name;
  - the spelling with the most votes wins, ties by byte order, so "RARBG" comes before "Rarbg" (`choose_spelling`);
  - the result is `label` on the facet value. It's set only when it differs from `value`, and only for `group`.
- **Why not the alternatives.**
  - **Display terms written at index time** (`gdisp:<Original>` in the `tag` field) would add several bytes per torrent. The torrent index is at about 496 B against its 512 B budget (GS-2b). `tag` isn't a field the population contract tracks, so existing indexes would also need a new tracked entry and a backfill walk.
  - **A side table per segment** needs its own lifecycle across merges, rebuilds and deletes, because Tantivy has no custom segment component that follows a merge. It's the most code and the most risk, and it gains nothing over the stored names.
- **No `SCHEMA_VERSION` bump, no backfill, 0 index bytes.** Existing indexes show labels at once.
- **The same snapshot gives the same label in every search at one list length:** the samples come from the term's postings, not the search's matches. Facets stay a pure function of (snapshot, AST, parameters), so the result cache (B5c) still holds them.
  - A long list ("Show more", limit 100) reads 3 names per value where the default reads 8, so a spelling can differ between the two. The 3 are the first of the 8 (the van der Corput order), so it differs only when those 3 disagree with the rest. Values past the read cap show as sent.

## Resisting junk and hostile names
- **One vote per torrent, spread by rank over the group's torrents.** No single record's latest write decides, and neither does where a record sits in the index. A spelling has to be on a large share of a group's torrents to win.
  - The first version spread samples over doc ids. It sampled only the record after a gap: 40 torrents of a group packed at low ids, 960 of another group, then one odd spelling, gave that spelling (self-review). Ranks fix it.
- **Tests:**
  - one odd record among 40 doesn't take over;
  - a burst of 10 odd records written last doesn't either;
  - a cluster, a gap of 960 other torrents, then 1 and then 6 odd records doesn't either;
  - deleting the majority spelling's torrents moves the label to the live one;
  - a property test: the samples for 3 are among those for 8, are spread over both halves, and are never closer than 1/16 of the term's documents.
- **Bounded work on untrusted names.** A value longer than 64 bytes gets no label: that's the query language's longest keyword value, and real groups are far shorter. Each name is scanned once, in one pass (a 16 KB name of near-misses is a test case). The cancel token is checked between values and every 4,096 postings of a walk.
- **Hidden torrents can vote.** The samples are the term's documents, so a torrent the always-on filters hide (Adult while hidden, filter rules) can be one. All it can change is the case of a value the facet already lists; the counts never include it (§8).
- **What may show (runner calls, §2.26).** Names are untrusted, so a spelling shows only when it passes `fit_label`:
  - **S1-20: labels limited to ASCII re-casing.** It may differ from the value only by ASCII letters in upper case (`a`–`z` as `A`–`Z`); every other char is exactly the value's. So a label never holds a char the value doesn't, other than a capital ASCII letter, and can't pass for another group's name: no lookalikes of another group. A non-ASCII letter keeps the value's form ("GRüPPE" for `grüppe`).
  - It's NFKC-stable (`label == nfkc(label)`), so compatibility characters never show: the Kelvin sign (U+212A), the Angstrom sign (U+212B), ligatures and full-width forms.
  - It has the value's char count, so no char whose lower case is two chars (`İ`).
  - A name with no fit spelling gives no label, and the value shows as sent.

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
- **The Builder still shows groups lower-cased.** Its value list (`assist/BuilderRow.svelte`, outside this slice) calls `valueLabel` without a label. It's a follow-up for the runner.
- **`/facets` latency (§3, gate p95 ≤ 100 ms):** accepted by the runner (§2.26), under its 5 ms line. Measured with search-bench at 200,000 synthetic torrents, `--queries 1500`, with the rank sampler. Three runs each of main (bdb189a3) and this branch, interleaved on one dataset. The median of the three runs' p95, in ms:

| Class | main, limit 8 | branch, limit 8 | main, group limit 100 | branch, group limit 100 |
|---|---|---|---|---|
| broad word | 6.57 | 6.28 | 6.44 | 6.01 |
| word + selections | 9.03 | 8.70 | 8.38 | 8.53 |
| browse everything | 35.03 | 33.28 | 31.96 | 33.30 |
| browse + selections | 25.29 | 25.36 | — | — |

  - The p50 of "broad word" is 2.17 ms on main and 2.02 ms on the branch.
  - The differences are within run-to-run noise: the same binary's p95 for "browse everything" ranged 30.9–35.5 ms on main. The largest rise, "browse" at group limit 100 (+1.3 ms), is under the runner's 5 ms line.
  - The first version, which sampled by doc id, measured the same within noise (largest rise +3.5 ms, the same class).
  - The group-limit-100 rows came from a local, uncommitted copy of `facets_path` that also runs `limit.group=100`; the committed bench is unchanged.
  - The name reads land in the searcher's document-store block cache; a rank walk visits at most the group's own postings, cheap beside a facet pass.
- **Web size (gzip; main 6d117c17 → branch):**
  - Facet rail (lazy): 18,825 → 18,848 B. The limit goes from 18.83 to 18.85 kB (runner, §2.26).
  - Builder (on demand): 8,430 → 8,441 B, from `valueLabel`'s optional `label` argument in `label-release`, which it shares with the rail. The limit goes from 8.43 to 8.45 kB (runner, §2.26).
  - Within their limits: Search route JS +2 B, Total first load +2 B, Route table +1 B, Compare route +6 B.
