# Logging, redaction, settings and configuration review
Reviewed at 55e155a9.

Nothing was built or run (the brief forbids it). Every finding below comes from reading the code path end to end; "confirmed" means traced in the source, not executed.

## Coverage
- Read in full (non-test code):
  - `crates/log/src/lib.rs`, `value.rs`, `dispatch.rs`, `event.rs`
  - `crates/log/src/url.rs` lines 1-1480: both entry points (`scrub`, `scrub_text`), all of stage 1, `only_more`, the look-alike readings, and the wide stage's typed-URL path (`wide::scrub_with`, `finish`, `scrub_path`)
  - `crates/lodestar/src/logging/`: `mod.rs`, `layer.rs`, `format.rs`, `entry.rs`, `file.rs`, `names.rs`, `files.rs` (to line 400), `view.rs`, `source.rs`, `state.rs`, `ring.rs`, `history.rs` (`page_capped`, `locate`)
  - `crates/lodestar/src/settings/`: `mod.rs`, `log.rs`, `logging.rs`, `notifications.rs`, `integrations.rs`, `crawler.rs` (to line 230), `network.rs`, `transport.rs`, `storage.rs`, `search.rs`, `general.rs`, `trackers.rs`, `content.rs`, `updates.rs` (to line 290), `insights.rs`, `privacy.rs`, `watch.rs` (to line 260), `portable.rs`, the checks of `preferences.rs`
  - `crates/lodestar/src/config.rs` (to line 1056), `crates/lodestar/src/cli/settings.rs` (to line 757)
  - `crates/api/src/logs.rs`, `crates/api/src/logs/stream.rs` (to line 300), `crates/api/src/log_events.rs`
  - Followed into: `crates/store/src/settings.rs`, `crates/api/src/auth/http.rs` (`protected`), `crates/api/src/settings/checks.rs` (saved-secret use), `crates/lodestar/src/services.rs:99-164`, `crates/lodestar/src/main.rs` (error printing), `crates/lodestar/src/windows/temp_log.rs`
