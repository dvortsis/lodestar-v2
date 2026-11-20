# X-win-data-dir-followups decisions

Follows X-win-data-dir-unusable-log (#983), whose review left these items. The runner queued them. Calls not marked as the runner's are this PR's, and the runner flags them to the owner (`docs/owner-decisions.md` §2.19). This file adds to `2026-11-20-X-win-data-dir-unusable-log.md`; it doesn't replace it.

## A data folder the service can't write to (runner's brief)
- **Before:** a data folder the service could list but not write to passed #983's checks. The service then stopped with error 1 and no Event Log entry.
- **Now:** it's refused as `Problem::Unwritable`, under the same Event Log ID 6 and exit code 3. The note says the folder "can't be written to by the service account".
  - **The fix for the default folder** is Repair, as for "can't be read": Repair resets the folder's permissions.
  - **The fix for any other folder:** give the Lodestar service permission to change files there, or set the setting to a folder it can write to.
- **The probe:** create a uniquely named file in the folder with `create_new`, then delete it.
  - **The name** is `.lodestar-write-check-<process>-<time>-<n>`.
  - **No link is followed:** `create_new` never opens an existing file or link.
  - **A crash can't break the next start:** if a name is taken, for example by a probe a crash left behind, the probe moves on to the next name. It tries 16 names.
  - **A failed delete is ignored.** The file is empty, and the next start uses another name.
- **When (this PR's call):** only in the check that runs once the config is loaded and the guard has run. That covers the service's own folder too, after the ACL step has made it private.
  - **Not in the check before the ACL step,** so nothing is written into the service's folder before it's private.
  - **After the existing checks,** in this order: is it a file, can it be read, create it if it's missing, then write.

## Review optionals from #983
- **The fix text for "can't be created"** now says to move away any file in the folder's path, then create the folder. "Create that folder" alone can't work when a file is in the way. The temp log already names the file.
- **The can't-create test** asserts that the service's own folder doesn't exist when the ACL step runs. This shows the check before that step never creates it.
- **`load_guarded` is test-only** (`#[cfg(test)]`). Its documentation moved to `load_checked`, and the intra-doc links now name `load_checked`. The one mention outside this file (`cli/mod.rs`) is plain text and outside this PR's paths, so it's unchanged.

## User guide
- **§6.2 of `docs/user/06-troubleshooting.md`** describes event ID 6: what it means, the setting it names, and a table of the four problems with the fix for each.
  - **The safe choice:** give access to `NT SERVICE\Lodestar` only, with the exact `icacls` command, and never to Everyone or Users.
  - **The "stops at once" row** in the table at the top of the page now looks for event ID 3, 5 or 6.

## Tests
- **`the_service_refuses_a_data_folder_it_cant_write`:** a folder created fresh through `LODESTAR_DATA_DIR` passes, with nothing left in it. A write-denied folder is refused through `LODESTAR_DATA_DIR`, and the service's own folder is refused after the ACL step.
  - **Write is denied** with `icacls /deny Everyone:(WD)` on Windows and mode 555 on Unix. The test is skipped where permissions don't apply (root), and the deny is lifted on drop.
  - **It fails with the probe removed** (checked by disabling it, as an unprivileged user).
- **`a_taken_probe_name_is_skipped`:** a file already holding the first probe name is left untouched, and the check passes.
- **Every new message** is covered by the existing note test, which now includes `Unwritable`.
