# X-facet-group-case

Runner brief, 2026-11-21Z. Slice id `X-facet-group-case`, branch `slice/X-facet-group-case`, PR title `X-facet-group-case: <title>`. The plan entry is in `runner/slices.yaml` (records r8, `docs/decisions/2026-11-21-runner-records-r8.md`). Opus builds it: this is search (facets, the index), core work. A fresh Opus review follows.

## Why
Release groups show lower-cased in the facet rail ("rarbg", not "RARBG"). The index stores tag terms lower-cased (`crates/search/src/doc.rs` around 129 and 150) and facets read those terms back (`crates/search/src/facets.rs` around 1115). No client formatter can recover the case (`docs/decisions/2026-11-21-X-labels.md`, "Release-group case").

## What
1. **Investigate first, then tell me the plan before you build it.** Find where a display form of each group term could come from without changing what the index matches on:
   - data the index already stores (a stored field, the doc's original record, a per-segment side table);
   - a small display map maintained at index time, keyed by the lower-cased term, holding the most common original spelling (bounded: say how);
   - or computing it at facet time from the top documents per term.
   For each, give the cost against the search budgets (§3, §10.1, `bench/` where it exists) and whether it needs a `SCHEMA_VERSION` bump. **If every workable option needs a schema bump, stop and tell me**: it then rides the next bump, and this slice becomes that plan.
2. **Matching doesn't change.** Queries already lower-case values (`crates/search/src/query.rs` around 1101); `group:rarbg` and `group:RARBG` keep finding the same documents, and a facet click still sends the value that matches. If the display form can't simply replace `FacetValue.value`, add a separate display field to the facet value in the API dto (`crates/api/src/dto/facets.rs`), regenerate `openapi.json`/`schema.d.ts` with the repo's tooling, and show it in the facet rail (`web/src/lib/features/search/facets/**`, via `groupLabel` in `web/src/lib/format/label-release.ts`); tell me before taking those files.
3. **Ties and noise.** Two spellings of one group ("RARBG", "Rarbg"): pick deterministically (most frequent, then a stable tie-break), and test it. A spelling from a hostile or junk record can't take over a popular group's display; say how your choice resists that (for example, frequency over documents, not a single latest write).
4. **Tests.** Unit and property tests for the display choice; a facet test that shows "RARBG" for a mixed-case corpus; matching unchanged. Bench the facet path if the plan calls for it, and give before and after numbers.
5. **A decisions file** `docs/decisions/2026-11-21-X-facet-group-case.md`, heading with `(#<this PR>)`: the option chosen and why, the budget numbers, and any call you made (a runner call under §2.26; name it).

## Owns (refined after item 1)
- `crates/search/src/facets.rs`, `crates/search/src/doc.rs`, `crates/search/tests/**` (and `facets/tests.rs`)
- `docs/decisions/*-X-facet-group-case.md`; this slice's `runner/slices.yaml` entry only
- only with my OK after item 1: `crates/api/src/dto/facets.rs`, the generated API files, `web/src/lib/features/search/facets/**`, `web/src/lib/format/label-release.ts`

`openapi.json` is also owned by X-safety-undo-danger (#1027) until it merges, and X-insights-source-names later. If you need it, tell me and I'll sequence it.

## Bars
`runner/standards.md`; `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; property tests for anything that parses or chooses; web checks if you touch `web/`. No `unsafe`. Push once per round, and send "ready <pr> <sha>" when CI is green. Follow `.claude/skills/steward/SKILL.md`; the runner merges.
