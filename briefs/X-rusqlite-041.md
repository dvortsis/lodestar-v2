# X-rusqlite-041

Runner brief, 2026-11-21Z. Slice id `X-rusqlite-041`, branch `slice/X-rusqlite-041`, PR title `X-rusqlite-041: <title>`. The plan entry is in `runner/slices.yaml`. This is a security dependency fix, so the review includes /security-review.

## Why
`deny.toml` ignores RUSTSEC-2026-0459 (libsqlite3-sys / bundled SQLite) with the reason "narrowed, not closed". Lodestar writes its own database, and the Magnetico importer refuses a non-empty `-wal` or `-journal` before it opens a database and before each batch. That check is a stat, not atomic. The ignore must be reviewed by 2026-12-13. The fix is rusqlite 0.41 with libsqlite3-sys 0.39.

## What
1. Bump `rusqlite` to 0.41.x in `crates/store/Cargo.toml` and `crates/sources-importers/Cargo.toml`, with the same features: `bundled`, plus `hooks` and `limits` for the importer. Cargo.lock should then carry exactly one libsqlite3-sys, 0.39.x, and no second rusqlite.
2. Remove the RUSTSEC-2026-0459 ignore from `deny.toml`, and show that `cargo deny check advisories` (or the CI advisory job's command) passes without it.
3. **Fix what the bump breaks.**
   - Most rusqlite use is in `crates/store/src/**` and `crates/sources-importers/src/**`, which you own.
   - Other crates use it too: api (5 files), cluster (2), lodestar (14), pipeline (4) and search (1).
   - If the bump needs edits outside your owns, send me the file list first. X-prefs-invisible-share (#1015) owns `crates/lodestar/src/settings/{mod.rs,preferences.rs}`, and CR-1c (#1014) owns `crates/lodestar/src/config.rs` and `worker/runtime.rs`. Both should merge soon.
4. **lodestar-store's SQLite memstats is the workspace's one `unsafe` exception** (`unsafe_code = "forbid"` everywhere else). If the new libsqlite3-sys changes its API there, keep the change minimal. Don't widen the exception or add any `unsafe` anywhere else; if it seems to need that, stop and tell me.
5. **Behaviour:**
   - The on-disk format and schema stay unchanged. Show that a database written by main opens and passes the store's tests. If there's an existing fixture or golden database, use it.
   - The Magnetico importer's `-wal`/`-journal` refusal stays as it is.
   - Re-read the advisory, and say in the decisions file what 0.39 fixes and whether the importer's stat check is still needed.
6. Run the full workspace tests (`cargo test --workspace`), since SQLite underlies the store, the pipeline and the search.
7. A decisions file `docs/decisions/2026-11-21-X-rusqlite-041.md`. Narrow your slices.yaml entry's owns to what you touched.

## Owns
- `crates/store/Cargo.toml`
- `crates/sources-importers/Cargo.toml`
- `Cargo.lock`
- `deny.toml`
- `crates/store/src/**`
- `crates/sources-importers/src/**`, only as the bump needs
- the decisions file
- this slice's slices.yaml entry

Send me the list before you edit anything else.

## Bars
`runner/standards.md` and the Rust gates in CLAUDE.md. Push once per round of fixes, and send "ready <pr> <sha>" when CI is green, including Windows and macOS if CI runs them.
