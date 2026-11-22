# X-builder-group-case decisions (#1037)

Runner brief, 2026-11-21Z, after X-facet-group-case (#1033), which gave facet values a server display form (`FacetValue.label`, "RARBG" for `rarbg`) that the facet rail shows. The Builder's value options call `valueLabel(field, v, locale)` with no label (`web/src/lib/features/search/assist/BuilderRow.svelte`), so this slice checked whether a release group can show lower-cased there.

## Finding: no release-group value reaches the Builder's labelled value options (`valueLabel`)
- **`valueOpt` labels only a field's fixed list.** `valueOptions` in `BuilderRow.svelte` maps `spec.values`, and only when the field has one and isn't a `share`. Any other field is a free-text input.
- **`group` has no fixed list.** The search catalogue gives `TagKey::Group` the shape `(ValueType::Keyword, LIST, Vec::new())` (`crates/search/src/catalogue.rs`). `/search/fields` sends `values: None` for an empty list (`crates/api/src/dto/suggest.rs`), so the client sees `type: "keyword"` and no `values`.
- **Its field list comes only from `/search/fields`; its rows come from the search AST.** The bar's `/suggest` dropdown (`assist/suggest.ts`) and the facet data don't feed the value options.

## What does show lower-cased
- **A group row, its label and the query chips show the server's lower-cased form, as the bar does.**
  - A row's value comes from the server's AST (`assist/builder.ts`, `rowOf`: `node.any.join(',')`), and the parser lower-cases tag values (`crates/search/src/query.rs`, `tag_value`). So `group:RARBG` typed in the bar shows `rarbg` in the row's input.
  - A facet click sends the lower-cased value (`TermGroup.svelte`, `onvalue(v.value, …)`), so the row and its chip read `rarbg` too.
  - The row's read-only label and the query chips print the value as the AST holds it (`query/clauses.ts`, `tagValue`).
- **That's the query's text, not a display label,** so it's the same in the bar, the chips and the Builder.

## Decision (runner)
- **Record only, no code change in this slice.** The Builder's labelled value options never list a group.
- **The lower-cased rows and chips are accepted for now.** A follow-up slice, **X-group-chip-label**, shows the rail's label on group chips and Builder rows when one is known. The runner places it.
- **If `group` ever gains a fixed list,** the label has to come from the server alongside `values` in `SearchField`, under #1033's rule (S1-20: labels limited to ASCII re-casing). It's never re-derived on the client from a name.
- **No test is added** (runner): this slice changes no behaviour.
