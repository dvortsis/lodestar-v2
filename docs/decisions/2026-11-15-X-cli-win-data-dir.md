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
- **Permissions:** the CLI only reads the service's folder. It doesn't run the service's `secure_dir`, and it changes no permission on ProgramData. An administrator's PowerShell can open the folder, as before with `LODESTAR_DATA_DIR`; a standard one is refused by the folder's own permissions.

## When there's no store
- `no store at <db>` now goes on to name the data folder looked in, and how to point at another: `LODESTAR_DATA_DIR`, or `--config` with a `data_dir`.
- This is the same for the read-only commands (`stats`, `import status`), the commands that lock the store, and `reindex`.
- `import`'s own "start Lodestar once" refusal is unchanged.

## The guide
- **`docs/user/03-keep-lodestar-safe.md` §3.7** gets "Back up and restore on Windows":
  - backing up from an administrator's PowerShell with the MSI's `lodestar.exe`, with no variable, then copying the folder somewhere private;
  - restoring with `Stop-Service`, removing the write-ahead files and the index, copying the files back, and `Start-Service`;
  - the same for a portable copy from its folder.
- **`docs/user/04-install-on-windows-and-linux.md`, "Install the MSI", step 4:** the log has a token line per start and only the newest works, so the command ends in `Select-Object -Last 1`.

## Tests
- **`cli::tests::the_msi_executable_finds_the_services_data_folder`**, with injected paths and environment and no real Program Files or ProgramData:
  - with nothing set, the service's folder;
  - its `config.toml` is read, and a `data_dir` there wins;
  - `--config` without a `data_dir` keeps the service's folder;
  - `LODESTAR_DATA_DIR` wins and the service's `config.toml` isn't read;
  - `worker --data` wins.
- **`cli::tests::other_executables_keep_data_in_the_working_folder`:** a portable executable, one elsewhere in Program Files, no `%ProgramFiles%`, and no install at all keep `data`. The test binary itself is never the MSI's, on any platform.
- **`cli::tests::the_msi_executable_matches_whatever_its_case`** runs on every platform.
- **`cli::tests::no_store_says_where_it_looked`** checks the message.
- None of these is Windows-only: each runs the same on every platform.
