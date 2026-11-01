# S-search-meta-web decisions

Runner brief, 2026-11-01, on #335 (B-search-meta). No owner decision changes.

## The Builder reads its fields from `GET /search/fields` (§8, §10.4, §20.9)
- **No client-side field list.** `FIELDS` and `OPERATORS` are gone. The field Select lists the server's names; operators come from each field's `operators` (`eq`→`is`, `gt`, `gte`, `lt`, `lte`; `range`, `any_of`, `phrase`… are value syntax the server parses); a field with `values` gets a Select of them instead of free text. A text-typed field still quotes a multi-word value; a `size` field's range stays a read-only row.
- **Adult.** The server leaves out `adult:` and `type:adult` while Adult is hidden. Nothing client-side names `adult`; a vitest scans the assist code and fails if it comes back.
- **Fetched each time the Builder opens, not cached for the session (runner refinement, performance).** Listening to the event stream for a `content` settings change or a `resync` pulled the events module into the Builder's chunk (+2.5 KB against a 5.69 KB limit that may not be raised). The call is two static tables on the server with no index read, so asking again per open is cheaper than the module and can never show a list from before the setting changed. The list is dropped while the answer is out.
- **Loading.** While the answer is out the panel shows no rows and no fields (just Close); a "Loading…" line cost ~50 B of a limit with 0.01 KB to spare. It is quick: two static tables.
- **Fixed value lists are a Select only for one listed value.** A row with several (`type:movie,tv`) or a `cat:`/`bytes:` share field (`cat:video>=50%`) stays a text input, and share fields offer only `is`. AST names `season`/`episode` show as the query's `s`/`e`.
- **Failure.** The Builder says "Couldn't load the list of fields." with Retry and shows no rows or fields; there is no fallback list.
- A row whose field isn't in the list (it can't happen from the server's own AST while the list is current) shows an empty field Select rather than inventing an entry.

## "≈" on estimated suggestion counts (§8 "Counts")
- A suggestion with `count_approx` shows `≈ 2,400` (same `formatCount` approx form as other totals). Screen readers get "about 2,400 torrents" through hidden text and the visible one is `aria-hidden`, since "≈" reads inconsistently. Exact counts are unchanged.

## Known gap, not changed here
- `/suggest` still offers the `adult:` field name while Adult is hidden (noted in B-search-meta); it is server-side (`suggest::field_names`) and out of this slice's paths.
