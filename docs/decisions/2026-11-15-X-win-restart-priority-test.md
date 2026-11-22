# X-win-restart-priority-test decisions

A follow-up from #924's review (`2026-11-15-X-win-portable-restart-load.md`).

## Calls
- **The copy Restart now starts runs at its configured priority, not an inherited one** (runner).
  - Since #924, it starts at normal priority and lowers itself through `priority::apply` in `startup`, as any start does. `LODESTAR_PRIORITY` decides the class: below normal by default.
  - It stays at normal priority in three cases:
    - where `hook` returns before `startup`: an ordinary `lodestar worker` run, or a command line `service_command` doesn't take;
    - where the config doesn't load;
    - where `apply` itself fails, which logs `WINDOWS_PRIORITY_FAILED`.
  - Before #924, they inherited the old copy's class: below normal where it had lowered itself.
- **The test that lowers the test binary puts back the class it found.** It used to restore a hard-coded `Normal`. Restoring takes only Windows' own class names (`priority::set_class`, test-only).

## Tests
- `portable_restart` (`crates/lodestar/tests/windows_service.rs`) is used by `a_portable_restart_serves_again_on_the_same_port` and `a_portable_restart_starts_a_new_copy`. Once the new copy serves, it now polls until the one Lodestar process left on the test's config reads `BelowNormal`. It uses `wait_for` with a 30 s deadline, as `runs_as_a_service` does, and fails with the console and log dump. This covers what keeps the steady state unchanged now that nothing is inherited.
- Priority exists only on Windows. The parts that don't depend on the platform already have tests on every platform:
  - the creation flags (`restart_now_starts_its_processes_at_normal_priority`);
  - the setting and its script (`priority::tests`).
