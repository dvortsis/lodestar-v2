# X-prowlarr-register-followups (Opus): a source check that the Prowlarr plugin never fetches an item link, plus review nits

Runner brief, 2026-11-12 (UTC). Same session as X-prowlarr-register (#709, merged as 4a044c3e). Slice entry: `runner/slices.yaml`. Use the steward skill; Auto mode.

## Why
The Legal Notice (version 5) says the Prowlarr/Jackett source "never downloads a .torrent from any indexer". rv-709 asked for a test that backs that sentence, so a later edit can't quietly break it.

## Do
1. **`crates/sources-plugins/tests/prowlarr_no_fetch.rs`:** a test over `crates/sources-plugins/src/prowlarr.rs` that fails if the plugin ever fetches an item's `link` (or `enclosure` URL, or any per-item URL). Two parts:
   - a **behavioural** test: drive the plugin against a local fake Prowlarr whose results carry item links pointing at a second local listener, and assert the second listener gets zero requests. Condition waits with a deadline of at least 10 s; no fixed sleeps.
   - a **source-text** guard as a backstop: the plugin's fetch calls only take URLs built from the configured base URL and the search path; name the exact pattern you check, and keep it narrow enough that comments and test code don't trip it.
2. **Nits from rv-709:**
   - `crates/lodestar/src/consent_gate.rs:311`: the rewrap (one-line shared edit, approved).
   - The X-prowlarr-register decisions file heading gets "(#709)".
   - **`services.rs:347` using `prowlarr::ID` is NOT in this slice:** `services.rs` is TZ-0's (#715) and X-upd-stop-early's next. I've moved that nit to X-upd-stop-early.
3. A short decisions file `docs/decisions/2026-11-12-X-prowlarr-register-followups.md` if you make any call; otherwise say "none" in the PR.

## Owns
`crates/sources-plugins/src/prowlarr.rs` (only if the test needs a seam; keep any change minimal and named), `crates/sources-plugins/tests/prowlarr_no_fetch.rs`, the one-line `consent_gate.rs` rewrap, the decisions heading. Anything else: ask the runner. Don't touch `crates/lodestar/src/services.rs`.

## Bar
- runner/standards.md; `cargo fmt --check`, `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Branch `slice/X-prowlarr-register-followups` from fresh `origin/main`; PR `X-prowlarr-register-followups: Prowlarr registration follow-ups`. Small: aim under 400 lines.
- Self-review including /security-review of the test's fake servers (they bind to 127.0.0.1 only). Push once per round; reply "ready" to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with PR number and full head SHA when ci-ok is green.
