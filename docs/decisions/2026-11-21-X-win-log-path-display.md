# X-win-log-path-display decisions

From the Windows re-test (X-test-vm, on 0728f0a9). The Windows service's refusals in its temp log (`%SystemRoot%\Temp\lodestar-service.log`) showed paths with `Debug` quoting, `"C:\\ProgramData\\Lodestar"`, so a path copied from the log didn't work. UX only: no refusal, check or Event Log rule changes.

- **The temp log shows a path as Windows users read and paste it,** in plain quotes with single backslashes: `"C:\ProgramData\Lodestar"`, `"\\server\share\x"`, `"D:\My Lodestar\data"`. A new helper, `quoted()` in `windows/service.rs`, does this. It's used for every path the refusals name:
  - the default data folder from `%ProgramData%`;
  - `LODESTAR_DATA_DIR`;
  - `--config`;
  - the config file and its `data_dir`;
  - the restart witness folder, from the file or `LODESTAR_DHT_WITNESS_DIR`;
  - each `blocklist.files` entry;
  - the unusable data folder and the file that chose it.

  No other `{:?}` in `windows/*.rs` formats a user-facing path. `priority.rs` quotes a priority name, which isn't a path, so it's left as it was.
- **Nothing unprintable is written raw.** A value can come from a config file, so these stay escaped:
  - a control character: `\n`, `\r`, `\t`, `\0`, `\u{1b}`;
  - a `"`, as `\"`, so the quoting stays unambiguous;
  - every character `acl::invisible` lists (Unicode's Cf, Zl and Zp: bidi controls, zero-width characters, the BOM, line and paragraph separators), always as `\u{…}`, even where Rust's `Debug` would count one printable.

  Accents and other visible text stay as they are.
- **Both logs show the same path text.** The Event Log already showed the folder with `Display` and single backslashes (`event_log_text`). It keeps its own rules from X-event-log-followups: no quotes on `eventcreate`'s line, control characters flattened to spaces and invisible ones dropped by `one_line`, and a cut at 260 characters. So an ordinary folder now reads the same in both logs (tested: plain, UNC, spaces). Only an unprintable character differs: escaped in the temp log, which only administrators read, and flattened in the Event Log, which every local user can read.
- **The user guide is unchanged.** `docs/user` doesn't quote these messages; it names the log and shows how to read it.
- **Tests.**
  - `quoted()` is tested on plain, UNC, spaces, accented and empty values, on control and invisible characters, and on `"`.
  - Every refusal site gets a backslash value that is relative on any system, so it runs off Windows too.
  - The temp log and the Event Log are checked to show the same folder.
  - A Windows-only test asserted the TOML-escaped value (`\\w`) in the message. It now asserts the value as written, quoted.
  - These mutations fail a test (checked): the helper reverting to `Debug`, a call site reverting to `{:?}`, and dropping either escape.