- Skimmed: `logging/read.rs`, `zip.rs`, `fold.rs`, `refs.rs`, `preview.rs` (outlines only); `crates/api/src/logs/params.rs` (not opened; its use in `logs.rs` read); test files (`settings/tests.rs`, `logging_tests.rs`, `api_tests.rs`, `logging/tests.rs`) by grep, to see what is and isn't pinned.
- Not read: `crates/log/src/url.rs` 1480-2996 (the wide stage's free-text rules: `scrub_pass`, magnet spans, `normalize_separators`, the wide `scrub_debug_forms` and `secret_cuts`) and its 1,600 lines of tests. It is heuristic code that five recorded review rounds have already worked over (`docs/decisions/2026-11-04-X-log-url-redact.md` to `-5.md`); I judged the settings load path a better use of the time. `logging/events.rs` (catalogue declarations), `logging/bench.rs`, `settings/screen_tests.rs`, `crates/lodestar/src/windows/logfile.rs` (outside the area; see the last area-level concern).

## Findings

### LOGSET-01 Saved settings with a cross-field check are dropped at every start (logging `data` preset, `file_size_mib`)
- Severity: blocking
- Kind: correctness
- Confidence: confirmed
- Where: `crates/lodestar/src/settings/mod.rs:786-852` (`Entry::new` validates saved rows one at a time), `crates/store/src/settings.rs:37-40` (`ORDER BY key`), `crates/lodestar/src/settings/logging.rs:550-559` (`data` / `data_until`), `:572-575` (`file_size_mib` / `file_total_mib`)
- What: a save checks the whole patch on the section it makes (`plan`, `mod.rs:1001-1014`). The load at start instead validates each saved row alone, in key order, against `base` plus only the rows read so far, and runs the same `check(field)`. A field whose check depends on a field that sorts after it is judged against that other field's default and skipped as `invalid` (`settings.saved_ignored`). The row stays in the database, so it happens at every start.
  - `settings.logging.data` sorts before `settings.logging.data_until`. `check("data")` refuses `data != private` while `data_until == 0`; at load `data_until` is still the default 0.
  - `settings.logging.file_size_mib` sorts before `file_total_mib`. `check("file_size_mib")` refuses a size over the total; at load the total is still the default 256.
- Failure scenario: the admin chooses data "Hashes, until I turn it off" (`{"data":"hashes","data_until":-1}`: accepted, applied live). After any restart or update the section loads `data = private` with `data_until = -1`. Logs go back to Private, the Logging screen shows Private as a default, `lodestar settings get logging.data` says `[default]`, and only a `settings.saved_ignored reason=invalid` line says why. A timed "Full for 24 hours" whose deadline is still ahead is lost the same way. `file_size_mib = 512, file_total_mib = 1024` loads as 16 / 1024. The direction is fail-closed for privacy, but a saved setting is silently lost and "until turned off" can't survive a restart.
- Suggested fix: at load, first collect every row that passes its type and range check into one map, then run each field's `check` on the complete section (as `plan` does) and drop only the fields that still fail. Add a reload test per section with a cross-field check: save the pair, load a second `Registry` over the same store, compare.
- Tracked?: not found (`runner/slices.yaml`, `docs/decisions/*settings*`, open PR titles). `saved_values_survive_a_restart` (`settings/tests.rs:311`) covers `crawler` only. `settings/watch.rs:10-21` shows the authors know `check` runs on saved values at load (rv-765 B1), but not this ordering effect.

### LOGSET-02 The logs folder can exceed its total cap by up to one file's size
- Severity: important
- Kind: resource
- Confidence: confirmed
- Where: `crates/lodestar/src/logging/file.rs:887-921` (`prune`), `:651-657` (the write-time check), `:222-223` and `:590-600` (when `prune` runs); spec `docs/architecture.md:1426` ("the logs folder never goes over a total cap"), `settings/logging.rs:207` ("never goes over this")
- What: `prune` trims rotated files until `current + rotated <= total`, but it runs only after a rotation's compression, at open and when the limits change, which is when the current file is nearly empty. The current file then grows to `file_size_mib` with no further pruning. The write-time check compares only the current file's own size with the cap (`self.size + len > self.cfg.total`), not the folder's. `check` allows `file_total_mib == file_size_mib`.
- Failure scenario: `file_total_mib = 16` (the minimum the form takes) with the default `file_size_mib = 16`: after a rotation `prune` keeps compressed files up to 16 MiB (seven of them at a typical 1-2 MiB each come to 7-14 MiB), then the new current file fills to 16 MiB, so the folder holds 23-30 MiB against a 16 MiB cap. At the defaults (16 / 256, 7 files) the overshoot can't happen in practice, because seven compressed files stay far under 240 MiB. `keeps_the_count_the_age_and_the_cap` (`file.rs:1150-1158`) only checks that the folder fits at the moment `prune` runs.
- Suggested fix: in `prune`, reserve the current file's limit: trim while `rotated + max(current, cfg.size) > cfg.total`. Then `total == size` keeps no rotated file, which is what the cap asks for.
- Tracked?: not found. `docs/decisions/2026-10-23-B-log1.md:60` records only the current-file stop (`logging.over_cap`).

### LOGSET-03 Three settings groups are saved and shown as live, but nothing reads them
- Severity: important
- Kind: gap
- Confidence: confirmed (by grep for every consumer; only the registry, the test endpoint and the privacy banner touch them)
- Where: `crates/lodestar/src/settings/notifications.rs:86-95, 255-257` (`route_alerts`, `route_updates`, `route_health`), `settings/updates.rs:86-93` (`notify_before`, `notify_after`, `notify_failure`), `settings/insights.rs:55-67` (`digest`, `digest_day`, `digest_hour`, `digest_channels`, `query_logging`, `query_retention_days`), `settings/privacy.rs:30-33`, `crates/api/src/insights.rs:1879`
- What:
  - No code subscribes to `Notifications` outside `POST /settings/test`. Alerts, update news and health problems are never delivered to ntfy, Discord, email or the webhook, whatever the routes say. The module doc says "delivery reads them" (`notifications.rs:2-3`).
  - The Updates section's three "Notify me" switches are read by nothing; `notify_after` and `notify_failure` default to on.
  - The Insights section's digest schedule and channels are read by nothing. `query_logging` gates nothing (no search writes the query log), and the 30-day prune doesn't read `query_retention_days`.
  - The Overview's privacy summary still reports `query_logging: true` as "searches are being saved" when nothing is saved.
  - The UI presents all of these as working settings. Only the Transport fields carry a "not active yet" label (`web/src/lib/features/settings/messages/en.json:129-132`); Notifications reads "Where alerts, updates and health messages go."
- Failure scenario: an admin sets up ntfy, passes "Send test", routes Health to it and relies on it for a full disk or a stalled crawler. No message ever arrives.
- Suggested fix: either wire a deliverer (and the query log) or label these fields "not active yet" as Transport's are, and have the privacy summary say "saved" only when something is. Add a slice so it isn't lost.
- Tracked?: recorded as follow-ups in `docs/decisions/2026-10-31-B-settings-sections-b.md:23-25` ("notification routing has no deliverer"; "nothing writes the query log … the 30-day prune doesn't read `query_retention_days`"). I found no slice for either in `runner/slices.yaml`, and `docs/build-plan.md:122-126` (Track M7's remaining work) doesn't list them. The UI wording isn't covered by that record.

### LOGSET-04 `settings/log.rs` tests wait with fixed sleeps and then assert the work finished
- Severity: important
- Kind: test
- Confidence: confirmed
- Where: `crates/lodestar/src/settings/log.rs:327-332` (300 ms, then asserts the mask and that the file opened), `:351-352` (100 ms, then asserts a negative that also holds before the task has run, so it can't fail), `:356-358` (200 ms, then asserts the save applied); `:333` (the polling half uses a 5 s deadline)
- What: `deadlines_expire_without_a_save` and `a_save_applies_at_once` sleep a fixed time and assume `follow_with`'s spawned task has applied the section. `runner/standards.md` §2 makes this a blocking review finding: poll for the condition with a deadline of at least 10 s.
- Failure scenario: a loaded Windows runner schedules the spawned task after the sleep; the assertion fails with nothing wrong in the code.
- Suggested fix: poll `rt.applied()` for the expected mask with a 10 s deadline in all three places, and raise the 5 s deadline to 10 s.
- Tracked?: PR 778 (X-test-blockers-1112) is about test flakes. I didn't open it, so whether it covers these two tests isn't confirmed.

### LOGSET-05 `config.toml` values bypass the ranges the app enforces for the same settings
- Severity: polish
- Kind: gap
- Confidence: confirmed
- Where: `crates/lodestar/src/config.rs:704-818` (`Config::load` checks only `ranking`, `updates`, `cluster`), `:870-872` (`max_metadata_mib << 20`), `settings/mod.rs:764` (`base` is never range-checked), `settings/crawler.rs:99-121`, `storage.rs:29`, `search.rs:35`
- What: the registry bounds `max_sockets` (0-4096), `timeout_secs` (1-120), `max_metadata_mib` (1-64), `mmap_mib` (0-65536) and `heap_mib` (16-8192) for values saved in the app. The same keys in `config.toml` are taken as written, and `crates/peer-wire/src/fetch.rs:493` has no ceiling of its own beyond `opts.max_metadata`.
- Failure scenario: `[fetch] max_metadata_mib = 4096` makes every fetch accept a 4 GiB info dict from an untrusted peer, up to the socket count at once. The Settings screen then shows 4096 in a field whose schema says the maximum is 64. The operator wrote the value, so this is a guard rail, not a hole.
- Suggested fix: run each section's range check on the file's value in `Config::load` (as `Updates::check` and `Cluster::check` already do) and refuse to start with the key named.
- Tracked?: partly acknowledged for Trackers only (`docs/decisions/2026-10-31-B-settings-sections.md:23`, "bounds … are wide, so a `config.toml` value doesn't fall outside the schema"). Not found for the crawler, storage or search keys.

### LOGSET-06 The current log file's age is counted from process start, so restarts keep lines past the maximum age
- Severity: polish
- Kind: correctness
- Confidence: confirmed
- Where: `crates/lodestar/src/logging/file.rs:569` (`opened: SystemTime::now()`), `:681-690` (`due`), `:595-599` (the only check at open: the file's mtime), `:902-907` (`prune` ages rotated files by mtime)
- What: the age rotation fires when this process has had the file open for `file_max_age_days`. At open, the file is rotated only if nothing was written to it for that long. A file that is appended to across restarts never rotates by age. Separately, a rotated file left uncompressed by a previous run is compressed at the next start and gets a fresh mtime, which restarts its retention clock.
- Failure scenario: verbosity "Errors", a 16 MiB file that takes a year to fill, and a container that restarts weekly: `lodestar.jsonl` holds lines a year old although the maximum age is 14 days. The module doc's promise is "no file outlives the maximum age by much" (`file.rs:1-4`).
- Suggested fix: take the current file's age from its first line's `ts` (already read by `first_ts`) or keep the open time in `index.json`; age rotated files by the stamp in their name.
- Tracked?: not found.

### LOGSET-07 An unusable logs folder is retried and reported every heartbeat, with an uncapped side log on Windows
- Severity: polish
- Kind: resource
- Confidence: likely (traced in the code; the Windows branch not exercised)
- Where: `crates/lodestar/src/logging/mod.rs:365-382` (`apply` reopens whenever the state is `Off` and the file is enabled), `:453-456` (a failed open sets `Off`), `crates/lodestar/src/settings/log.rs:192-214` (`rt.apply` every loop, at most 60 s apart), `logging/file.rs:509-516` and `windows/temp_log.rs:49-70` (one appended line per failure, no cap)
- What: `FileState::Off` means both "turned off in the settings" and "couldn't be opened". `follow` calls `rt.apply` at every heartbeat, so a folder that can't be opened is tried again every minute, and each failure logs `logging.file_unavailable` at warn. On Windows, when a new folder's ACL can't be restricted, each try also creates and removes the folder and appends a line to `%SystemRoot%\Temp\lodestar-service.log`, which nothing trims.
- Failure scenario: `file_folder` points at a read-only mount: 1,440 warnings a day on the console and in the live log, for as long as it lasts.
- Suggested fix: keep a separate `Unavailable` state with a backoff, report the first failure and each change of reason only, and cap or rotate the service log.
- Tracked?: not found.

### LOGSET-08 Config parse errors go to stderr unscrubbed, with the offending line
- Severity: polish
- Kind: security
- Confidence: likely (toml 0.9's error `Display` includes the source line; not run)
- Where: `crates/lodestar/src/config.rs:720-724`, `crates/lodestar/src/main.rs:156, 170` (`eprintln!("config: {e}")`), against `main.rs:418-429` (`command_failed` scrubs with `scrub_text`)
- What: a TOML syntax or type error is printed with the parser's excerpt of the line at fault. `config.toml` can hold URLs with credentials (`[[subscriptions.lists]] url`, an HTTP tracker's passkey in `scrape.trackers`). `health_config` already names a tracker by position for this reason (`config.rs:982-983`), and `command_failed` scrubs its text, but the load path does neither.
- Failure scenario: an unterminated string on a `url = "https://user:pass@…` line prints that line into `docker logs`.
- Suggested fix: pass the message through `lodestar_log::url::scrub_text` before printing, as `command_failed` does.
- Tracked?: not found.

## Checked and found sound
- `/logs` routes are session-only (`crates/api/src/auth/http.rs:101-122`); API keys and feed tokens never reach them. Delete also needs a same-origin JSON request.
- First-party code has no plain `tracing` call left (grep finds none outside tests), so every first-party line is a typed `log_event!` whose fields render under the output's mask. `.github/scripts/log_lint.py` holds that in CI.
- Third-party lines pass `scrub_text` on the message and on every field before any output sees them (`logging/layer.rs:45-63`), and again when an older file is read back (`view.rs:348-351, 361-364`).
- Panics: the hook logs only the source location, never the message, and writes a fixed line straight to stderr when the panicking thread is inside the sink (`settings/log.rs:61-77`).
- Log injection: control characters, ESC, bidi overrides and U+2028/2029 are escaped on the text line and in the JSON line (`logging/format.rs:96-167`); the text download of a file escapes field names and values too (`view.rs:454-487`). SSE frames carry compact JSON, so a value can't start a frame line.
- Log file names from the request path, cursors and `index.json` are parsed into `LogName` and every path is built from the typed value (`names.rs:31-68`). Files are opened `O_NOFOLLOW | O_NONBLOCK` and only when regular; a download must also be in the folder's listing (`source.rs:247-252`).
- Timestamps are UTC RFC 3339 from the Unix clock with no time-zone database (`format.rs:28-40`); rotated names use the same stamp.
- Log files are created 0600 with `O_NOFOLLOW`, the folder 0700; a world-writable folder without the sticky bit is refused (`file.rs:410-491`).
- Reads are bounded: 2 file reads at once on the blocking pool (`api/src/logs.rs:379-398`), a 64 MiB read cap per history page, 1 MiB lines, a capped inflate (`files.rs:273-299`), 8 streams with 2 per session and a 256-record lag bound. The text filter is a substring, not a regex, matched after masking (`view.rs:209-227`), so it can't probe hidden values.
- The ring keeps only rendered values, purges a kind's values when it turns off, and applies deadlines at read time (`mod.rs:734-741`, `ring.rs:174-207`, `381-400`).
- Opt-ins never reach the console (`state.rs:87-90`, `mod.rs:867-868`) and can't be set to "until turned off" or more than 24 hours ahead (`settings/logging.rs:328-340, 542-549`), through the API, an import or the CLI alike.
- The setup token is `[redacted]` in the ring, the stream, history and redacted downloads (`entry.rs:315-331`, `view.rs:326-329, 365-368`).
- Settings saves are one SQLite transaction with the section revisions; in-memory state changes only after the commit, under one write lock (`settings/mod.rs:1590-1683`, `store/src/settings.rs:105-122`). The CLI goes through the same registry on a copy and writes back in one transaction.
- Secrets: sealed at rest with a context bound to section, field and item (`mod.rs:463-473, 543-566`); never returned (`redact`, `values_of`), exported or imported (`portable.rs:55-57, 223-224`); error messages for secret-holding fields never quote the value (`mod.rs:744-750, 987-1000`); `Debug` of `SecretText`, `SavedClient`, `Entry` and `Sealer` hides them. A saved secret is reset when its endpoint moves and is handed to a connection test only for the saved endpoint (`mod.rs:969-985, 1247-1293`).
- Tracker passkeys in `trackers.trackers`: returned and exported by recorded decision (`docs/decisions/2026-10-31-B-settings-sections.md:23`), and the field's help text tells the user not to put one there. Errors name a tracker by position.
- Environment values win over saved ones and are read-only in the app (`mod.rs:864-881, 943-948`); `secure_cookies` and `data_dir` are read-only everywhere but the file and environment.
- The 18+ confirmation holds through PATCH, batch, import (`portable.rs:262-266`) and the CLI (`cli/settings.rs:373-381`).
- Settings API calls that touch the database run on the blocking pool (`crates/api/src/settings.rs:394`).
- `restart_required` is derived at each read from the values the process started with (`mod.rs:899-901`), not stored, so the registry can't lose it. I did not review the restart banner's own state (`system::Restart`, PR 790).

## Area-level concerns
- **The settings loader and the save path check differently.** LOGSET-01 is the instance that bites today, but any later section that adds a check comparing two fields inherits it, with the outcome decided by the alphabetical order of its field names. Notifications and Integrations escape only by luck of naming (`email_password` before `email_username`, `clients` before `default_client`).
- **Every line is written synchronously under process-wide mutexes on the caller's thread** (`mod.rs:478-522`): the console write and flush, the fold tables, the ring and the file. A blocked stderr (a log driver applying backpressure, a paused console) stalls every thread that logs, tokio workers and the DHT actor included. This is the usual `tracing` trade-off and I found no record of it being weighed; it matters most at Detailed and Everything.
- **`crates/log/src/url.rs` is 3,000 lines of heuristics in two deliberately duplicated stages** (`docs/decisions/2026-11-05-X-log-url-redact-2.md`), with `only_more` joining them by word. It is the one place I could not review in full. Its typed-URL callers are few and low-risk (subscription list URLs, and notifier URLs that `lodestar_integrations` redacts first), and its free-text use is limited to third-party lines at warn by default, so the exposure is small; the maintenance cost is not. A short free-text line that doesn't take the fast path is read four or more times.
- **A write that fails part-way for a reason other than a full disk** (`file.rs:671-678`) leaves a partial line with no newline and doesn't count its bytes; the next line is glued to it. Readers count the merged line as unreadable, so nothing leaks, but two records are lost and the size accounting drifts until the next rotation.
- **`ENV_VARS` says it lists every variable `Config::load` reads** (`config.rs:97-120`) but six `LODESTAR_DHT_*` experiment switches are read and not listed (`config.rs:765-792`). They back no settings field today, so nothing shows a wrong source; a field added for one of them later would.
- **Suspected, outside my area and not traced:** on a Windows service the console is a file, `<data>\logs\lodestar.log` and `.1` (`windows/logfile.rs`). With `console_detail = "file"` it would hold torrent details, yet it isn't a `LogName`, so the Logs screen's listing, total cap, age limit and "delete files with details" wouldn't cover it. The platform reviewer should confirm or dismiss this.
- **Test gaps:** no test reloads a registry after saving a cross-field pair; the cap test checks only the instant `prune` runs; nothing asserts that a routed notification is delivered (there is nothing to assert against yet).
