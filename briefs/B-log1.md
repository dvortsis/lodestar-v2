# B-log1: Logging core
Covers the event catalogue (`log_event!`) with per-event switches and counters, typed data-kind wrappers per output, a call-site sweep, presets with timers, `GET /logs/catalog`, the rotating JSONL file, console options, the `logging` settings section, and the canary test.

**Model:** Opus. This is privacy- and security-critical work:
- it decides what reaches a log, and secrets must never reach one;
- one global subscriber and its atomics are shared by every thread;
- file rotation runs under concurrent writers.

**Branch:** `slice/B-log1`. **`/security-review` is required** before merge (secrets and identifying data in logs, standards §5.7). **Follow `.claude/skills/steward/SKILL.md` from your first push until the PR merges.**

**Spec:**
- `runner/standards.md`.
- **`docs/owner-decisions.md` §2.11, users control what is logged.** Every event and every kind of data can be switched, with no torrent specifics by default.
  - **Peer activity** and **Filter change details** are opt-in, off in every preset (Full included), limited to 1 or 24 hours, shown with a warning, and written only to the local file and the live log.
  - Secrets and sealed item names are never logged.
  - §2.11 narrows §1.9.7 and §1.8.8–9 for the log only, while an opt-in is on.
- `docs/owner-decisions.md` §2.14 (the user-facing steps go to the docs follow-up) and §2.19.
- Architecture:
  - **§12.1, all of it. It's the contract:** events, data kinds, presets, references, never-logged items, opt-ins, enforcement, the canary's four runs, the CI check's rules, outputs, rotation and `GET /logs/catalog`;
  - §12 (the setup token);
  - §10.10.7 (the Privacy grade);
  - §10.10.12 (what Settings → Logging needs from the server: events with rates and kind badges, the presets, per-output kinds, opt-ins with timers, the preview with example lines and volume, and outputs with size previews).
- `docs/architecture.md` §16:
  - B2's entry (`general.log_level` live through `log::follow`);
  - the settings session's **"Logging plan"** entry (B-log1 is "wrappers and sweep, file sink, `logging` section, canary"; B-log2 is the ring buffer, stream, history and file API; `general.log_level` moves into `logging`).
- `runner/slices.yaml` B-log1 notes:
  - it sweeps call sites to the wrappers, crate by crate;
  - **`LODESTAR_LOG` and the `config.toml` key `log` stay as aliases**;
  - the canary adds a Full-plus-opt-ins secrets run;
  - the CI check refuses argument-capturing `#[instrument]` and untyped error fields.
