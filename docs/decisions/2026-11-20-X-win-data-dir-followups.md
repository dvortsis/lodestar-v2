# X-win-data-dir-followups decisions

Follows X-win-data-dir-unusable-log (#983), whose review left these items. The runner queued them. Calls not marked as the runner's are this PR's, and the runner flags them to the owner (`docs/owner-decisions.md` §2.19). This file adds to `2026-11-20-X-win-data-dir-unusable-log.md`; it doesn't replace it.

## A data folder the service can't write to (runner's brief)
- **Before:** a data folder the service could list but not write to passed #983's checks. The service then stopped with error 1 and no Event Log entry.
- **Now:** it's refused as `Problem::Unwritable`, under the same Event Log ID 6 and exit code 3. The note says the folder "can't be written to by the service account".
  - **The fix for the default folder** is Repair, as for "can't be read": Repair resets the folder's permissions.
  - **The fix for any other folder:** give the Lodestar service permission to change files there, or set the setting to a folder it can write to.
- **The probe:** create a uniquely named file in the folder with `create_new`, then delete it.
  - **The name** is `.lodestar-write-check-<process>-<time>-<n>`.
  - **The probe never opens an existing name:** `create_new` fails on an existing file or link. A link or junction above the folder is followed, as the service's own writes there would follow it; the earlier checks still skip a data folder that is itself one, or a missing one with one above it.
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
  - **The safe recommendation:** use the default folder, which the service keeps private. Someone keeping their own folder is shown how to make it private to SYSTEM, Administrators and `NT SERVICE\Lodestar` with `icacls /inheritance:r /grant:r`, how to check it, how to remove a deny entry, and how to undo it.
  - **For the default folder,** the guide says not to create it by hand: a hand-made folder inherits shared permissions and the ACL step refuses it (ID 3). Repair, or the service's next start, creates it private.
  - **The "stops at once" row** in the table at the top of the page now looks for event ID 3, 5 or 6.

## Tests
- **`the_service_refuses_a_data_folder_it_cant_write`:** a folder created fresh through `LODESTAR_DATA_DIR` passes, with nothing left in it. A write-denied folder is refused through `LODESTAR_DATA_DIR`, and the service's own folder is refused after the ACL step.
  - **Write is denied** with `icacls /deny Everyone:(WD)` on Windows and mode 555 on Unix. The test is skipped where permissions don't apply (root), and the deny is lifted on drop.
  - **It fails with the probe removed** (checked by disabling it, as an unprivileged user).
- **`a_taken_probe_name_is_skipped`:** a file already holding the first probe name is left untouched, and the check passes.
- **The write test also checks** that an existing folder that passes keeps no probe.
- **Every new message** is covered by the existing note test, which now includes `Unwritable`.

## The runner's review (#985 at 428a77ee)
The runner decided these fixes go in this PR.
- **Only a denied write is a permission problem.** The probe reports `Problem::Unwritable` only for a permission error. Any other error, such as a full or write-protected drive, is a new `Problem::WriteFailed`, still under Event Log ID 6.
  - The note says the folder "can't be written to" and gives no permission fix.
  - It says to check the drive, and that the temp log names the system's error.
  - Before, these errors sent the administrator to permissions for nothing. The guide has a row for it.
- **A test now proves the probe runs after the ACL step.** The service's own folder is write-denied, and the ACL step stub lifts the deny. The start passes, and the earlier refusal shows the step ran first. Probing before that step fails the test (checked by changing the code).
- **§6.2 reads in order:**
  - IDs 5 and 6 come after the ID 3 steps, under "Other refusals". They name the temp log and the command that reads it.
  - The custom-folder steps warn that the `icacls` command removes every other account's access, and should be used only on a folder that holds only Lodestar's data.
  - Going back to the default folder says the library stays behind, and points to backup and restore (§3.7) to move it.
- **The optionals:**
  - the `denied` test helper's doc now says what it returns;
  - the guide says how to remove an extra allow entry or any `(DENY)` entry;
  - it says to choose Cancel, not Continue, in File Explorer's permission prompt;
  - it mentions Controlled folder access;
  - for the default folder, it says to remove a `(DENY)` entry before running Repair again.
- **Checked against the installer:** the installer's own script refuses any Deny entry. So the guide tells the reader to look for one in `icacls` output, which they can check, rather than quoting a Repair message.
