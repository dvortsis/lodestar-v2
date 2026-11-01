# F-suggest-adult-field: `/suggest` offers nothing Adult while Adult is hidden

**Date:** 2026-11-01 · **Slice:** F-suggest-adult-field · **Decided by:** the runner (content-safety tightening, §20.9)

While "Show adult content" is off, `/suggest` offers no `adult:` field, no
`adult:` or `type:adult` value, and no chip or did-you-mean that writes an
Adult clause. With it on, nothing changes. Only tightens.

| Path | Before | Now |
|---|---|---|
| Field names (`ad` → `adult:`) | offered | **changed:** left out unless Adult is shown |
| Did you mean, near field (`adul:vr` → `adult:vr`, `typ:adult` → `type:adult`) | offered (no count, no check) | **changed:** skipped when the rewrite has an Adult clause (either sign) |
| Values (`adult:…`, `type:ad…`) | already held: each value is counted through the base query, which ANDs `-type:adult`, so Adult values count 0 and are dropped | unchanged |
| Chips | already held: `chip_nodes` never makes `type:adult`, and no release-parser key is Adult | unchanged |
| Did you mean, fuzzy words | already held: a correction is offered only if `exists` finds a visible torrent through the base query | unchanged |
| Titles | already held: per-class FST counts, checked against the index under the base query | unchanged |
