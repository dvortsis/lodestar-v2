# B-update 5/5: where it stopped (paused by the runner, 2026-10-24)

Paused for v0.5 (the owner's priority). Nothing is pushed; nothing has been reviewed or run in CI. Resume from this branch, `slice/B-update`, on top of main f19681d (#157).

## Done (5a, local commits)
- `update/download.rs`: the streamed download through the check's host allowlist (at most 5 redirects); `.part` created new with `O_NOFOLLOW` and 0600; the size cap while streaming; SHA-256 and size compared after fsync; stale `.part` files removed; tests.
- `update/install.rs`: stages `<version>.tmp/` (binary 0700, manifest and signature), fsyncs it, renames it to `<version>/`, fsyncs `<data>/bin`; tests.
- `update/engine.rs`:
  - the install pipeline: the policy's Install now rules, manual-steps confirmation, busy probe, re-verify of the cached envelope, pin, free space, download, stage, `Installer::verify` (`overlay::verify` in production), re-verify, then `switch` (set_intent + `Restart::global().request()`);
  - queued installs (`when: window`) and the plan's automatic installs in the schedule loop;
  - events: waiting_window, downloading (throttled), verifying, installing, restarting, failed, checking_health, done, rolled_back;
  - `can_install` is real now (`NotBuiltYet` removed); `Context::dev`; tests for install now, refusals, a bad download or rebuild, the window queue, the automatic install and the start-up events.
- `update/probation.rs`: on a pass, appends `installed` to history and signals `passed()`.
- API: `POST /updates/install` (202; `404`, `409 version_skipped` or `install_unavailable`, `422 manual_confirmation_required`), `installing` in `GET /updates`, goldens, the route lists. `openapi.json` and `schema.d.ts` regenerated.
- `services.rs`: the busy probe (the rebuild under way). `log_events`: `update.install_failed`. The catalogue golden is re-blessed.
- The decisions file: `docs/decisions/2026-10-24-B-update-r5a.md`.

## Not yet done for 5a
- The full checks: they were started but stopped at the pause. Last known: the `update::` tests pass (84) and the API goldens pass. Next: fmt, clippy, the workspace tests, web check, vitest, log_lint and licences.
- Self-review against the runner's 5a security bar:
  1. verified against `trust.json` right before the switch (done: the second re-verify);
  2. symlink and TOCTOU safety of `.part` and staging, including a re-verify after the rename (`overlay::verify` runs after the stage);
  3. the size cap while streaming (done);
  4. one download at a time (the one-check lock).

  Then a fresh Opus review, `/security-review`, merge main, push, and the PR "B-update: … (5a/5: install)".
- Size: about 1,700 changed lines so far, generated files included. Measure before the PR.

## 5b (next PR)
- `POST /updates/rollback`: `202`, then `EXIT_ROLLBACK` once the answer is out; `409 rollback_unavailable`; `rollback.available` true when possible.
- The binary-level fake-source test (debug-only base URL, download-client policy): wrong hash, wrong size, oversized, another target's artifact.
- "Trust survives a rollback" (a rotation shipped in a release that fails probation).
- A log capture over an install: no URL with a query, no key material, no signature text.
- The PR closes the slice: the user-facing steps and the §2.19 list per the brief's "Done when".
