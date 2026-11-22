# X-units-typed decisions

Builds the rule behind U12 (`docs/decisions/2026-11-20-X-ui-plan-r5.md`, architecture §10.12): sizes the user types now mean what was typed, and stored query text keeps its meaning. The rulings below are the runner's (2026-11-22Z, under `docs/owner-decisions.md` §2.26), flagged to the owner under §2.19. None of them loosens stored-rule safety.

## Supersedes

- **`docs/decisions/2026-11-01-FX-6-I9.md`'s "Every parser version reads both spellings as 1024-based", for the typed-unit reading only.** Text the user types now (the bar, typed facet values, natural phrasing, `/search?q=`) reads `KB`/`MB`/`GB`/`TB` in 1,000s on `size:` and `largest:`. Stored text, and every stored caller, keeps FX-6-I9's rule unchanged, as r5 and architecture §10.12 scope it.

## The parser

- **Two readings, passed explicitly on each call.** `query::Units` has two values, `Stored` (the default) and `Typed`. `parse(text)` is `parse_with(text, Units::Stored)`, so every existing caller keeps its reading, and there's no global.
  - `Typed` changes only `KB`/`MB`/`GB`/`TB` on `size:` and `largest:`, to 1,000-based.
  - `KiB`…`TiB`, plain bytes, single letters and every other field read the same under both readings.
  - `piece:` stays 1,024-based, because piece lengths are powers of two (§10.12).
- **Single letters (`1.5G`) stay 1,024-based under both readings.** r5 says "Single-letter forms (`1.5G`) follow the same rule", but only in the bullet on canonical stored text. It doesn't say they take the typed reading, so per the runner's ruling they don't. Natural phrasing matches: `10g` is 10 GiB, `10 gb` and "10 gigs" are 10 × 1000³.
- **The stored path's code is unchanged in effect.** `parse_bytes` now hands its arithmetic to `scaled`. For a decimal multiplier, `scaled` divides exactly instead of multiplying first, so `1.123456789TB` reads under `Typed` instead of overflowing. A binary multiplier is never divisible by a power of ten, so the stored path runs the old arithmetic. A frozen copy of the old `parse_bytes` in the tests pins this.

## Who reads what

- **Typed:** `request_ast`'s `q`, which carries `/search` (GET and POST), `/facets`, `/export`, live search and saved-search create and edit. Also `/parse` and natural phrasing, including the syntax copied into a natural-phrasing proposal.
- **Stored:** everything else, including the filter rule loader (`crates/filter/src/load.rs` `rule_query`), the filters API's rule editor, packs and overrides, and `request_ast`'s check of a client `ast` (whose text is bytes or IEC, so the reading doesn't matter there).
  - Alerts percolate on the stored AST JSON and never re-parse text.
- **`/suggest` stays on the default reading (runner, Q2).** Its did-you-mean check counts the user's live query to decide whether there are "few" hits. Under the default reading, a typed `GB` bound counts about 7% wider, which moves only that threshold and nothing the user sees. This is a known drift.

## What's persisted is canonical

- **Anything written to storage is `Node::to_query()` output, in bytes or IEC units only** (runner's hard rule). Every release reads it the same way, so neither an upgrade nor a rollback changes a match set.
- **Saved searches and alerts now store `to_query()` of the AST, not the raw text sent** (runner, Q1). A typed `size:>10GB` is stored as `size:>9765625KiB`, with the AST in bytes.
  - A query that fits the 2,048-byte limit as typed but not once written out is refused as `query_syntax` on `q`. Before, the store refused it with the wrong field.
- **A saved search's `q` in the API is `to_query_typed()` of the stored AST, every time.** It's what the Alerts pages show and what "Open in search" sends to `/search?q=`. This also fixes searches saved before typed units: raw text `size:>10GB` with a 10 GiB AST now echoes `size:>10GiB`, so the link finds what the alert finds. The client never round-trips that text into an edit form.
- **The printer tweaks are dropped.** The runner approved them: (a) an exclusive bound at multiples of 1,000, and (b) bytes instead of a long KiB count. Without them, `to_query()` prints every AST byte-for-byte as it did before this slice, so no golden file, stored rule, allow entry or dedupe key sees new text for an existing AST. The cost is cosmetic, in text nobody reads: a stored typed size can print as a long IEC count (`size:>9765625KiB`). The user sees `to_query_typed()`. §10.12's example "`size:<1GB` is stored as `size:<1000000000`" is therefore stored as `size:<=999999999`: the same bytes, the same set.

## The echo (R1, R2)

- **`Node::to_query_typed()` echoes decimal sizes** (runner, R1). On `size:` and `largest:`, it writes a bound in the largest decimal unit when that takes fewer significant digits than the IEC form (`10GB`, `1.5GB`, `700MB`), and the IEC form otherwise (`10GiB`; on a tie, as `4096000` = `4000KiB`). A one-sided bound one byte off a round size prints exclusive (`<1GB`, `>10GB`). Because typed text is never stored, it may also print `a..<b`. Its `a..<b` form can run a byte longer per bound than `to_query()` (`0..<1KiB` for `0..1023`; a decimal unit is never longer than the bytes or IEC text), so past 2,048 bytes it falls back to `to_query()`, which reads back the same; the fuzz target asserts it fits whenever the stored text does.
  - It's used only for `/parse`'s `canonical`, `/search`'s `canonical`, the zero-result recovery's `clause` and `canonical` (all of which go into the bar and the URL, which read as typed), natural phrasing's `canonical` and chips, and a saved search's `q`. `parse_with(Typed, to_query_typed(ast)) == ast` is a property test and a fuzz assertion.
  - `/facets`' `canonical` stays `to_query()`. It isn't in this slice's owns, and the web doesn't use it. It means the same set either way.
- **The chip mirrors that rule on the exact byte value** (runner, R2). The client has no per-clause source text, and the strip never reads query text. So `clauses.ts` echoes `size:`/`largest:` in the decimal unit by the same rule, with the query's `.` decimal point and without `formatBytes`. `1500MB` echoes as "1.5 GB", and a size that's also a whole IEC size echoes as IEC. A bound one byte off a round size of 1 MB or more echoes that size, the way `formatBytes` rounds `<16GiB` to 16 GiB. `piece:` and everything measured stay IEC (`docs/metrics.md`, "never SI").

## Tests

- **The stored rule `size:<1GB` keeps matching under 1 GiB:** its upper bound is 2^30 − 1, which covers a 1,000,000,500-byte item.
- **A rule saved after the change matches the same set under the previous parser:** `to_query()` of any typed query has no decimal unit, and it parses to the same AST under both readings. The old parser read bytes and IEC units exactly as `Stored` does (the frozen-copy test). Saved-search create with `size:>10GB` checks the same at the API.
- **Property tests** (`crates/search/tests/query_props.rs`) cover the size grammar under both readings for every unit spelling, typed round-trips through both printers, and typed byte ranges as fixed points. The `query` fuzz target asserts the same for both readings.
