# X-win-data-dir-unusable-log decisions

A follow-up from the Windows service re-test on `main` (2026-11-20), queued by the runner. Calls not marked as the runner's are this PR's, and the runner flags them to the owner (`docs/owner-decisions.md` §2.19).

## The service refuses a data folder it can't use
- **Before:** when the data folder was a file, couldn't be read by the service account, or couldn't be created, the service went on to `serve`, failed there and stopped with service-specific error 1. Its log lives in that folder, so nothing was written, and the Event Log had no entry.
- **Now:** the service refuses the start, as it refuses a relative path (X-win-service-relative-paths and -2):
  - a new kind, `Refusal::Unusable(setting, problem)`, written as an **Error with event ID 6**;
  - the exit code is 3 (REFUSED), as for every refusal, so Windows shows service error 3 and the user guide's section 6.2 applies;
  - the existing kinds and IDs are unchanged: 3, 4 and 5.
- **The Event Log entry names** the folder, the setting that chose it (`LODESTAR_DATA_DIR`, `data_dir` in the config file, or the default under `%ProgramData%`), what's wrong ("is a file, not a folder", "can't be read by the service account", "can't be created") and the fix in plain words. Setting names are fixed text, and the folder is a folder path, so this stays within Legal Notice section 5. The folder is the unusable one, not the default folder the other kinds name.
- **The temp log** (`%SystemRoot%\Temp\lodestar-service.log`) gets the same, plus the config file's path for `data_dir` and the system's own error.
- **No message-file change.** The service writes through `eventcreate`, whose own message file renders any ID in 1–1000 as the text alone, so ID 6 needs nothing in the MSI.

## When and how it checks (this PR's calls)
- **The service's own folder** (`%ProgramData%\Lodestar`) is checked just before the ACL step, whenever the service uses that folder. Otherwise the ACL step fails first, as "isn't private" (ID 3), which points at Repair even when a file is in the way. It's never created here: `acl::secure_dir` creates it private, as before.
- **Any other data folder** is checked once the config is loaded, the relative-path refusals have run and the guard has secured the service's folder if the data folder is inside it. That's before the rollback check, logging or the store use it.
- **A missing data folder is created at that check** with `create_dir_all`, as `serve` and `worker` already did a moment later. Creating it is the only reliable test of "can't be created" (a read-only parent, a full or missing drive). Inside the service's folder it inherits the private ACL, as before.
- **"Can't be read"** means the service can't list it (`read_dir` fails). Writing isn't checked: the brief names reading only, and a write failure still stops the service with error 1 and its own log line.
- **No link is followed or created through.** A data folder that is a link, junction or any other reparse point, or a missing one with a reparse point among its existing parents, isn't checked or created here, and is used as before. `secure_dir`'s checks are unchanged, and still refuse reparse points in the service's own folder.
- **The default's `%ProgramData%` setting** is named "the default, under %ProgramData%". The fix for it is Repair (unreadable or can't be created) or moving the file away (a file).
- **The CLI is unchanged.** Only the service the SCM starts (`windows::service::load_checked`) checks.

## Code
- `load_guarded` keeps its signature, so the relative-path tests are unchanged. `load_checked` returns the folder the Event Log names too, and the SCM path uses it.

## Follow-up (not in this PR's paths)
- The user guide's section 6.2 (`docs/user/06-troubleshooting.md`) explains IDs 3 and 5. It should add ID 6. That needs a docs slice under `user-docs`.

## Tests
`windows::service::tests`, on every platform unless marked:
- **`the_service_refuses_a_data_folder_that_is_a_file`:** a file as `LODESTAR_DATA_DIR`, as `data_dir` in a `--config` file (naming the file), and as the service's own folder, where it's refused before the ACL step (that step panics if run).
- **`the_service_refuses_a_data_folder_it_cant_create`:** a data folder under a file, through `LODESTAR_DATA_DIR`, and the service's own folder when `%ProgramData%` is a file. A missing folder is created, and the service's own folder is left to the ACL step.
- **`the_service_refuses_a_data_folder_it_cant_read`:** a folder denied listing, through `LODESTAR_DATA_DIR` and as the service's own folder. On Windows the test denies Everyone list access with `icacls`, then removes the deny entry. On Unix it uses mode 000, and skips where permissions don't apply (root).
- **`a_link_is_never_followed_or_created_through`** (Unix): a symlink to a file is used as before, and nothing is created through a symlinked parent.
- **`the_unusable_folder_note_names_the_setting_folder_and_fix`:** every setting and problem names the folder, the setting, the problem and the fix, holds no account or quote, stays on one line and gets ID 6. IDs 3, 4 and 5 are unchanged.
- The file and can't-create tests fail with the new checks removed (checked by disabling them).
