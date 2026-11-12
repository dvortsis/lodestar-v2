# X-tz0-polish (Opus; same session as TZ-0)

Polish from the fresh Opus review of #715 (`reviews/rv-715-b30d1a8e.md`, MERGE, merged as c5abcb85). Branch `slice/X-tz0-polish` from current `main`. PR title `X-tz0-polish: <title>`.

## Do
1. The notice-gate `409` error document (`crates/api/src/torznab.rs:117`) tells the client to "use an API key". Say what the admin must actually do: accept the Legal Notice in the web UI.
2. Add the RSS 2.0 `<channel><link>` that the spec requires (`torznab.rs:389-391`). Use the server's own relative or configured base; never echo the request's Host header. Update the caps/search goldens.
3. The decisions file `docs/decisions/2026-11-11-TZ-0.md`: name the hand-written XML writer as a departure from the plan's line 126 (which said quick-xml), with the reason.
4. Settings help for `torznab_adult` says `/torznab/api`; make it `/api/v1/torznab/api`. Regenerate `crates/lodestar/src/settings/golden/settings_index.txt` with `LODESTAR_BLESS=1`.
5. Optional, decide and record: also strip the bidi marks U+200E, U+200F, U+061C, or keep them with a one-line reason in the decisions file.

## Not here
How the *arr apps display non-200 Torznab errors is for D-outputs-guide (the runner adds it to its notes).

## Shared edits approved
`crates/lodestar/src/settings/content.rs` (help text only) and the settings golden.

## Checks
`cargo fmt --check`, `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. No fixed sleeps. Steward skill; message the runner "ready <PR> <sha>" when ci-ok, web-ok and e2e-ok are green.
