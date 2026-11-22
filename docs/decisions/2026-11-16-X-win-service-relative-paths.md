# X-win-service-relative-paths decisions

A follow-up from X-win-service-cwd-fallback (#956), queued by the runner. Calls not marked as the runner's are this PR's, and the runner flags them to the owner (`docs/owner-decisions.md` §2.19).

## The service refuses a setting that isn't a full path

- **Before:** the service the SCM starts (`windows::service::load_guarded`) resolved an empty or relative `LODESTAR_DATA_DIR`, and a relative `--config`, against its working folder, which is the system folder under the SCM. For example, `LODESTAR_DATA_DIR=""` read `System32\config.toml`. Only an administrator can set these.
- **Now:** the service refuses them, and doesn't start. The reason names the setting and its value, and asks for a full path, such as `D:\Lodestar`.
  - **`LODESTAR_DATA_DIR` empty or relative, or `--config` relative:** refused before anything is read or checked.
  - **`data_dir` relative in the config file (this PR's call):** refused once the file is read, naming the file. It's the same fault: the service would otherwise use that folder relative to the system folder. It's refused before the after-load folder check.
  - **A relative service folder (this PR's call):** an empty or relative `%ProgramData%` would make the service's own folder relative. It's refused before anything is read, naming `%ProgramData%`, rather than reported as a `data_dir` fault.
  - **"Full path"** is Rust's `Path::is_absolute`. On Windows that means a drive and a root (`D:\…`), a UNC path or a `\\?\` path. `\Lodestar` (the current drive's root) and `D:Lodestar` (a drive's own working folder) depend on the working folder too, so they're refused.
- **How it's reported:**
  - **The temp log** (`%SystemRoot%\Temp\lodestar-service.log`) gets the full reason, with the setting's value. Only administrators can read it.
  - **The Application event log** gets a new note, `Refusal::NotFullPath`, an **Error with event ID 5**. Like the others, it's fixed text and the default data folder only, and never the setting's value (Legal Notice §5). Before this PR every load error was reported as "the folder isn't private" (ID 3), which would have sent an administrator to Repair.
  - **The exit code is unchanged:** 3 (REFUSED), so Windows still shows service error 3 and the user guide's section 6.2 still applies. Section 6.2 now says what ID 5 means.
- **The CLI is unchanged (runner's brief).** It keeps relative paths: a portable install uses `data` beside the executable, and the tests set a working folder (`Install::working`). The CLI is started from a folder its user chose, so a relative path there means what they meant.

## Amends #956's "Not covered" line

- **#956's decisions file** (`2026-11-15-X-win-service-cwd-fallback.md`) says a service folder whose path isn't valid UTF-8 keeps the old load.
- **Added: that case can't arise.** The service folder comes from `std::env::var("ProgramData")` (`windows::service::default_data_dir`). `std::env::var` returns an error for a value that isn't valid Unicode, and the service then uses its fixed fallback, which is valid UTF-8. So `default_dir.to_str()` always succeeds in the service. (A relative value is now refused; see above.)

## Tests

- **`windows::service::tests::the_service_refuses_a_setting_that_isnt_a_full_path`** runs on every platform:
  - an empty `LODESTAR_DATA_DIR`, and `data`, `./data` and `../Lodestar` as `LODESTAR_DATA_DIR` and under `--config`, are refused with `NotFullPath`, naming the setting, before the folder check runs (that check panics if called);
  - on Windows, `\Lodestar`, `D:Lodestar` and `C:` too;
  - a relative `data_dir` in a `--config` file, and an empty one in the service folder's own `config.toml`, are refused, naming `data_dir` and the file;
  - a relative service folder is refused, naming `ProgramData`;
  - full paths load as before, through `LODESTAR_DATA_DIR`, `--config` and the default.
- **`the_event_log_text_names_only_the_folder_and_the_fix`** covers the new note: fixed text with the folder, no account or secret, one line, within `eventcreate`'s range, and its own event ID.
- **`the_service_never_reads_its_working_folders_config`** now checks the child's summary line exactly (`test result: ok. 1 passed;`), so another line that happens to contain "1 passed" can't satisfy it.
