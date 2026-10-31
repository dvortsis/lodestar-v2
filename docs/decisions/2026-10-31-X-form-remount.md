# X-form-remount decisions: a resync doesn't remount the Settings form

The brief assumed `SettingsForm` unmounts its children on a resync, which resets the hand-built widgets' local state and drops keyboard focus. It doesn't.

- **Finding.** Only `Engine.load()` sets `status = 'loading'`. Only the first load and Retry after a failed first load call it. `resync()` and `refetch()` never do. A mount counter on the Logging event list shows one mount across a toggle, a save and a real `resync` event. The focus and search-text loss seen in #289 most likely came from the `<details ontoggle>` race, which the click-driven open state already fixed.
- **No engine change.** A keep-mounted path in `SettingsForm` would never run in production and would add about 0.06–0.07 KB to every Settings first view, against the owner's smallest-and-fastest priority. `form/**` is unchanged.
- **A regression guard instead.** `web/e2e/settings-form-focus.keyboard.test.ts` saves, sends a real `resync` event, waits for the refetched value, and checks that focus stays on the control, the Logging search text is unchanged and the group stays open. It covers one generated screen and the Logging event list, keyboard-only, with axe.
- **#289's workaround removed.** The Logging event list's `opened` is component state again, and its comment no longer says a resync remounts the list.
- **Correction to `2026-10-31-S-logs-2.md`.** Its review-round bullet says the module-level `opened` survives "the list remounting during a resync" and that a resync "shows the form's loading state for a moment". Neither happens. That file is left as written.
