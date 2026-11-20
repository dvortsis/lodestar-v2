# X-network-lost-msg (Sonnet, small)

Runner brief, 2026-11-20. Source: the #950 gluetun re-test.

## Problem
The `system.network_lost` event message (crates/lodestar/src/logging/events.rs, SYSTEM_NETWORK_LOST, ~line 335) reads "...only the loopback interface has been left for this many seconds...". The number is missing from the sentence a user reads in the log. The event carries a `seconds` field (net_watch.rs ~261 passes HOLD = 30 s).

## Fix
- Make the message read correctly for a user reading the log line. Follow the catalogue's existing convention for how messages refer to their fields: look at other events in events.rs with numeric fields, and at how the log line and the Logs UI render `message` plus `fields`.
- Prefer wording that stays true if HOLD changes, e.g. it names the `seconds` field the line shows. Hard-code "30" only if the convention does that and a test ties it to HOLD.
- Keep the meaning: Lodestar stops and exits 75 so its restart policy starts it again inside the shared container's network.
- Check every test, golden or doc that pins the old text (`git grep "this many seconds"`, settings goldens, docs/user) and update it in this PR.
- The web copy (en.json/zh.json `settings_logging_ev_system_network_lost_h`) already says 30 seconds. **Do not edit web files**: another open PR owns those message files.

## Owns
- crates/lodestar/src/logging/events.rs (the SYSTEM_NETWORK_LOST entry only)
- crates/lodestar/src/net_watch.rs (only if needed)
- any test or golden that pins this exact message, outside web/

## Size
≲ 60 changed lines. No decisions file is needed for a wording fix, unless you change the field.

## Checks
cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test -p lodestar (at least the logging and settings tests).
