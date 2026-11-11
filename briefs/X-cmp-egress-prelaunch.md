# X-cmp-egress-prelaunch (Opus, core, /security-review)

Continues in the CMP-5g-3 session. From the #660 review, runner-state reviews/rv-660-c54df896.md.

## Goal
A supervised companion must never send traffic outside its chosen route, even for the seconds before CMP-8b's configuration runs. Today, Prowlarr on a SOCKS5 route boots with no proxy until the follower or `start_installed` configures it through the API. Its startup tasks (update check, indexer-definition sync, telemetry) go direct in that time, which breaks Q8.

## Approach
Prefer writing the proxy before launch: put Prowlarr's proxy settings in its config file (`config_files.rs`), beside the credentials and bind keys this slice already writes before every start. That is only acceptable if Prowlarr reads them at boot. Check this against the pinned version's config schema and say how you checked it.
- If that's not possible, the fallback is: start the companion with no network until it's configured, then restart it on its route.
- Either way, a companion whose route can't be expressed before launch must not start (fail closed). It must never start "direct, then fix".
- qBittorrent and FlareSolverr already refuse SOCKS5 (`cant_follow_route`). Keep that.

## Also
- **L1** (`crates/companions/src/manager.rs` ~1727): when the follower can't claim the slot, it must not return quietly and leave the companion running unconfigured. Either halt it (fail closed), or retry with a deadline while `is_run` still holds. Add a test for an operation that claims the slot and fails.
- **A deadline:** `configure_now` gets an overall `LIVE_DEADLINE`.
- **The decisions file:** in the CMP-5g decisions file's wording, the window "first becomes reachable in CMP-5g-3". Record this slice in `docs/decisions/2026-11-11-X-cmp-egress-prelaunch.md`.

## Owns
- `crates/companions/src/manager.rs`
- `crates/companions/src/config_files.rs`
- `crates/companions/src/configure/prowlarr.rs`
- `crates/lodestar/src/companions.rs`
- the decisions file, and the one-line wording fix in the CMP-5g decisions file
- tests beside these

## Rules
- No fixed sleeps; condition waits need deadlines of at least 10 s.
- No secrets in logs or errors.
- No API shape change without asking the runner.
- Follow the steward skill. Message the runner when the PR is ready.
