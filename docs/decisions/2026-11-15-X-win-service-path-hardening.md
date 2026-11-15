# X-win-service-path-hardening decisions

Hardening of how Lodestar on Windows decides that a folder is the service's data folder (`%ProgramData%\Lodestar`), and of when the service checks that folder. These items were deferred from X-cli-win-data-dir's reviews (#926, S8-07). Every remaining precondition needs an administrator, so nothing here was held. Calls not marked as the runner's are this PR's, and the runner flags them to the owner (`docs/owner-decisions.md` §2.19).

## The service checks its folder before reading from it
- **What changed:** the service the SCM starts now makes its own folder private (`acl::secure_dir` and its verdict) before it reads any config file from there, however the folder was named. Before this, a `config.toml` in a folder named by `LODESTAR_DATA_DIR`, or a `--config` file inside the folder, was read first.
- **How:** the service loads its config through `windows::service::load_guarded`, which uses the CLI's guard (`cli::ServiceGuard`). So the service and the CLI follow one rule and can't drift apart:
  - **Before any config is read,** the folder is secured when:
    - neither `--config` nor `LODESTAR_DATA_DIR` points elsewhere, since `config.toml` there is read;
    - `LODESTAR_DATA_DIR` names the folder or a folder inside it;
    - the config file is inside the folder.
  - **After loading, before anything else,** it's secured when the data folder is the service's folder or inside it.
  - **At most once.** The guard runs the check once and gives every later ask the same answer.
- **What's accepted is unchanged:** the same `secure_dir`, with the same verdict and the same refusal. Only the order moved.
- **Not changed here, a follow-up for the runner:** with no `config.toml` in the service's folder and nothing set, the service still falls back to `data\config.toml` in its working folder. Under the SCM that's the system folder. The behaviour predates this slice.

## The CLI's implicit config file is checked too (from the review)
- **What the guard sees** is the config file that will actually be read (`cli::config_file`, or the MSI branch's file), implicit ones included, not only an explicit `--config`.
- **So:** a working folder that is the service's folder or inside it is checked before its `data\config.toml` is read, whatever that file says.
- **Relative paths** start from the working folder, as the config loader resolves them. Tests set that folder (`Install::working`) without changing the process's.
- **The file the loader would look for counts whether it exists yet or not,** so a file created between the check and the load is still covered by the check.

## A data folder inside the service's folder
- **The guard covers it,** in the CLI and the service: a data folder inside the service's folder is checked as the folder itself is.
- **What's checked is the service folder's own verdict.** That verdict already refuses a link or junction anywhere beneath the folder, and an entry directly in it that another account owns.
- **Not chosen: applying the strict folder rule to the nested folder too (runner's call).** A subfolder that inherits the private folder's permissions legitimately fails the rule that a data folder's permissions must not inherit.
- **Whole path parts only.** A folder whose name merely starts the same, such as `Lodestar2` beside `Lodestar`, isn't inside it.

## Comparing paths by what they are
- **`windows::service::same_dir` and `within` compare what the system says a folder is,** when both paths can be opened. The comparison is the volume serial number and file index, each path opened with `FILE_FLAG_BACKUP_SEMANTICS`, after links and junctions are followed.
  - **`within`** compares the outer folder with the inner path and with each folder above it, so a junction to the service's folder, and anything under one, counts.
  - **A match always counts.** Otherwise the comparison falls back to the path's text.
- **The dependency: `same-file` 1.0.6 (runner's OK).** It's a direct dependency of the lodestar crate on Windows only. It was already in `Cargo.lock` through walkdir, so nothing new is downloaded.
  - **Why:** its `Handle` reads the volume serial and file index through a safe API. The workspace forbids `unsafe` (`docs/owner-decisions.md` §1b.5), so calling `GetFileInformationByHandle` directly isn't possible.
  - No `unsafe` is added anywhere.
  - **Shipped licences:** `same-file` and its `winapi-util` now ship in the Windows build, so both are listed in `web/licenses/rust.json` (Unlicense or MIT, both on the allowlist; owner §2.12).
- **The fallback, for a path that can't be opened** (one that doesn't exist yet, or one this account can't read), compares the text as Win32 reads it:
  - the full path through `GetFullPathNameW` (`std::path::absolute`), which resolves `.`, `..`, trailing dots and spaces, and `/`;
  - the nearest existing folder's real path, as before;
  - then the text itself, case-insensitively and without a `\\?\` prefix.
- **Only text the system hasn't resolved is tidied (from the review).**
  - **Tidied:** a name not created yet, or a path whose folders can't be resolved. Here `.`, `..`, doubled and trailing separators, and trailing dots and spaces are handled as Win32 handles them. `..` never climbs above the drive, or above a UNC server and share.
  - **Not tidied:** the system's own real path, and any part of a path given verbatim (`\\?\`), which Win32 takes literally. So a real folder named `Lodestar.` is never matched to `Lodestar`.
  - **Why it matters:** besides the guard, the update check treats a match as "the folder the service checked". The fallback mustn't match a different folder.
- **The fallback's limits:**
  - It lowercases with Unicode's rules, not NTFS's own case table.
  - It doesn't strip a `\\.\` device prefix.
  - It's reached only when the folders involved can't be opened and resolved.
- **Left to the system:** a mapped drive or an SMB loopback path (`\\localhost\C$\…`) to the service's folder is recognised by file identity alone. Their text differs from the folder's. Setting one up needs an administrator.
- **`within` opens the outer folder once,** then compares the inner path and each folder above it with it.
- **Off Windows** nothing is compared by file identity, and there's no service folder to compare with.

## Not taken: reading ProgramData from the system's known folder (runner's call)
- **The service and the CLI keep reading `%ProgramData%` from the environment,** with the service's fallback, so they still agree on where the folder is.
- **Why not the known folder:**
  - The direct call needs `unsafe`, which the workspace forbids.
  - Asking PowerShell for it would add a PowerShell start, about 0.3 to 1 s, to nearly every Windows CLI run.
  - The registry's own setting still has to be expanded from the environment.
  - An administrator's environment is already trusted, so the case it guards is administrator-only.

## Behaviour an administrator sees
- **None in normal use.** A service whose folder is private starts as before.
- **A folder that isn't private is refused** whichever way it's named, and the refusal is the same. `docs/operations.md` is unchanged.

## Tests
- **`windows::service::tests::the_service_secures_its_folder_before_reading_from_it`** (d and b) runs on every platform. Each case tells the two orders apart, and each fails with the check moved after the read (checked by reverting the change):
  - **`LODESTAR_DATA_DIR` naming the folder, or a folder inside it:** its `config.toml` is broken and the check repairs it, so the config loads only if the check ran first.
  - **`--config` inside the folder, moving the data elsewhere:** refused. Read first, the data would be elsewhere and the folder never checked.
  - **Nothing set, with a valid `config.toml` in the folder that moves the data elsewhere:** refused. Read first, the folder would never be checked. (Its other "nothing set" case, a broken file, is refused either way, by the check after loading.)
  - **Checked after loading:** `--config` elsewhere naming a data folder inside it.
  - **Never checked:** a sibling with a similar name, or another folder.
  - **A private folder** is checked once, then read.
- **`cli::tests::a_data_folder_inside_the_services_folder_is_checked`** (b): the same for the CLI, with the `Lodestar2` sibling unchecked.
- **`cli::tests::the_implicit_config_file_is_checked_too`:** a working folder inside the service's folder, with a `data\config.toml` there that moves the data elsewhere, is refused. That holds for a portable executable, and for the MSI's running `serve` or a maintenance command. A working folder elsewhere is read unchecked. The test fails when the guard sees only `--config`. With no `data\config.toml` there yet, the test records the order: the check had already run when the loader first asked for a setting only it reads, so a file created in between is covered. That case fails when the guard sees only a file that exists.
- **`windows::service::tests::a_real_folder_with_a_trailing_dot_is_its_own`:** on every platform, a real folder named `Lodestar.` isn't `Lodestar` or inside it. On Windows it's also told apart by file identity. The test fails when the real path is tidied.
- **`windows::service::tests::a_folder_is_compared_by_what_it_is`** (c, Windows only):
  - a junction to the folder, and paths under it, including one not created yet;
  - another case;
  - the `\\?\` form;
  - a trailing dot;
  - its 8.3 short name, when the volume makes them (the lookup itself must succeed);
  - a sibling with a similar name is neither the folder nor inside it.
- **`windows::service::tests::off_windows_folders_are_compared_by_text`:** its off-Windows counterpart. Nothing is compared by file identity, and the text decides.
- **`windows::service::tests::paths_compare_as_windows_spells_them`** (c's fallback) runs on every platform:
  - trailing dots and spaces, `.` and `..`, doubled separators, `/` and the `\\?\` prefix;
  - `..` stopping at a UNC share;
  - siblings and parents that aren't the folder.
