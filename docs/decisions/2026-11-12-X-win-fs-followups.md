# X-win-fs-followups decisions

Follow-ups from #728's review (`runner-state:reviews/rv-728-f4d88a14.md`) and from #724. These are the runner's calls under §2.33. None loosens an owner decision or a security decision.

## The service's Event Log note spawns as the helper's does (rv-728 N4)
- **`windows/service.rs` builds its `eventcreate` in `event_note_command`:**
  - `windows::system32_command`: `System32`'s copy by absolute path, no console window, no relaunch token;
  - then `update::windows::helper::clean_env`, which removes every `LODESTAR_*` variable, as the helper's `Windows::command` does.
- Before, it built the path itself and removed nothing, so any `LODESTAR_*` variable in the service's environment reached the child. Now both Event Log writers spawn the same way.
- **The arguments are unchanged:** `/T`, `/ID`, `/L APPLICATION`, `/SO Lodestar`, `/D` and the note's text.
- **Test, Windows only:** `the_event_note_spawns_as_the_helpers_does` checks the program path, that the relaunch token and every `LODESTAR_*` variable the test process has are removed, and the arguments.
  - It can't plant a variable of its own, because the workspace forbids `unsafe` and `set_var` needs it under edition 2024. `clean_env`'s filtering has its own test over a given list of names (`helper.rs`).

## Docs (rv-728 N1)
`2026-11-12-X-svc-eventcreate-bounded.md`: the clippy command gets its missing `--` before `-D warnings`, and the note about the pre-existing Windows warnings is no longer in the first person.

## Checks
- `cargo +1.99.0 clippy -p lodestar -p lodestar-pipeline --all-targets -- -D warnings` passes.
- A Windows-target pass (`--target x86_64-pc-windows-gnu`) finds nothing in the touched files. It shows only the warnings already on `main`, queued as X-win-clippy.
