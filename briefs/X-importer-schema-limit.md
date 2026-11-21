# X-importer-schema-limit

Runner brief, 2026-11-21Z. Slice id `X-importer-schema-limit`, branch `slice/X-importer-schema-limit`, PR title `X-importer-schema-limit: <title>`. The plan entry is in `runner/slices.yaml` (#1020). Opus builds it, and it gets `/security-review`: the Magnetico importer opens user-supplied databases.

## Why
From #1018's review: SQLite 3.54 adds `SQLITE_LIMIT_SCHEMA`, which caps the number of schema objects, and rusqlite 0.41 exposes it as `Limit::SQLITE_LIMIT_SCHEMA`. `harden()` (`crates/sources-importers/src/magnetico.rs`) doesn't set it yet, so a hostile file with a huge schema can make schema loading allocate a lot of memory before the progress-handler deadline stops it.

## What
1. **In `harden()`,** set `SQLITE_LIMIT_SCHEMA` next to the other limits, with a cap chosen from the largest real Magnetico schema (count its tables, indexes and triggers in the schema the importer expects, and any version differences the code handles) with a wide margin. Write the count and the margin in the decisions file. Check the semantics in the rusqlite 0.41 and SQLite 3.54.1 sources: what counts, when it's enforced (on schema load at open, or at first statement), and what error comes back.
2. **The error a user sees:** a file over the cap is refused as a finding or error the importer already names for a hostile or unreadable file, never a panic, and never a message that echoes file content. Reuse the existing error path.
3. **Tests:**
   - a generated database just over the cap is refused, with the error above;
   - one at the cap opens and imports;
   - a real-shaped Magnetico file is unaffected.
   Generate them in the test; don't commit binary fixtures. Watch the test's run time.
4. **A decisions file** `docs/decisions/2026-11-21-X-importer-schema-limit.md`, heading with `(#<this PR>)`.

## Owns
- `crates/sources-importers/src/magnetico.rs` (and its in-file tests)
- the decisions file, and this slice's `slices.yaml` entry

Send me the list before you edit anything else.

## Bars
`runner/standards.md`. `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar-sources-importers`, and the workspace before review. Run /security-review yourself before you send "ready". Push once per round, and send "ready <pr> <sha>" when CI is green.
