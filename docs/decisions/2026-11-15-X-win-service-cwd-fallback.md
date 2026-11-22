# X-win-service-cwd-fallback decisions

A follow-up from X-win-service-path-hardening (#936), queued by the runner. Calls not marked as the runner's are this PR's, and the runner flags them to the owner (`docs/owner-decisions.md` §2.19).

## The service no longer reads its working folder's config

- **Before:** the service the SCM starts (`windows::service::load_guarded`) could read a `config.toml` it shouldn't have. With no `config.toml` in its own folder and no `LODESTAR_DATA_DIR`, it called the config loader with no file. The loader then fell back to `data\config.toml` in the process's working folder, which is the system folder under the SCM. That file wasn't covered by the folder check.
- **Now:** in that case the config is loaded as if pointed at the service's own folder, exactly as the CLI's MSI branch already does (`cli::load_config`). So only `config.toml` in the service's folder is ever looked for, and the working folder is never read.
- **Nothing else moves:**
  - The folder isn't counted as set by the environment.
  - A `data_dir` in a config file, `LODESTAR_DATA_DIR` and `--config` still choose as before.
  - The folder check runs in the same order, with the same verdict.
- **Not covered:** a service folder whose path isn't valid UTF-8 keeps the old load, as the CLI's branch does. `%ProgramData%` is a system path, so this isn't expected to happen.
- **Behaviour an administrator sees:** none. A service that relied on a `config.toml` in the system folder would now ignore it. Lodestar never documented putting one there.

## Tests

- **`windows::service::tests::the_service_never_reads_its_working_folders_config`** runs on every platform. The working folder belongs to the process, so the test re-runs its own test binary as a child process.
  - The child starts in a folder holding a `data\config.toml` that sets `http.bind`, and loads the service's config there.
  - It checks that the config file's setting wasn't read, that the data folder is the service's, and that `LODESTAR_DATA_DIR` isn't counted as set.
  - The parent also checks that the child actually ran the test.
  - It fails with the old load: "the working folder's config.toml was read".
- **The existing order and folder tests are unchanged** (`the_service_secures_its_folder_before_reading_from_it`).
