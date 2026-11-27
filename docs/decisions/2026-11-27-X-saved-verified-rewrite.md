# X-saved-verified-rewrite decisions

Saved searches and alerts made with Show unverified keep finding imports. Before X-import-verified-no (#1139), "Show unverified" sent `verified:yes,no`, and imports matched it only because they were indexed `yes`. Now they're `imported`, so a stored `verified:yes,no` would leave them out without notice (`2026-11-27-X-import-verified-no.md`, "Follow-up, a release blocker"). This slice rewrites those once, at start. Nothing released was affected. The runner decides it as a release blocker.

## What's rewritten
- **A `verified:` node whose values are exactly {yes, no}** (any order, repeats allowed) **becomes `verified:yes,imported,no`**, at any depth under `and`, `or` and `file:`. It meant "verified and unverified", and both then included imports.
- **Left alone:**
  - **a lone `verified:yes`.** The UI never wrote it: the toggle has only ever written `verified:yes,no` (064fadc45, ee2fb49fa) and now `yes,imported,no`, and its "off" keeps a `verified:yes` only when the user typed it. No server code builds a `verified:` node except the default `verified:yes,imported`. So a stored `verified:yes` was typed, and it now means "hashed here", which is the meaning #1139 intends.
  - `verified:no`, and any list that already has `imported`.
  - **anything under a negation (`-…`), as the brief says.** The only negation affected is `-verified:yes,no`. It matched nothing before #1139 and matches imports only now. The UI never wrote it. Asked of the runner (2026-11-27); flipping it is a one-line change.
- **Other stored queries can't hold one.**
  - API keys store scope bits only.
  - Feed tokens point at a saved search's id, so they follow its rewrite.
  - The percolator's promoted queue holds `reindex_log` seqs and ids.
  - Filter, classification and allow rules can't name `verified:` (`Node::mentions_provenance`, `crates/filter/src/load.rs`, `crates/api/src/filters.rs`).
  - Old URLs and bookmarks can't be rewritten. The toggle reads `verified:yes,no` as on, and switching it replaces the list with all three values (`toggle.test.ts`, "still reads an older verified:yes,no URL as shown").

## How
- **One transaction, with a done marker** (`Store::rewrite_saved_verified`). It reads the marker, rewrites the rows and writes `settings` key `saved_verified_rewrite_v1` in one `IMMEDIATE` transaction. A crash or failed write rolls back everything, marker included, so the next start runs it again whole; once the marker is there it never runs again. No schema migration.
- **Parsing stays out of the store.** The store takes a function from a stored AST to the new text and AST. `lodestar_search::saved_rewrite::rewrite_verified_yes_no` rewrites the `Node`, and the app (`services.rs`, `rewrite_saved_ast`) reads and writes the JSON, because `serde_json` is only a dev-dependency of the search crate.
- **The text is the AST's canonical `Node::to_query`.** A saved search made before typed units (X-units-typed) that holds its text as typed now holds the canonical text. That text reads back as the same AST.
- **Only `query` and `ast` change.** The watermark, `paused`, the channels, `updated_at` and the alerts stay as they were. So a rewritten search alerts only on torrents above its watermark and never re-fires old hits.
- **When it runs:** at start, where the API's user-data connection is opened. That's before the percolator's connection is opened or `percolator::spawn` runs, and before the API serves. It runs whether search is on or not.
- **Kept as they are, and counted:**
  - a row whose AST this build can't read (never dropped);
  - a row whose rewritten text would pass 2,048 bytes or whose AST would pass 64 KiB, the limits `create_saved_search` checks. Writing it would fail the table's check and stop every start.
- **One Info event, `search.saved_verified_rewritten`,** with the counts `rewritten`, `unparseable` and `too_long`, on the run that writes the marker. It has an en and zh name and help in the Logging settings. The search event-names chunk grows from 2.07 KB to 2.21 KB gzipped, under its 2.25 KB limit. No limit was raised.
