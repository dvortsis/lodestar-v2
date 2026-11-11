# X-event-log-followups (Opus, /security-review): Event Log follow-ups after #702

Runner brief, 2026-11-11 (UTC). The CMP-11b session continues (#711 merged as b98e18a9).

## Why
#702 (X-event-log-notice, Legal Notice version 5) made the Windows service write its refusal reason to the Application log, and the update helper already writes failures there through `eventcreate`. Several comments, docs and checks still describe the old state, and rv-702 left nits.

## Do
1. **Stale comments.** `crates/lodestar/src/settings/logging.rs` (about lines 209 and 435) says nothing writes to the Windows Event Log. Correct them: the service writes a refusal note, the update helper writes update failures, and `event_log_errors` stays hidden in the UI.
2. **Guide.** `docs/user/06-troubleshooting.md` 6.2 already has the Application-log pointer (PK-guide-2b, #707). Check that the table rows at the top also point to the Application log where they mention Event Viewer, and that wording matches the service's note. Use the user-docs skill; mark it "(Newer than v0.5.1)" as the section already does.
3. **Operator guidance.** `docs/legal/operator-guidance.md` line 3 says "version 4 of the Legal Notice". Make it version 5, and check the rest of the page for the Event Log exception.
4. **The update helper's interpolated reasons.** In `crates/lodestar/src/update/windows/helper.rs`, the `event_log` calls (about lines 737 and 803) interpolate `{why}`. Confirm by reading every `why` source that none can carry an account name, a path inside a user profile, a config value or a secret. Where one can, replace it with a fixed reason code. Add a test that pins the allowed shapes.
5. **Bound the `eventcreate` wait** (helper.rs, about line 1476): a deadline, with the child killed and the call abandoned on expiry. No fixed sleep. A test with a fake command that never exits.
6. **Wider invisible-character filter.** `crates/lodestar/src/windows/service.rs` `invisible()` (about line 202) also drops U+00AD, U+061C, U+2060–U+2064 and U+206A–U+206F. Compare with `windows/acl.rs`'s list (about line 384) and share one function if that's cheap. Tests for each new range.
7. **Smoke check.** `packaging/windows/smoke.ps1`: after the refusal scenario (if the smoke has one), check with `Get-WinEvent` (LogName Application, ProviderName Lodestar) that the service account wrote the event. Use a bounded wait, at least 10 s, never a fixed sleep. If the smoke has no refusal scenario, say so and skip; don't add one.
8. **Legal README.** In the `docs/legal/README.md` table, list the .NET `EventLog` and `WriteEntry` patterns beside `eventcreate`, so the legal_facts check knows them.
9. **Nit.** The heading of `docs/decisions/2026-11-11-X-event-log-notice.md` gets `(#702)`.

## Owns
The files above: `crates/lodestar/src/settings/logging.rs`, `crates/lodestar/src/update/windows/helper.rs` (only the event_log reason and wait), `crates/lodestar/src/windows/service.rs`, `crates/lodestar/src/windows/acl.rs` (only if you share the filter), `packaging/windows/smoke.ps1`, `docs/user/06-troubleshooting.md`, `docs/legal/operator-guidance.md`, `docs/legal/README.md`, the #702 decisions file, tests next to them, and `docs/decisions/2026-11-12-X-event-log-followups.md`. The Legal Notice text itself (`notice.md`, `legal-notice.md`) is not changed. If a fix needs it, stop and tell me: notice wording is the owner's.

## Bar
- runner/standards.md; `cargo fmt --check`, `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar` for the touched modules, plus `legal_facts`; Windows-gated tests pass on windows-latest in CI.
- About 200–400 lines. Branch `slice/X-event-log-followups` from fresh main, PR titled `X-event-log-followups: <title>`.
- Steward skill; self-review first; push once per round. Reply "ready <PR> <full sha>" to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) when ci-ok, web-ok and e2e-ok are green.