- Open PRs whose decisions you build on:
  - **B12** (#112, about to merge), `docs/decisions/2026-10-23-B12.md` on `origin/slice/B12`. "For the logging slice": **`apikey` and `token` query values are redacted** from any line carrying a URI, and headers are never logged. "Never logged or echoed" lists secrets, hashes, `Authorization` and `X-Api-Key`, `apikey` and `token`, and the proxy user header.
  - **M6-win** (#98), `docs/decisions/2026-10-22-M6-win.md` "Logging under the service": `windows/logfile.rs` is a stand-in until this slice. It asks for an `init` that takes a writer.
- **The code:**
  - `crates/lodestar/src/settings/log.rs`: `FILTER`, `parse`, `init`, `reload`, `follow_with` and `follow`;
  - `crates/lodestar/src/settings/general.rs`: `General.log_level`, its field `.file("log").env("LODESTAR_LOG")`, `MAX_FILTER` and the `verbose_logs` preview;
  - `crates/lodestar/src/settings/{mod.rs,README.md}`: `Registry::load`, `Field`, the hint vocabulary and `every_field_has_its_form_hints`, `KEY_PREFIX`, and "Adding a section";
  - `crates/lodestar/src/config.rs` (`Config.log`; `ENV_VARS` has `LODESTAR_LOG`; `deny_unknown_fields`), read but not edited;
  - `crates/lodestar/src/main.rs` (`init_logging` → `settings::log::init`), and `services.rs:229` (`settings::log::follow(settings.subscribe())`, which infers the section type from `follow`'s parameter);
  - `crates/lodestar/tests/serve.rs` (reads `setup token ` from stderr);
  - `crates/api/src/auth/http.rs` (`protected`).

## Own
- **`crates/log/**` (new), the `lodestar-log` crate.** It holds what every crate's call sites need:
  - `log_event!` and the catalogue types;
  - the per-event atomic switch and counter;
  - the data kinds and the typed wrappers (`Torrent`, `Name`, `Path`, `Peer`, `Url`, `Query`, `Client`, `Error`).

  It depends on `tracing` only. A macro other crates call can't live in the binary.
  - **Wiring:** one line in the root `Cargo.toml` `[workspace.dependencies]` (`members = ["crates/*"]` already includes it). Record the new crate in your decisions file (§13 layout).
- **`crates/lodestar/src/logging/**`, the binary's runtime:**
  - gathering every crate's catalogue (§12.1: "no registry dependency", so it's an explicit list);
  - the layers (console, JSONL file), per-output kinds, the references key, presets and timers;
  - the Libraries cap and URL scrub, the panic hook and the required events.
- `crates/lodestar/src/settings/logging.rs` (the section) and `crates/lodestar/src/settings/log.rs` (`init`/`follow`).
  - Also: removing `log_level` from `settings/general.rs`, one registration line, the widget vocabulary and the saved-key move in `settings/mod.rs`, and the affected goldens under `settings/golden/`.
  - New golden cases go in a new `settings/logging_tests.rs`, not `api_tests.rs`: #112, #111 and #48 change that file.
- **`GET /logs/catalog`:**
  - `crates/api/src/logs.rs` and `crates/api/src/dto/logs.rs` (new; B-log2 extends `logs.rs` next);
  - `crates/api/src/log_events.rs` (the api crate's catalogue);
  - registration lines in `crates/api/src/{lib.rs,openapi.rs,dto/mod.rs}`, `tests/openapi.rs` `ROUTES`, the 401 list in `tests/api.rs`, and the regenerated `openapi.json` + `schema.d.ts`.
- **Sweep:** only the files in Build step 8 that no open PR touches, plus `crates/integrations/src/log_events.rs` with its `mod` line in `crates/integrations/src/lib.rs`.
- **Dependency lines:** `lodestar-log.workspace = true` in `crates/{lodestar,api,integrations}/Cargo.toml`; `flate2` in `crates/lodestar/Cargo.toml` (step 6); `Cargo.lock`.
- **One line in `crates/lodestar/src/main.rs`:** `mod logging;`, placed between `mod http;` and `mod metrics;`, away from #114's `mod fixture;` and #98's `mod windows;`. Nothing else in `main.rs`: `init_logging` already calls `settings::log::init`, whose body you own.
- `.github/scripts/log_lint.py` + a test, and one step in `ci.yml`'s `lint` job.
- `crates/lodestar/tests/log_canary.rs` (new), and `docs/decisions/<date>-B-log1.md`.
- **Don't edit** (open PRs touch these, or other slices own them):
  - `crates/lodestar/src/services.rs` (#114, #112, #111, #48, B11a);
  - `config.rs` and `metrics.rs` (#48), `windows/**` (#98), `fixture.rs` (#114);
  - `crates/lodestar/src/system.rs` and `crates/api/src/system.rs` (B2c, running beside you);
  - `crates/api/src/auth/**` (#112);
  - every deferred file in step 8;
  - `docs/user/**` and `docs/operations.md`. Put the user-facing steps in the PR.
- **Nearby open PRs:** #114 and #98 (`main.rs`, `crates/lodestar/Cargo.toml`, `Cargo.lock`), #112, #111, #115, #102 and #48 (`api/{lib.rs,openapi.rs,dto/mod.rs}`, `tests/openapi.rs`, `tests/api.rs`). Keep your lines additive and merge `main` as each lands. B2c adds `mod system;` and its own registration lines in the same files.

## Build
1. **The catalogue and `log_event!`** (in `lodestar-log`).
   - **Each crate declares** a static catalogue. Each event has:
     - a stable id (`fetch.failed`);
     - a component (DHT, Fetch, Pipeline, Search, API, Sources, Filters, Integrations, Workers, Companions, Auth, Libraries) and a level;
     - label and help message keys;
     - default on or off, and required;
     - the data kinds it can carry;
     - its declared fields.
   - **`log_event!(EVENT, field = wrapper, …)`** checks one atomic (on or off), bumps the event's counter (counters count even when the event is off), and only then formats.
     - **It takes no format string.** The message is the catalogue's, and every dynamic part is a declared, typed field, so an untyped error or name can't slip into a message.
     - Undeclared fields are refused: at compile time where that's cheap, otherwise by the CI check.
   - **The binary gathers the catalogues** and registers them once (for example a `OnceLock` in `lodestar-log`), so `GET /logs/catalog` reads them without a new `ApiState` field.
   - **Required events, which can't be turned off:**
     - start, stop and crash (a panic hook installed by `settings::log::init`);
     - the setup token;
     - settings saved (field names only, never values);
     - restart;
     - disk full and rotation failure;
     - data kinds or opt-ins switched on, off or expired.

     Declare them all now, including those whose call sites are deferred (`services.rs`, B2c's restart). None of them carries torrent data.
2. **Data kinds and wrappers** (§12.1's table).
   - **The kinds:**
     - info hashes, torrent names, file names and paths, tracker and source URLs, search queries and peer addresses: off by default;
     - client addresses (sign-in and API) and sizes: on by default;
     - peer activity: opt-in only.
   - **Each wrapper** marks its field with its kind in the record, and the output's layer renders it by that output's switch. A kind that's off is never written: a torrent shows as `ref:xxxxxxxx` and anything else as `[hidden]`.
   - **References:**
     - a keyed hash of the info hash under a random key made at each start (OS RNG; `getrandom` 0.3.4, `sha2`/`hmac` are already locked);
     - never written down, and not reversible.
   - **`Url` (and a request-URI form) always removes** query values for `apikey`, `token` (B12), `passkey` and other credential-like names, plus tracker passkeys in the path, **even with URLs on**. Headers are never logged.
   - **`Error`** renders the error's kind, and its message only for error types marked free of names and paths.
   - **Never loggable at any setting:** passwords, API keys, feed tokens, cookies, secrets, sealed values, and the names or paths of quarantined, dropped or hash-listed items.
3. **Presets with timers.**
   - **Verbosity:** Errors only, Warnings, **Normal** (the default), Detailed, Everything. Changing one event makes it Custom, and "Reset to preset" undoes that.
   - **Data:** **Private** (the default), Hashes, Full, Custom.
   - **Identifying kinds** (hashes, names, paths, URLs, queries, peer addresses) are time-limited: 1 hour (the default), 24 hours, or until turned off.
   - **Opt-ins** (Peer activity, Filter change details) last 1 or 24 hours only, never "until off", and no preset touches them.
   - **"Detailed for 30 minutes"** is a timed verbosity.
   - **Store each timer as a deadline,** so a kind is on only while the deadline is ahead: expiry needs no save, survives a restart, and logs the required switch event when it passes. Record the representation, including the sentinel for "until off". `check` refuses an opt-in longer than 24 h.
4. **The `logging` settings section** (`settings/logging.rs`).
   - **Fields cover:**
     - verbosity and per-event overrides (only deviations from the preset are stored);
     - per-component levels (Advanced);
     - the raw filter for Libraries (Expert);
     - the data preset, per-kind switches with deadlines, and the opt-in deadlines;
     - console: format (text or JSON), detail (Private, or the same as the file) and level;
     - file: on or off, folder (restart), file size (16 MiB), files kept (7), maximum age (14 days) and total cap (256 MiB);
     - Windows Event Log errors (stored now; M6 wires it).
   - **Hints:** every field gets hints from the registry's vocabulary. Add new widgets (for example `log-events`, `log-kinds`, `duration`) to that vocabulary and its test.
   - **Preview:** `Section::preview` gives warnings (identifying kinds or opt-ins on) and estimates (volume per day per output, from the counters). Give the screen its example lines, rendered from sample data and never real torrents, either here or in the catalogue. Pick one and record it.
   - **Aliases:**
     - The verbosity, component and Libraries fields carry `.file("log").env("LODESTAR_LOG")`.
     - A bare level (`error`…`trace`) maps to the verbosity preset. A filter string maps its default level to verbosity, `lodestar_*` directives to component levels, and everything else to the Libraries filter.
     - Set by the environment, those fields are read-only (`409 set_by_environment` naming `LODESTAR_LOG`).
     - **Move a saved `settings.general.log_level`** to the new keys once, in one transaction, at `Registry::load`, and test it. Saved keys are named by section, so this is a rename.
   - **What waits for #48** (`config.rs` is in it): a `[logging]` table in `config.toml` and any new `LODESTAR_LOG_*` variables. Until then the other fields are set in the app only, like `transport`. List them for the follow-up.
   - **No `services.rs` change:** `follow` takes `watch::Receiver<Logging>`, so `services.rs:229` stays as written.
5. **Outputs.**
   - **Console (stderr):** always on, text by default, JSON as an option. It stays on Private kinds unless "same as the file" is chosen, and opt-ins never reach it.
     - Keep message text the same in text format: `tests/serve.rs` finds `setup token ` there.
   - **JSONL file:** on by default at `<data>/logs/lodestar.jsonl`, with fields marked by kind so B-log2's `redact=true` can strip them.
     - Write JSON with `serde_json`; don't enable `tracing-subscriber`'s `json` feature, which needs an unlocked crate.
     - The folder comes from the section, since `Logging::from_config` sees `config.data_dir`. The file sink opens when `follow` first runs. Lines from before that are buffered (bounded) and written once it opens, so the file has the start-up.
   - **Libraries** (third-party targets) are capped at warnings by default, with query strings and passkeys removed from URLs in their messages.
   - **Deferred first-party plain `tracing` lines** flow as before, under their component's level, until the follow-up sweep converts them.
   - **For M6-win:** add an `init` variant that takes the service's writer, as #98 asks. Wiring it in `windows/**` is M6's follow-up.
6. **Rotation.**
   - **Policy:** rotate by size and gzip rotated files (`flate2` 1.1.10, already locked as a build dependency, is approved as a normal one). Keep the count, age and total-cap limits.
   - **Disk full** stops file logging with a console warning and the required event; Lodestar keeps running.
   - **Track which files were written while an identifying kind or opt-in was on,** for example in a small index beside the files, so B-log2 can badge them and delete "only those with details".
   - **Concurrency:** writers never block on rotation for long, and a failed rotation keeps appending rather than dropping lines (M6-win's rule).
7. **`GET /logs/catalog`** (admin session only, `auth::protected`; keys and feed tokens can't read logs).
   - **Every event:** id, component, level, label and help keys, default, current, data kinds, required and rate per hour.
   - **Every data kind:** its switch per output, whether it's identifying or an opt-in, and its deadline.
   - **Also:** the current presets and the outputs.
   - The screen renders from it, so a new event needs no UI change.
8. **The sweep.** Convert only files no open PR touches. A crate is swept only when its catalogue's home (`lib.rs`) and `Cargo.toml` are also untouched; the binary and `api` are the exceptions, through your registration lines. Line numbers are as on `main` today.
   - **Sweep now:**
     - **`crates/integrations`** (fully):
       - `clients/aria2.rs`:191, `deluge.rs`:238,519, `qbittorrent.rs`:194,282,388, `rtorrent.rs`:168,312, `transmission.rs`:246,331,361,395;
       - `notify/discord.rs`:230, `email.rs`:608, `mod.rs`:316,329, `ntfy.rs`:249, `webhook.rs`:146;
       - `secret.rs`:516,534.

       Client and notifier URLs go through `Url`.
     - **`crates/lodestar`:** `floor.rs`:64, `fsdetect.rs`:109, `privdrop.rs`:55,58,63, `subscriptions.rs`:94,292,304,331,335,365,389 (list URLs through `Url`), `settings/mod.rs`:426,430,436,448,457,978,986 (`settings.saved` with field names), `settings/crawler.rs`:225, and `settings/log.rs`.
     - **`crates/api`:** `error.rs`:147 (`Error`), and `filters.rs`:1143,2141.
       - Add `filters.rule_set_updated` and the opt-in `filters.change_detail` at the apply path if that's a contained change. Otherwise declare them and list the call site for the follow-up.
       - Never name an item a change released.
   - **Deferred to a follow-up sweep slice** (propose it as `B-log1b` in the PR for the runner to add to `slices.yaml`; don't edit `slices.yaml`):
     - **`crates/api`:** `auth/http.rs`:182, plus #112's new `auth/**` and `keys.rs` lines, the `api.request` event (`apikey`/`token` redacted) and the sign-in events. Blocked by #112.
     - **`crates/filter`:** `lists.rs`:580. `lib.rs` is in #110.
     - **`crates/pipeline`** (`lib.rs`, `writer.rs` and `Cargo.toml` are in #48; B11a edits `rollups.rs`):
       - `control.rs`:67,496,501,519,698, `dht.rs`:133;
       - `lib.rs`:153,378,391,457,497,689,692;
       - `worker.rs`:72,140,199. **`key = %key` is a fetch key, so identifying: it becomes `Torrent`;**
       - `writer.rs`:184,233,236,249,323,431,455,470.
     - **`crates/search`** (#102, #48, B11a):
       - `index.rs`:709,949, `live.rs`:216,382;
       - `rebuild.rs`:156,599,612,713,773,781,791,836,865,926,960,984,1006,1013,1032.
     - **`crates/lodestar`:**
       - `main.rs`:116,145,180 (#114, #98);
       - `services.rs`:164 (the setup token, a required event), 274, 327 and 338 (#114, #112, #111, #48, B11a).
     - **Code those PRs add:** `windows/**` (#98), `fixture.rs` (#114), `health/**` and `tracker-scrape` (#48), `insights` (B11a) and `system.rs` (B2c).
     - **Also deferred:** the `[logging]` config table and new env variables (`config.rs`), `lodestar_log_records_total{level}` (`metrics.rs`; the counters exist), and the Windows service's sinks and Event Log (`windows/**`).
9. **The CI check** (`.github/scripts/log_lint.py`, run in `ci.yml`'s `lint` job).
   - **It refuses:**
     - plain `tracing` macros (`trace!`…`error!`, `event!`, spans used as log lines) outside tests;
     - `#[instrument]` without `skip_all`, or with a `skip(..)` that names every argument (`fields(x)` alone still records every argument);
     - any `%`/`?` error field (`%e`, `?err`, `error = %e`);
     - `log_event!` with undeclared fields, if the macro can't check that itself.
   - **It enforces an explicit list of paths:** the fully swept crates, and the swept files of partly swept ones. PRs that merge after you, and files your sweep defers, don't go red. The follow-up widens the list to every first-party crate, and the check's own test fails if a listed path is missing.
10. **The canary** (`crates/lodestar/tests/log_canary.rs`, against the real binary, as `tests/serve.rs` does). It turns on every event through `PATCH /settings/logging`, captures stderr, and reads the JSONL file. The ring buffer, live stream and download join it in B-log2; note that for B-log2.
    - **Fixture data:** use `serve --fixture` if #114 has merged. Otherwise seed a temporary folder through `lodestar_store`.
    - **Then exercise:** searches, detail, filters and subscriptions with a tracker URL carrying a passkey.
    - **The four runs (§12.1):**
      - (1) Private: no fixture hash (hex or base32), name, path, tracker passkey or peer address in any output;
      - (2) one kind at a time: only that kind appears;
      - (3) opt-ins: peer-with-torrent and filter-change detail appear only while on, only in the file, and are gone once the deadline passes. Test-declared events may stand in where no real event carries them yet;
      - (4) **Full plus both opt-ins:** no secret or sealed name anywhere.
    - **Run (4) covers:**
      - the admin password (setup, login, password change);
      - the `lodestar_session` cookie value;
      - the setup token after setup;
      - an API key sent as `Authorization: Bearer`, `X-Api-Key` and `?apikey=`;
      - a feed token as `?token=`;
      - the trusted-proxy user header (`Remote-User` from a trusted peer);
      - a tracker passkey;
      - a quarantined fixture's name.
    - **Keys, tokens and the proxy user need B12** (#112, about to merge): bring it in before opening the PR. If it hasn't merged when everything else is ready, message the runner.
    - **A leak found in a deferred file:** convert it if the file is free. If not, exempt it narrowly (that target and value, named in the PR) and never weaken the canary as a whole.

## Tests
- **Catalogue:** ids are unique; every required event can't be turned off; an event that's off costs only the switch check and the counter, and its counter still moves; undeclared fields are refused.
- **Wrappers and per output:**
  - every kind renders per output: hidden, `ref:` or the value;
  - references are stable within a run and change across runs;
  - `Url` strips `apikey`, `token` and passkeys with URLs on;
  - `Error` hides messages of types not marked plain;
  - the console stays Private under a Full file; opt-ins never reach the console.
- **Presets and timers:** presets set the switches and Custom is detected; opt-ins are refused beyond 24 h and untouched by presets; deadlines expire without a save, across a restart, and log the required event.
- **Section:**
  - the registry's walks (schema, hints, the preview equals PATCH property);
  - the aliases: `LODESTAR_LOG=debug`, a filter string, `log = "…"` in `config.toml`, and read-only under the environment;
  - the `general.log_level` move.
- **File:**
  - rotation at size, gzip, count, age and total cap;
  - disk full keeps Lodestar running;
  - concurrent writers lose no line and interleave none;
  - the details index is right;
  - start-up lines are buffered into the file.
- **`/logs/catalog`:** a golden; 401 without a session; refused with an API key when B12 has merged.
- **The CI check's own tests** cover each rule, including the explicit path list.
- **The canary's four runs.**

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass after merging `origin/main`.
- `cargo run -p lodestar --bin openapi` + `cd web && npm run api:types` leave no diff, and `python3 .github/scripts/log_lint.py` passes.
- `/security-review` has been run on the branch; its findings and fixes are in the PR.
- **The PR** is titled `B-log1: Logging core: event catalogue (log_event!) with per-event switches and counters, typed data-kind wrappers per output and call-site sweep, presets with timers, GET /logs/catalog, rotating JSONL file, console options, logging settings section, canary test`. Its description lists:
  - the event ids per component, with the required ones;
  - the kinds and wrappers, the reference scheme and the always-redacted query names;
  - the presets and the timer representation;
  - the `logging` fields with their defaults, the alias mapping and the saved-key move;
  - the outputs, the file format and rotation;
  - the catalogue's shape;
  - the CI rules and the enforced path list;
  - what each canary run covers, and what waits for B-log2;
  - the swept and deferred call sites per crate, with the PR blocking each, as the proposed `B-log1b`;
  - every shared-file line;
  - the user-facing steps for the docs follow-up (S-logs and the user guide), including that `LODESTAR_LOG` still works;
  - the §2.19 refinements, such as the new crate, deadline-based timers, the explicit lint path list and the narrow canary exemptions.
