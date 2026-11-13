# Platform, lifecycle and operations review
Reviewed at 55e155a9.

## Coverage
- Read in full (non-test code): `crates/lodestar/src/main.rs`, `services.rs`, `privdrop.rs`, `system.rs`, `system/restart.rs`, `system/address.rs`, `system/info.rs` (sizes, memory, free space), `windows/mod.rs`, `windows/service.rs`, `windows/acl.rs`, `windows/ports.rs`, `windows/process.rs`, `windows/temp_log.rs`, `windows/logfile.rs`, `storage.rs`, `fsdetect.rs`, `thp.rs`, `memstats.rs`, `floor.rs`, `ops.rs`, `cli/mod.rs`, `cli/lock.rs`, `cli/util.rs`, `cli/backup.rs`, `cli/vacuum.rs`, `cli/reindex.rs`, `cli/stats.rs`, `cli/rollup.rs`, `Cargo.toml`, `rust-toolchain.toml`, `crates/lodestar/Cargo.toml`, `crates/store/Cargo.toml`, `crates/store/src/memstats.rs` (the workspace's only `unsafe`).
- Skimmed: `metrics.rs` (label construction, counter/gauge types, a scripted cross-check of every family name against `docs/metrics.md`; not every help string), `insights.rs` (host checks, the recorder loop and its stop; not the compaction or coverage arithmetic), `cli/companion.rs` (first 230 lines), `cli/update_helper.rs` (first 120 lines), `http.rs` (router and `/metrics`, `/healthz` only, to check auth), `update/supervisor.rs` (lock, signals and reaping only, for PID 1 behaviour), `packaging/windows/main.wxs` and `secure-datadir.ps1` (only what the service code depends on), `packaging/systemd/lodestar.service`, the test modules of the files above (for sleeps and deadlines).
- Not read: `windows/priority.rs` (143 lines, out of time), `ops/tests.rs` and `system/address/tests.rs` beyond their waits, the test halves of `restart.rs`, `service.rs`, `acl.rs`, `metrics.rs`, `insights.rs`. Nothing in this area was built or run; Windows behaviour is read from code only.

## Findings

### PLAT-01 A start that fails after the pipeline is running drops everything without a shutdown
- Severity: important
- Kind: correctness
- Confidence: confirmed
- Where: `crates/lodestar/src/services.rs:204` (`open_hot`), `:312-318` (auth load), `:408` (`Pipeline::start`), `:417-453` (cluster port, `Workers::open`), `:461-468` (`Control::start`), `:693-694` (HTTP bind); `crates/lodestar/src/main.rs:299-311`, `:237-246`; `crates/pipeline/src/lib.rs:1710-1723`; `crates/lodestar/src/windows/service.rs:1068-1093`
- What: `Services::start` opens the hot columns (which clears the snapshot's stamp, `lib.rs:1723`), starts the pipeline, and only then does the steps most likely to fail: the cluster port and the HTTP listener. Every later `?` returns the error and drops the running pipeline, index and tasks. Nothing calls `Handles::shutdown`; `main` then ends the runtime after 5 s.
- Failure scenario: the HTTP port is taken (a second container, a stale process, a mistyped `http.bind` in `config.toml`). Each start crawls for a moment, exits 1 with no queue, DHT state or hot snapshot saved, and the next start rebuilds the hot columns from SQLite and runs the unclean-stop repair. Under a restart policy this repeats every few seconds. On the Windows service the ledger stays `running`, so the next good start records `unclean_stop` and Restart now is paused for 25 hours.
- Suggested fix: bind the HTTP and cluster listeners before `open_hot` and `Pipeline::start` (the pending-address path already binds early), or run the shutdown for whatever has started when a later step fails.
- Tracked?: not found.

### PLAT-02 The Windows service's folder check ignores the permissions of the entries inside it
- Severity: important
- Kind: security
- Confidence: likely (read from code; not run on Windows)
- Where: `crates/lodestar/src/windows/acl.rs:297-321` (`verdict`), `:531-543` (`INSPECT_SCRIPT`); compare `packaging/windows/secure-datadir.ps1:169-185` (`Get-InsideProblem`)
- What: at every start `verdict` checks the folder's owner and DACL, and for each top-level entry only its owner and whether it is a link. The entry's SDDL is fetched but its `D:` part is never checked. The installer does check every item's own DACL, so the two are out of step, which the module header and `docs/decisions/2026-10-22-M6-win.md` say must not happen. Windows grants "Bypass traverse checking" to Everyone by default, so a file inside a private folder is openable by full path when its own DACL allows it.
- Failure scenario: an administrator restores `auth.json`, `config.toml` or `lodestar.db` by moving it within the volume from a folder Users can read or write. A move keeps the file's ACL, the owner is Administrators, and the service starts. Any local user can then read the password hash, session key and API keys, or rewrite `config.toml`.
- Suggested fix: apply the installer's rule in `verdict`: each entry's allow entries only for the trusted SIDs, OWNER RIGHTS and CREATOR OWNER (`log_folder_private` already implements that rule).
- Tracked?: not found. The decision's "What remains" lists hard links and deeper owners only.

### PLAT-03 `lodestar backup` on Windows writes `auth.json` and `seal.key` with whatever permissions the target folder has
- Severity: important
- Kind: security
- Confidence: confirmed (code); impact depends on the folder chosen
- Where: `crates/lodestar/src/cli/backup.rs:137-159` (`create_private_dir`, `restrict`), `crates/lodestar/src/cli/util.rs:232-249` (`create_new_private`, `copy_new_private`)
- What: the backup folder is 0700 and its files 0600 on Unix. On Windows both helpers do nothing, so the folder and files inherit the parent's ACL. `lodestar_integrations::secret::restrict_dir_acl` exists and is used for the logs folder.
- Failure scenario: `lodestar backup --to D:\Backups` (or the portable build's default `data\backups` under a shared folder). The copy of `auth.json` (password hash, sessions, API keys), `seal.key` and the store is readable by every account the parent folder lets in, typically all local users on a data drive.
- Suggested fix: call `restrict_dir_acl` on the new backup folder before anything is written into it, and fail the backup if that fails.
- Tracked?: not found. `docs/decisions/2026-10-22-M6-cli.md` records the Unix modes only.

### PLAT-04 The portable Windows build stops uncleanly when its window is closed or Windows shuts down
- Severity: important
- Kind: gap
- Confidence: confirmed (no handler exists); impact likely
- Where: `crates/lodestar/src/main.rs:363-382` (`shutdown_signal`)
- What: off Unix only `tokio::signal::ctrl_c()` is awaited. Nothing handles the console's close, log-off or shutdown events (`tokio::signal::windows::ctrl_close`, `ctrl_logoff`, `ctrl_shutdown`, `ctrl_break`); a search of `crates/` finds none.
- Failure scenario: a portable user closes the console window, signs out or restarts the PC. The process is killed with no graceful shutdown: the hot snapshot, fetch queue and DHT state are not saved, and the next start rebuilds from SQLite. The docs and `Start Lodestar.cmd` ask for Ctrl+C first, but closing the window is the usual way to stop a console program.
- Suggested fix: add the close, log-off and shutdown events to `shutdown_signal`. Windows allows only a few seconds after them, so save the cheap state first.
- Tracked?: not found.

### PLAT-05 On Unix the data directory and the store are created with the default umask
- Severity: important
- Kind: security
- Confidence: confirmed (code); exposure only outside Docker and the shipped systemd unit
- Where: `crates/lodestar/src/services.rs:103-104`, `crates/lodestar/src/privdrop.rs:518-520` (both `create_dir_all`); contrast `crates/lodestar/src/cli/backup.rs:137-159`
- What: the data directory is created 0755 under a normal umask and SQLite creates `lodestar.db` 0644. The key files and the logs folder are made private individually, and the backup restricts its copy of the store "since backups hold secrets", but the live store is not restricted.
- Failure scenario: the Linux tarball run as `./lodestar serve` on a shared host. Other local users can read `data/lodestar.db`: the query log, saved searches and settings. The systemd unit avoids it (`StateDirectoryMode=0700`, `UMask=0077`).
- Suggested fix: create the data directory 0700, and narrow an existing one that Lodestar owns, as `logging/file.rs` does for logs.
- Tracked?: not found.

### PLAT-06 Sixty of the 164 `/metrics` families have no row in `docs/metrics.md`, and no test checks the rest
- Severity: polish
- Kind: docs
- Confidence: confirmed (scripted name check)
- Where: `crates/lodestar/src/metrics.rs:60-360`, `:851-1050`; tests at `:1710-1745` cover only the memory, segment and held-bytes families
- What: the families from the early slices (`dht_*` basics, `intake_*`, `writer_*`, `leases_*`, `scrape_*`, `search_*` counters, `tracker_*`, `source_up`, `have_*`, `fetch_active` and others) are rendered but not documented. Later sections were added per slice.
- Failure scenario: a dashboard or the soak analyzer relies on a name that is later renamed with no contract to catch it.
- Suggested fix: one test that every family `render*` emits has a row, and add the missing rows.
- Tracked?: not found.

### PLAT-07 `/stats.disk_free_bytes` is always null although the free-space read now exists, and nothing warns of a filling disk
- Severity: polish
- Kind: gap
- Confidence: confirmed
- Where: `crates/lodestar/src/ops.rs:163-166`; `crates/lodestar/src/system/info.rs:254-270` (`free_bytes`, `statvfs` and `fs4`)
- What: the comment says std cannot read free space and a new dependency needs approval. `system/info.rs` already reads it for `/system` on Unix and Windows. `docs/metrics.md:422` records "always null in this build", so this is a known deferral whose reason has gone. Insights' host exceptions cover the filesystem type and the port range, not free space.
- Failure scenario: Insights' Index stage shows a dash for free disk space, and a crawler that grows until the volume is full gives no warning first.
- Suggested fix: reuse `info::free_bytes` in `stats_now`, and consider a low-space host exception.
- Tracked?: `docs/metrics.md` §7.6 only; no slice found.

### PLAT-08 Several tests in this area wait with deadlines under the 10 s the standard requires
- Severity: polish
- Kind: test
- Confidence: confirmed
- Where: `crates/lodestar/src/system/restart.rs:966` (2 s), `:972` (1 s); `crates/lodestar/src/system/address/tests.rs:360`, `:650`, `:685`, `:808` (5 s each); `crates/lodestar/src/ops/tests.rs:302` (1 s, a settle bound)
- What: `runner/standards.md` §2 asks for a deadline of at least 10 s so tests pass on the slowest runner. These wait on spawned tasks or blocking store writes with 1 to 5 s. The fixed sleeps at `ops/tests.rs:278,872` and `address/tests.rs:657,681` are negative checks and are commented as such.
- Failure scenario: a loaded Windows runner takes over 5 s to run the follower's blocking save, and `the_follower_records_a_save_through_the_registry` fails.
- Suggested fix: raise the deadlines to 10 s or more.
- Tracked?: PR 778 (X-test-blockers-1112) covers test flakes; I did not confirm it touches these.

### PLAT-09 `seal.key` is created in place, so a crash during the first start can leave an empty file that blocks every later start
- Severity: polish
- Kind: correctness
- Confidence: likely
- Where: `crates/lodestar/src/services.rs:1093-1108`; `crates/lodestar/src/filter_load.rs:40-49`
- What: the key is written with `create_new`, `write_all`, `sync_all`, with no temporary file and rename. `read_seal_key` treats any length other than 32 as an error.
- Failure scenario: power is lost between the create and the write on the first start. Every later start fails with "not a 32-byte key" until someone deletes the file, although nothing was ever sealed with it.
- Suggested fix: write a temporary file and rename it, or treat an empty file as missing.
- Tracked?: not found.

### PLAT-10 `Handles::shutdown` skips the remaining stops when the pipeline or the indexer fails to stop
- Severity: polish
- Kind: correctness
- Confidence: confirmed
- Where: `crates/lodestar/src/services.rs:1019-1058`
- What: `let mut store = pipeline?;` returns before the hot save, `search.stop()`, the Insights recorder and the percolator. `search.stop()` failing likewise skips the last two. The comment on the hot save says "the indexer still gets to stop cleanly", which holds only when the pipeline stopped without error.
- Failure scenario: the pipeline's final save fails on a full disk. The index's last commit and the recorder's last write are skipped too, and the tasks are cut by the runtime shutdown.
- Suggested fix: keep the first error and carry on through the remaining stops.
- Tracked?: not found.

### PLAT-11 On Unix a stop signal during a long start is acted on only after the start finishes, and a second signal is ignored
- Severity: polish
- Kind: correctness
- Confidence: confirmed
- Where: `crates/lodestar/src/main.rs:287-311`; compare `crates/lodestar/src/windows/service.rs:1074-1077`
- What: the Windows service races `Services::start` with its stop channel. `serve_as` does not: the signal is latched and seen only by `Services::run`. Once the signal task has fired, later signals are swallowed, so a stuck shutdown step can only be killed.
- Failure scenario: `docker stop` during a long hot-column rebuild or migration. The start runs to completion, then shuts down; past the 60 s grace Docker kills it, as an unclean stop.
- Suggested fix: race the start with the signal as the service does, and make a second signal exit at once.
- Tracked?: not found.

### PLAT-12 Dependency pins in the manifests are mostly not exact
- Severity: polish
- Kind: maintainability
- Confidence: confirmed
- Where: `crates/*/Cargo.toml`, `bench/Cargo.toml` (about 165 caret requirements against 48 `=` pins); partial versions include `axum = "0.8"`, `rustls = "0.23"`, `mimalloc = "0.1"`, `serde = "1"`, `serde_json = "1"`, `toml = "0.9"`, `tracing-subscriber = "0.3"`, `rand = "0.10"`, `tower = "0.5"`, `tokio-postgres = "0.7"`; `rust-toolchain.toml:2` (`channel = "stable"`)
- What: `runner/standards.md` §2 says "pinned exactly". Releases, the Dockerfile and the MSI build pass `--locked`, so shipped binaries follow `Cargo.lock`; the manifests themselves allow drift on any `cargo update`. The toolchain is unpinned, which the standards already note has caused lint differences.
- Failure scenario: a routine `cargo update` moves a TLS or allocator crate across minor versions with no manifest change to review.
- Suggested fix: decide whether the lock file is the pin (and say so in the standard) or use `=` throughout; pin the toolchain version for releases.
- Tracked?: not found.

### PLAT-13 `begin_purge` runs a blocking store read on an async worker
- Severity: polish
- Kind: resource
- Confidence: confirmed
- Where: `crates/lodestar/src/ops.rs:727-734`
- What: `self.control.sources()` takes the shared store lock and queries SQLite directly in the async function. `sources()` and `purge_preview()` in the same file wrap the same call in `spawn_blocking`.
- Failure scenario: a purge begins while the shared store lock is held by a long write; a runtime worker blocks until it is free.
- Suggested fix: use `spawn_blocking` as the neighbours do.
- Tracked?: not found.

### PLAT-14 The service's refusal log in `%SystemRoot%\Temp` can be pre-created by an unprivileged user
- Severity: polish
- Kind: security
- Confidence: suspected (depends on the default ACL of `C:\Windows\Temp`, which lets Users create files; not tested)
- Where: `crates/lodestar/src/windows/temp_log.rs:49-70` (`append_to`)
- What: the file is opened with create and append, and refused only if it is a reparse point or not a regular file. Its owner is not checked and its size is not bounded.
- Failure scenario: a local user creates `lodestar-service.log` first. They own it, so they can read the refusal text (paths, SIDs, account names) and put their own lines in the file an administrator is told to read for "the exact fix".
- Suggested fix: refuse a file not owned by SYSTEM, Administrators or the service, or write under the private data folder when it exists.
- Tracked?: not found.

## Checked and found sound
- `privdrop.rs`: order is setgroups, setresgid, setresuid; then verification (real and effective ids, no supplementary groups, `setuid(0)` and `setgid(0)` must fail, `CapPrm`/`CapEff`/`CapAmb` zero); then `no_new_privs`. The per-thread syscalls are guarded by a one-thread check, and `CapEff` is checked before anything is chowned.
- `privdrop.rs` chown walk: every entry is opened `O_PATH|O_NOFOLLOW` and changed through that descriptor; hard-linked and special files are skipped; `/`, a symlinked data directory and a non-empty folder without Lodestar's markers are refused; the directory itself is changed last so a cut walk is retried.
- `main.rs`: the drop runs before the config is read, a socket is bound or a thread is spawned; `PUID=0` needs `LODESTAR_ALLOW_ROOT=1`; non-Linux Unix refuses root.
- Startup order (`services.rs`): auth is loaded and the pipeline (with its filter) is started before the listener serves; with nobody set up, no source starts (`setup_done`).
- Shutdown (`services.rs`): the drain is bounded (10 s), streams, jobs and logs are closed on the signal, the runtime exit is bounded (5 s), and a signal wins over a restart (`main.rs:339`).
- `system/restart.rs`: concurrent requests run the prepare step once under a lock; a paused service refuses whoever asks; the Unix re-exec handles a replaced binary (`(deleted)`).
- `system/address.rs`: no restart or crash loop. A pending bind that fails reverts at start, a revert that cannot be saved falls back to the previous address for the run, deadlines are clamped against clock jumps and enforced on both clocks.
- `windows/service.rs`: Stop, Shutdown and Preshutdown are acknowledged at once and ticked from their own thread; a stop is re-read after the runtime ends so the SCM never restarts a stopped service; the restart ledger is bounded, strictly parsed, link-safe and fails closed. `main.wxs` sets `FailureActionsWhen="failedToStopOrReturnedError"`, a virtual account and a 60 s pre-shutdown.
- `windows/acl.rs` SDDL parsing: strict (protected required, allow entries only for trusted SIDs, object and conditional entries refused, NULL DACL refused); paths and SIDs reach PowerShell through environment variables, never as script; tools are run by absolute path from `System32`.
- `windows/mod.rs` relaunch: the token goes only to the new copy; the copy waits for the old process object to exit before opening the data folder, with bounded waits and bounded watch output.
- Two instances on one data folder: the Unix supervisor takes `<data>/bin/.lock`, the index root has its own lock, and the offline commands take SQLite's exclusive lock (`cli/lock.rs`).
- `cli/backup.rs`: `VACUUM INTO` on a read-only connection gives a consistent copy while the server writes; the copy is `quick_check`ed; an existing backup folder is never written into. `cli/vacuum.rs`: staged copy, checked, renamed under the lock; a crash leaves one whole store.
- CLI secrets: database and worker secrets come from files or environment variables, never arguments; `command_failed` scrubs URL credentials.
- `/metrics` labels are bounded: plugin ids, fixed enums and tracker ids (never URLs). `/metrics` and `/healthz` are unauthenticated on purpose (`http.rs:65-67`) and carry no torrent names.
- `storage.rs`, `memstats.rs`, `fsdetect.rs`, `thp.rs`: sampling is off the async workers and never holds a store or index lock; `/proc` parsing is bounded and fails to "absent"; overmounts resolve to the newest mount.
- `unsafe`: forbidden workspace-wide; the only two blocks are in `crates/store/src/memstats.rs`, argument-free SQLite counter reads, each with a SAFETY comment and its own `allow` (owner decision §2.35). Every other crate inherits the workspace lints.
- Lock file: one version each of `rustls`, `ring`, `rustls-webpki`, `hyper`, `tokio`, `rusqlite`, `tantivy`. Duplicates are transitive only (`getrandom` ×3, `rand` ×2, `thiserror` ×2, `windows-sys` ×3).

## Area-level concerns
- **`Services::start` is one 660-line function** with about twenty fallible steps and no unwinding for what has already started. PLAT-01 follows from that shape; so will the next service added after the pipeline.
- **`[profile.release]` sets neither `panic` nor `overflow-checks`.** Release arithmetic wraps silently. `windows/ports.rs:272,293` add two parsed `u32`s unchecked; the input is `netsh`, so it is not reachable from the network, but the pattern is worth a workspace decision.
- **Poisoned locks are handled two ways.** `system.rs`, `system/address.rs`, `system/info.rs`, `ops.rs` and `http.rs` use `.lock().expect(...)` in 12 places; the rest of the area recovers with `into_inner`. One panic under the shared control-store lock would turn `/system`, `/metrics` and the address timer into repeated panics.
- **Default bind is `0.0.0.0:8090` on every platform,** including the Windows portable build and the bare Linux tarball (architecture §11, recorded). An unclaimed instance is protected only by the setup token, and `/metrics` and `/healthz` are open to anyone who can reach the port.
- **The backup's own `README.txt` restore steps say "copy these files"** without the ownership note that `docs/user/03-keep-lodestar-safe.md` §3.7 gives. The PUID chown walks only when the directory's own owner differs, so root-owned restored files in a correctly owned folder are not repaired and the start fails with a permission error.
- **Two decisions disagree on `secrets.key` in backups:** `2026-10-31-B-settings-sections.md` says backups should keep it with the database; `2026-11-06-X-legal-docs-r2.md` and the code leave it out. The later one matches the code.
- **Windows behaviour here is covered by pure-function tests plus `smoke.ps1`.** The service control path, the ACL inspection through PowerShell and the relaunch hand-over have no unit coverage of the system side, so findings PLAT-02 to PLAT-04 and PLAT-14 could not be confirmed by a test in the repository.
