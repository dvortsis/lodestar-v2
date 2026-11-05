# X-source-adult-2: the always-adult mask on the API correction path and on imports
**Model:** Opus (content safety). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05T10:20Z.

## Why
X-source-adult (#467) classifies items from an always-adult source (Sukebei) as Adult on every pipeline path. It is derived, never stored. Its decisions file (`docs/decisions/2026-11-05-X-source-adult.md`, under "Gaps the runner tracks") names two paths that must close before Sukebei is registered:
1. **`crates/api` corrections.** `/classify/correct` reclassifies with the correction you send and no source signal. Any correction that leaves Is adult and Not adult unset un-adults a source-adult torrent: clearing a correction, or setting only Not junk. The torrent stays at detection's type until an always-adult source finds it again, which may be never.
2. **Imports.** An import commit (`crates/store/src/import.rs`, `crates/sources-importers`) doesn't read `unverified_items`. If a dump holds a hash that only a Sukebei unverified item claims, it's stored with the dump's classification and without Sukebei's bit.

## Do (branch `slice/X-source-adult-2` from `origin/main`; RP-3 (#478) has merged)
1. **Corrections:** in the `/classify/correct` handler (`crates/api/src/filters.rs`, routed at about line 136; find the handlers), apply the same `source_adult::correction` used in the pipeline. A torrent whose `sources` holds an always-adult bit stays Adult unless the user's own explicit Not-adult correction says otherwise, which is the accepted rule. Clearing a correction or setting only Not junk must never un-adult it.
   - **Test:** for a source-adult torrent, cover clear, Not junk only, Is adult, and Not adult.
2. **Imports:** inside the import commit's transaction, a stored torrent whose hash an always-adult unverified item claims gets that source's bit, and it's classified through the mask, the same way `Writer::source_adult` does it. Use the retry mark (`mark_adult_found`, as #479 did for claims) inside the same transaction, so a crash can't leave it un-adult.
   - **Test:** a dump holding a Sukebei-claimed hash is stored Adult. Test a crash between the two steps as well.
3. **Grep for any other path** that writes a classification or `sources` for a stored torrent without the mask: API exports or imports, rebuild, snapshot restore, Undo. Fix it or list it.
4. **Decisions file:** `docs/decisions/2026-11-05-X-source-adult-2.md`. Update the X-source-adult gap list to say these are closed.

## Own
- the `/classify/correct` handler in `crates/api/src/filters.rs`;
- `crates/store/src/import.rs` and the import commit path in `crates/sources-importers` (only the commit hook);
- `crates/api/tests/source_adult.rs`;
- their tests;
- the decisions file.

Don't touch the rest of `api/filters.rs`; RP-5 will take it next. Also leave `crates/pipeline/src/writer.rs` alone: #479 holds it.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and the tests for the api, store, sources-importers and pipeline crates pass. Then run `/security-review`.

Open a PR against `main` titled "X-source-adult-2: Always-adult mask on the API correction path and imports". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round. No fixed sleeps.
