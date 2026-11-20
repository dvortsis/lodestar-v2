# Runner: placement after X-ui-plan-r5 (#996)

The runner's changes to `runner/slices.yaml`, following what #996 wrote into `docs/architecture.md` §10.

- **X-alert-noun owns the search messages.** The Search toolbar's "Save search" label lives in the `search_toolbar_save*` keys of `web/src/lib/features/search/messages/`. No slice owned them, so "Create alert everywhere" would have left the toolbar unchanged. X-alert-noun now owns that folder and runs after X-search-toolbar, which edits the same files. The two never run at once, and neither does X-alert-noun with X-zero-rescue, RX-2 or X-units-typed, which edit the same en and zh files (#997's review).
- **X-settings-find and X-client-handoff wait for X-ui-plan-r5.** They build rules #996 wrote: Settings "Start here" (§10.10.3) and the `o` guard (§10.13). X-help and X-shell-home already depended on it.
- **X-units-typed follows §10.12's two readings.**
  - The stored-text reading, where `KB/MB/GB/TB` are 1,024-based, stays the parser's default. That default is the runner's refinement of §10.12, flagged under §2.19. Filter rules and every other stored caller keep it with no code change and no migration.
  - Only the query bar, typed facet values, natural phrasing and `/search?q=` opt in to the typed-unit reading, at the API. This adds `crates/api/src/search.rs` to the slice's owns. Its `request_ast` also serves facets, export, live search and saved-search previews, so they read the same set as search. `size:` and `largest:` take typed units; `piece:` stays 1,024-based.
  - The chip's echo of the typed text is built in `web/src/lib/features/search/query/clauses.ts`, which this slice now also owns. It never runs with X-zero-rescue, which owns that file too.
  - The printer writes only bytes or IEC units, so a rollback reads stored text the same way.
  - The slice gets `/security-review`, because §20 filter rules use the query language.
  - It also gets a dependency on X-ui-plan-r5.
  - Making the safe reading the default means a stored caller that is missed fails toward the old, unchanged meaning.

Flagged to the owner under §2.19 with #996: "GB" typed in a search means 1,000³ bytes, and stored rules keep their match sets.
