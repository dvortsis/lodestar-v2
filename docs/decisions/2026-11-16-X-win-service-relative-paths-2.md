# X-win-service-relative-paths-2 decisions

A follow-up from the review of X-win-service-relative-paths (#959, finding O1), queued by the runner. Calls not marked as the runner's are this PR's, and the runner flags them to the owner (`docs/owner-decisions.md` §2.19).

## The service refuses every relative path setting
- **Before:** #959 refused a relative data folder and config file. The service still resolved three other path settings against its working folder, which is the system folder under the SCM. For example, `restart_witness_dir = "w"` meant `System32\w`.
- **Now:** the service refuses these too, with the same refusal as #959 (`Refusal::NotFullPath`, Event Log ID 5, exit code 3):
  - `dht.restart_witness_dir` in the config file;
  - `LODESTAR_DHT_WITNESS_DIR`, which wins over the file, as `Config::load` reads it;
  - each `blocklist.files` entry, named by its place in the list (`entry 2`) and its value.
- **When:** in `windows::service::load_guarded`, once the config is loaded, before the after-load folder check. That's before any of these paths is created, opened or made private: `apply_witness_default` and the blocklist read both run later.
- **The temp log** names the setting, its value and the config file, and gives the fix. **The Event Log** text stays fixed and holds no value (Legal Notice §5). It now says "a path setting, such as LODESTAR_DATA_DIR, --config or a folder or file in its config file", rather than listing every setting.
- **The CLI and the portable install are unchanged (runner's brief).** They keep relative paths.

## What counts as chosen (this PR's call)
- **An empty `LODESTAR_DHT_WITNESS_DIR` chooses nothing,** as `Config::load` reads it, so it isn't refused.
- **The loader's native default isn't checked.** When neither the file nor the variable chooses a witness folder, `Config::load` fills in one from `%LOCALAPPDATA%`. The service never uses it: with the restart proof on, `apply_witness_default` replaces it with `%ProgramData%\Lodestar-witness`, and with the proof off there's no witness. So a relative `%LOCALAPPDATA%` doesn't stop the service.

## Every path setting is now covered
- **The config's path settings** are `data_dir`, `dht.restart_witness_dir` and `blocklist.files`, plus the variables `LODESTAR_DATA_DIR` and `LODESTAR_DHT_WITNESS_DIR`. With `--config` and `%ProgramData%` from #959, the service now refuses a relative value for each of them.
- **Not path settings:** `LODESTAR_ACL_PATH` is set by Lodestar itself for its own PowerShell step, never read from the config. Blocklist subscriptions are URLs. Settings saved in the app live in the store, inside the data folder.
- **The `load_guarded` doc comment** says "no path setting" again, as the runner asked.

## Tests
- **`windows::service::tests::the_service_refuses_other_relative_path_settings`** runs on every platform:
  - `w`, `./w` and `../w` as `dht.restart_witness_dir`, as `LODESTAR_DHT_WITNESS_DIR` over a full path in the file, and as the second `blocklist.files` entry after a full one. Each is refused with `NotFullPath`, naming the setting and the value.
  - On Windows only, `\w` and `C:w` too: they're relative there, and plain names elsewhere.
  - Full paths load as before, and an empty variable chooses nothing.
  - It fails with the new check removed (checked by disabling it).
