# F-updater: an acknowledged updater pause is durable or reported as failed (DUR-26, release blocker)
**Model:** Opus. **Security-sensitive** (update engine): run `/security-review` before the PR.

## Base and PR timing (read first)
- Branch `slice/F-updater` from `origin/land/L4`. That branch is the owner's soaked work up to the updater ordering fix (`9d64bca6`) and the persistence audit; it sits on `origin/next`, the rewritten history that replaces `main` (owner-decisions §2.25, on `origin/claude/busy-allen-aptli2`). Never base on, merge or pull the old `origin/main`. A head containing the old root `15620683` or the old tip `2d5e761d` will be refused.
- Your PR opens only after (1) the owner's history swap and (2) L4 merging into `main`. The runner tells you when both are done; then merge `origin/main` once and open the PR. Until then: build, test, self-review, push your branch and message the runner "ready, waiting for L4".
- The spec lives on a later landing branch: `git show origin/land/L7:docs/plan/2026-10-27-persistence-and-import-audit.md` (section "Updater acknowledgment semantics (DUR-26)"). Read all of it; it is the contract.

## Problem
`crates/lodestar/src/update/engine.rs` `save_updater` (~line 247) discards `bin.write` errors (`let _ = …`). `Engine::pause` returns `UpdatesStatus`, and the service adapter wraps it in `Ok`. A pause the user was told succeeded can vanish after restart.

## Do
1. **Contract first (the spec requires an API/schema review before code).** Write `docs/decisions/<date>-F-updater.md` proposing:
   - the typed persistence error and its fixed error code on `POST /updates/pause` (and any other mutating `/updates` route that saves updater state);
   - the status field that says automatic updates are held in this process only;
   - the safe, localized explanation (never a raw path or OS error text).

   Message the runner with a short summary and wait for its OK before implementing. Keep it inside the existing DTO and error envelope style (`crates/api/src/dto/updates.rs`, `crates/api/src/updates.rs`).
2. **One serialized mutation path:** under the existing mutex, clone the last durable state, apply the mutation, write it atomically, then publish it as durable and acknowledge. Write, flush and rename failures return the typed error. Apply it to every updater-state mutation (pause, resume, skip, history clear, background check timestamps). Background check timestamps may stay transient on failure but must never overwrite a durable pause.
3. **The failed-pause hold:** a failed pause inhibits automatic update work in this process until a later explicit save succeeds. The hold is never written as a durable pause. A failed resume keeps the pause and any hold. No retry silently acknowledges the original request.
4. **Unsupported platforms** (Windows today) keep returning the existing unavailable capability. No in-memory imitation of updater support.
5. **Out of scope:** install, rollback journal, policy eligibility, scheduling, and any web UI (S-updates doesn't exist yet; the API contract is what it will consume).
6. **Tests (no fixed sleeps; wait on conditions with deadlines ≥ 10 s):** every Unix fixture listed in the spec, including the queued-save ordering, write/flush/rename failure injection, fail-pause → restart, fail-resume → retry → restart, and the background-check race. Assert non-zero selected test counts on Unix. Windows tests assert capability reporting only.
7. Regenerate `web/src/lib/api/openapi.json` and `schema.d.ts` with the repo's tooling if the DTO changes; never edit them by hand. Update the log-event catalogue golden through its generator if you add events.

## Own
`crates/lodestar/src/update/**`, `crates/api/src/updates.rs`, `crates/api/src/dto/updates.rs`, `crates/api/tests/updates*.rs`, the generated `web/src/lib/api/openapi.json` and `web/src/lib/api/schema.d.ts`, `docs/decisions/<date>-F-updater.md`.

The paused B-update 5/5 (install path) also touches `update/`. It stays paused until this slice merges; don't touch install code.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass, including the Unix updater tests with non-zero counts; `/security-review` is clean; the fresh-reviewer self-review is done; and the runner has OK'd the contract.
