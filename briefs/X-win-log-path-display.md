# X-win-log-path-display

From the Windows re-test by X-test-vm on 0728f0a9 (2026-11-20). Low priority, UX. The runner's call (§2.26). Opus.

## Problem
The Windows service's refusal messages Debug-quote paths, for example `"C:\\ProgramData\\Lodestar"`, with doubled backslashes (crates/lodestar/src/windows/service.rs, around :259, :306 and :353, plus any similar lines). Users read and copy these from the temp log (`%SystemRoot%\Temp\lodestar-service.log`) and the Event Log. The doubled backslashes look wrong, and a pasted path doesn't work.

## Do
1. Show paths as Windows users read them: `"C:\ProgramData\Lodestar"`, using Display inside plain quotes, in the temp log and in the Event Log text, which must match each other.
2. **Safety.** Control characters can't occur in Windows path components, but the text may come from a config file. Keep any non-printable or bidi character escaped, by reusing or adding a small helper beside the existing `shown()` in service.rs. Never write it raw.
3. Find every user-facing path in crates/lodestar/src/windows/*.rs that goes through `{:?}` or `shown()`, and convert them the same way. Test fixtures that write config files with `{:?}` stay as they are.
4. If docs/user shows any of these messages verbatim, update them. Check first; that edit goes in owns only if needed.

## Owns
- crates/lodestar/src/windows/service.rs
- crates/lodestar/src/windows/temp_log.rs, only if needed
- their tests
- docs/user pages that quote these messages, only if any do; list them in your slices.yaml entry
- your own slices.yaml entry and dashboard.yaml placement line
- docs/decisions/<date>-X-win-log-path-display.md

None of these are owned by a running slice. CR-HINT-FIRST owns crates/lodestar/src/config.rs and metrics.rs, so don't touch either.

## Tests and gates
- Unit tests for a plain path, a UNC path (`\\server\share\x`), a path with spaces, and a value with a control or bidi character, which must stay escaped.
- The Event Log text matches the temp log text.
- Mutations: Debug quoting comes back, or escaping is dropped.
- `cargo fmt --check`, clippy `-D warnings` (the Windows code is cfg-gated, so make sure the CI Windows job builds it), `cargo test -p lodestar`, and the status-connector test.
