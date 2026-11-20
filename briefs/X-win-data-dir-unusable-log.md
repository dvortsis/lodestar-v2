# X-win-data-dir-unusable-log

**Title (PR):** `X-win-data-dir-unusable-log: refuse an unusable data folder with an Event Log entry`
**Model:** Opus. **Branch:** `slice/X-win-data-dir-unusable-log` from fresh `main`.

## Why
The Windows service re-test on main (2026-11-20) found one gap: when the configured data folder is a *file*, or a folder the service account can't read, the service stops with service-specific error 1 and writes no Lodestar log line and no Event Log entry. The user sees only "error 1" in the Services console, with nothing pointing at the setting or the fix.

## What
- Refuse that start the way the relative-path refusals do (X-win-service-relative-paths and -2: `load_guarded`, `Refusal`, `not_full` in `crates/lodestar/src/windows/service.rs`). The Event Log entry names:
  - the setting (`data_dir`, `LODESTAR_DATA_DIR`, or the `%ProgramData%` default);
  - the path;
  - what's wrong ("is a file, not a folder" / "can't be read by the service account" / "can't be created");
  - the fix, in plain words.
- Use an existing `Refusal` kind if one fits, or add one with its own service-specific code and message-file text. Keep the existing codes stable.
- Check before the folder is used, and before the ACL step if the ACL step would otherwise fail first with a less clear message.
- Don't follow or create anything through a reparse point the existing ACL code would refuse. Keep `secure_dir`'s checks as they are.

## Owns
`crates/lodestar/src/windows/service.rs` and its tests. If the message file (`.mc`) or another Windows file must change, ask the runner first.
Decisions file: `docs/decisions/2026-11-20-X-win-data-dir-unusable-log.md`.

## Done when
- Unit tests cover a data folder that is a file, one that can't be created, and (where Windows CI can make one) one the account can't read. Each gives the named refusal, not error 1.
- The existing relative-path refusals and their tests are unchanged.
- The CLAUDE.md Rust checks pass; test (windows-latest) is green.
- No fixed sleeps; ≲ 400 changed lines.
