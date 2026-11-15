# X-cli-win-data-dir decisions

CLI commands run on a Windows MSI install now find its data folder with no environment variable. Found on X-test-vm (AWS Windows, `main` bd239634, the same on v0.5.1): from an elevated PowerShell, `& 'C:\Program Files\Lodestar\lodestar.exe' backup --to C:\bk` printed `no store at data\lodestar.db`, and so did `stats` and `import status`. The runner gave the slice and its goal. The resolution below is the service's own rule, so no new user-visible choice is made. Calls not marked as the runner's are this PR's, and the runner flags them to the owner (`docs/owner-decisions.md` §2.19).

## Where the CLI looks
- **The service** finds its folder in `windows::service`. Its `config.toml` is `--config`, else `config.toml` in `LODESTAR_DATA_DIR` or in `%ProgramData%\Lodestar` if present. Its data folder is `%ProgramData%\Lodestar` unless the config file or `LODESTAR_DATA_DIR` chose another. The MSI passes the service no folder of its own: `LodestarServiceArgs` is `serve` or `worker`.
- **The CLI, run from the MSI's executable,** now does the same (`cli::load_config`, `cli::msi_data_dir`). The MSI's executable is `%ProgramFiles%\Lodestar\lodestar.exe`, compared as Windows compares paths: case-insensitively, on the real path. It's the only place the MSI installs (`main.wxs` `WrongInstallFolder`), and the sign the update check already uses for an MSI install. `%ProgramFiles%` comes from `update::windows::msi::system_program_files`, which reads the registry when the variable is unset.
- **What still wins, in order:**
  1. `worker --data`;
  2. `LODESTAR_DATA_DIR`;
  3. a `data_dir` in the config file read, whether `--config` or the folder's own `config.toml`.
- **With no `config.toml` in `%ProgramData%\Lodestar`,** the CLI never reads `data\config.toml` from the working folder. `Config::load` would otherwise fall back to it, so the CLI points the load at the service's folder. That folder isn't then counted as set by the environment.
- **Anything else is unchanged:**
  - the portable zip (its launcher sets `LODESTAR_DATA_DIR` to the `data` folder beside the exe, and run by hand from its folder it finds `data` there as before);
  - an executable anywhere else on Windows;
  - Docker, Linux and macOS, where no executable is ever the MSI's (`Install::here` names none off Windows).
- **`worker` never defaults to the service's folder (runner's call).** A worker's folder is the installer's to set up (`join-worker.ps1` gives its files to the worker's service account), so `worker` keeps `data` unless `--data` or `LODESTAR_DATA_DIR` names one. The worker's own folder creation is never reached for the service's folder.

## The folder must be private first (S8-07, from the runner's reviews)
- **The CLI checks the service's folder is private, as the service does,** before it uses it (`cli::check_service_dir`). It's the read-only half of the service's `acl::secure_dir`: the same inspection and the same verdict (`acl::verdict`), with the service's SID. It never creates the folder and changes no permission.
- **When it's checked:** whenever the service's folder is the one in use, from any executable (the MSI's or a portable one) and however it was named, mirroring the service's own rule (`windows::service` `load()`):
  - before anything is read: when `LODESTAR_DATA_DIR` or `worker --data` names it (compared as Windows compares paths), when the `--config` file is inside it, or when the MSI's executable reads `config.toml` there;
  - after loading, before any command opens the store: when the config's data folder is the service's, such as a `--config` from anywhere whose `data_dir` names it.
- **When it isn't:** a folder that isn't used isn't checked. `--config` or `LODESTAR_DATA_DIR` naming another data folder works from the MSI's executable whatever state the service's folder is in, as "What still wins" says, so `no store`'s advice to pass `--config` leads somewhere.
- **Not private:** the command refuses with exit status 3 and the service's own words (`acl::refusal`): what's wrong and the fix, the installer's Repair or `icacls`. Nothing in the folder is read.
- **Its permissions can't be read:** refused the same way, with a hint to run the command from an administrator's PowerShell.
- **Missing:** `no store at`, naming the folder; nothing is created.
- **Commands that write there:** `backup` (its `backups` folder, unless `--to`), `vacuum`, `reindex`, `rollback`, `rollup`, `import`, `settings` and `companion` add or change files in the service's folder. New files inherit the folder's permissions and belong to the administrator's default owner (normally Administrators), which the service's verdict accepts. `stats` and `import status` only read. The tester confirms on a VM that the service still starts after a `backup`.
- An administrator's PowerShell can open the folder, as before with `LODESTAR_DATA_DIR`; a standard one is refused by the folder's own permissions.

## When there's no store
- `no store at <db>` now goes on to name the data folder looked in, and how to point at another: `LODESTAR_DATA_DIR`, or `--config` with a `data_dir`.
- This is the same for the read-only commands (`stats`, `import status`), the commands that lock the store, and `reindex`.
- `import`'s own "start Lodestar once" refusal is unchanged.

## The guide
- **`docs/user/03-keep-lodestar-safe.md` §3.7** gets "Back up and restore on Windows":
  - backing up from an administrator's PowerShell with the MSI's `lodestar.exe`, with no variable, then copying the folder somewhere private;
  - deleting the copy left in the `backups` folder once it's safe elsewhere;
  - restoring as the Unraid and Compose steps now do: `Stop-Service`, removing the write-ahead files, copying the files back, `lodestar.exe reindex`, and `Start-Service`;
  - the same for a portable copy from its folder, and how to set `LODESTAR_DATA_DIR` in PowerShell.
- **"Rebuild the search index"** gets a Windows row: `reindex` from the MSI's executable needs no variable either.
- **`docs/operations.md` §2, "Commands from §10 on Windows":** the MSI needs no variable, and a command refuses a folder that isn't private; the variable stays for the portable ZIP run from elsewhere.
- **`docs/user/04-install-on-windows-and-linux.md`, "Install the MSI", step 4:** the log has a token line per start and only the newest works, so the command ends in `Select-Object -Last 1`.

## Tests
- **`cli::tests::the_msi_executable_finds_the_services_data_folder`**, with injected paths and environment and no real Program Files or ProgramData:
  - with nothing set, the service's folder;
  - its `config.toml` is read, and a `data_dir` there wins;
  - `--config` without a `data_dir` keeps the service's folder;
  - `LODESTAR_DATA_DIR` wins and the service's `config.toml` isn't read;
  - `worker --data` wins, and `worker` without it keeps `data`.
- **`cli::tests::the_services_folder_is_used_only_when_private`** (S8-07), with an injected `acl::Ops`: a folder a standard user made, and a private folder holding another user's file, are refused before its `config.toml` is read; a missing folder is `no store at` with nothing created; a private one is used.
- **`cli::tests::the_services_folder_is_checked_however_its_named`** (S8-07): `LODESTAR_DATA_DIR` naming it, from the MSI's and a portable executable; `worker --data` naming it; a `--config` from elsewhere whose `data_dir` names it, from either; and a config file inside it are all checked. `--config` or `LODESTAR_DATA_DIR` naming another folder is never checked.
- **`cli::tests::other_executables_keep_data_in_the_working_folder`:** a portable executable, one elsewhere in Program Files, no `%ProgramFiles%`, and no install at all keep `data`. The test binary itself is never the MSI's, on any platform.
- **`cli::tests::the_msi_executable_matches_whatever_its_case`** runs on every platform.
- **`cli::tests::no_store_says_where_it_looked`** checks the message.
- **The Windows-only code** (`Install::here`, the check with the real service SID) is compiled and linted for Windows by CI's Windows job.
- None of these is Windows-only: each runs the same on every platform.
