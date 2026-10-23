# B-log1b: Logging sweep
This slice converts every remaining raw `tracing::` call site to catalogued `log_event!` events, one crate per commit, until `NOT_YET_SWEPT` is empty. It also:
- emits `auth.setup_token` through the catalogue;
- adds `api.request` and `auth.*` events, with canary positive controls;
- adds a hot-path bench, and cache-line padding, before the DHT and fetch sweep;
- adds a test that `TARGETS` covers every crate.

It also carries the B-log1 and B-log2 follow-ups: URL scrubbing gaps, a linear `scrub_text`, Windows log-folder hardening, a shared `%SystemRoot%\Temp` log helper, lint gaps, and a bounded graceful shutdown.

**Model:** Opus. This is privacy- and security-critical work that touches concurrency:
- every line it converts decides what reaches a log. Secrets, the setup token after setup, and sealed names must never reach one, and torrent details only through their kinds;
- the new events sit on the DHT and fetch hot paths, where one shared counter can cost every crawler thread;
- the Windows changes decide who can read the log folder, which holds the first-run setup token, and what a SYSTEM service appends to in a world-writable folder.

**Branch:** `slice/B-log1b`. **`/security-review` is required** before every PR merges (secrets and identifying data in logs, request logging, Windows ACL and reparse-point handling; standards §5.7). **Follow `.claude/skills/steward/SKILL.md` from your first push until the last PR merges.**

**Spec:**
- `runner/standards.md`, and CLAUDE.md "Before pushing".
- **`docs/owner-decisions.md`:**
  - §2.11: users control what's logged. There are no torrent specifics by default. Secrets and sealed names are never logged. Peer activity and Filter change details are opt-in only, and go only to the file and the live log;
  - §1.9.7 (no profiles of people), §1.8.8–9 (no audit log), §1.6 (performance is a top priority), §2.14 (user-facing steps go to the docs follow-up), §2.19 (the runner decides refinements; record each one).
- **Architecture §12.1, all of it.** In particular:
  - the catalogue ("Every line Lodestar writes is a catalogued event": `dht.key_harvested`, `fetch.failed`, `api.request`, `auth.sign_in_failed`);
  - the required events;
  - the data kinds and peer activity;
  - "Never logged, at any setting";
  - "Enforced where the line is written" (an event that's off costs a load and an increment);
  - the canary's four runs, and "Floods".

  Also §12 (the setup token).
- **`docs/decisions/2026-10-23-B-log1.md`, all of it:**
  - "Typed events bypass `tracing`", "Undeclared fields don't compile", "URLs", "`Error`", "Verbosity presets and `default_on`";
  - "First-party targets are an explicit list, `logging::TARGETS`";
  - "The CI check enforces…" and round 1's "The CI check scans every crate";
  - "Narrow canary limits" and "Private holds for typed events now, and for every line after B-log1b";
  - **both "For B-log1b" lists** (rounds 1 and 3), and the Windows log-folder bullets of rounds 2 and 3.
- **`docs/decisions/2026-10-23-B-log2.md` and `2026-10-23-B-log2-files.md`:**
  - flood folding (per-output identity; it never helps distinct lines);
  - "The setup token never enters it" and the retirement note ("When B-log1b logs the setup token as `auth.setup_token`, retire the plain-line special case");
  - "Carried forward (Windows hardening)";
  - "After the self-review" (a client that never reads holds the graceful shutdown).
- **`docs/decisions/2026-10-23-B12.md`:** "For the logging slice" (`apikey` and `token` query values redacted from any line carrying a URI; headers never logged) and "Never logged or echoed".
- **`docs/decisions/2026-10-22-M6-win.md`** ("Logging under the service") and the header of `crates/lodestar/src/windows/acl.rs` (the data folder's privacy rules).
- **`runner/slices.yaml` B-log1b:**
  - one crate per commit;
  - high-volume events stay `default_on: false` until the bench decides (Build 3);
  - never log a secret: the canary covers each new event.
- **The code:**
  - **`crates/log/src/`:**
    - `event.rs`: `Event` (`on`, `count`, `declare`, `is_on`, `hit`) and `Kind`;
    - `lib.rs`: `catalogue!` and `log_event!`;
    - `dispatch.rs`: `emit`, `fallback` and `register`;
    - `value.rs`: `Url`, `Url::request`, `Client`, `Peer`, `Listen`, `SetupToken`, `Count` and `Error`;
    - `url.rs`: `scrub`, `scrub_with`, `userinfo`, `hard_stop`, `url_char`, `scrub_text`, `redact_secrets`, `find_ci` and `hide_runs`.
  - **`crates/lodestar/src/logging/`:**
    - `mod.rs`: `CATALOGUES`, `TARGETS`, `first_party`, `component_of`, `Runtime` and the `Sink` impl, including the `APP_STOP` fold flush;
    - `events.rs`: the required events, including `auth.setup_token { token: SetupToken, address: Label }` and `app.restart`, both declared with nothing emitting them yet, and `app.stop`, which nothing emits either;
    - `entry.rs` and `view.rs`: `redact_setup_token`;
    - `file.rs`: `Index` (`restricted`, `load`'s fail-closed rebuild), `folder()`, `service_log()` and `private()`;
    - `files.rs`: `folder()`, whose ownership check is Unix-only.
  - **`crates/lodestar/src/settings/log.rs`:** `init`, `init_with_writer` (made for the Windows service) and `follow`. Read it, but don't edit it.
  - **`crates/lodestar/src/windows/`:**
    - `service.rs`: its own subscriber (`LogFile`), `log_refusal` and four plain lines;
    - `logfile.rs`: the stand-in;
    - `acl.rs`: `parse_sddl`, `trusted`, `check_dacl` and the `Windows` ops;
    - `ports.rs` and `priority.rs`.
  - **`crates/lodestar/src/services.rs`:** `Services::start` (the setup token at :201) and `Services::run` (`with_graceful_shutdown(signal)`, then `events.close()` and `handles.shutdown()`).
  - **`crates/api/src/`:**
    - `lib.rs`: `router`;
    - `auth.rs`, `auth/{http,guard,proxy,limit}.rs` and `extract.rs` (`peer_ip`);
    - `log_events.rs`.
  - **`.github/scripts/log_lint.py`:** `NOT_YET_SWEPT`, `EXEMPT`, `manifests`, `LOG_DEP`, `listed_in`, `grown`, `base_list`. Also `test_log_lint.py`, and `ci.yml`'s `lint` job (lines 86–87).
  - **`crates/lodestar/tests/log_canary.rs`:** `Server`, `exercise`, `absent` and the four runs, including `run_4_keys_tokens_and_the_proxy_user_never_show_either`.

## Own
- **`crates/lodestar/src/logging/**`.** Includes the new `bench.rs` (Build 3), `CATALOGUES` lines and `TARGETS` entries.
- **`crates/log/**`.** slices.yaml doesn't list it, and nobody owns it now; the runner adds it to B-log1b's owns. Changes: the padding, the URL and `scrub_text` fixes, and any wrapper you need. List each change in the PR.
- **`crates/api/src/auth.rs` and `crates/api/src/auth/**`.** Includes a new `auth/log_events.rs` (an `AUTH` catalogue).
- **Request logging:**
  - `crates/api/src/request_log.rs` (new);
  - `crates/api/src/log_events.rs` (additive: `api.request`);
  - in `crates/api/src/lib.rs`, **only** `mod request_log;` and one `.layer(…)` line in `router`. #133 adds lines there too; keep yours additive.
- **`crates/pipeline/src/**`.** After #48 this includes its `log_events.rs` and `health/**`.
- **`crates/search/src/rebuild.rs`, plus `index.rs`, `live.rs` and `log_events.rs`.** `index.rs` and `live.rs` are beyond slices.yaml's list; the runner adds them. Call-site lines only, plus the catalogue.
- **`crates/dht/src/**` and `crates/peer-wire/src/fetch.rs`:**
  - new `crates/dht/src/log_events.rs` and `crates/peer-wire/src/log_events.rs`;
  - one `pub mod log_events;` line in `crates/peer-wire/src/lib.rs`;
  - `lodestar-log.workspace = true` in `crates/{dht,peer-wire}/Cargo.toml`.
- **`crates/filter/src/lists.rs`:**
  - a new `crates/filter/src/log_events.rs`;
  - one `mod` line in `crates/filter/src/lib.rs`;
  - `lodestar-log.workspace = true` in `crates/filter/Cargo.toml`.
- **`crates/lodestar/src/windows/**`** (slices.yaml) **and `crates/lodestar/src/system/**`.** The runner adds `system/**` to slices.yaml (carry-forward `slices-yaml-B-log1b-system`).
- **`crates/lodestar/src/{fixture.rs,insights.rs}`:** call-site lines only.
- **`crates/lodestar/src/services.rs`:** the call-site lines (:125, :130, :194, :201, :363, :433, :453, plus any plain lines #48 adds there) **and the bounded shutdown in `Services::run` (Build 9). Nothing else.**
- **`crates/lodestar/src/main.rs`:** the three call-site lines only (:129, :159, :216), and only once B-update (#128) has merged. B-update owns `main.rs`. If B-update's next PR is open by then, message the runner before you touch it. If you can't sweep it, `main.rs` stays in `NOT_YET_SWEPT`, with B-update named as the reason.
- **Tests:**
  - `crates/lodestar/tests/log_canary.rs`;
  - `crates/lodestar/tests/shutdown.rs` (new);
  - unit tests beside each change.
- **Lint and CI:**
  - `.github/scripts/log_lint.py` and `test_log_lint.py`;
  - in `ci.yml`, **one** new step or job for the bench gate (Build 3);
  - if the bench job needs path triggers, the matching line in `ci_changes.py`.
- **Generated files:** the goldens under `crates/lodestar/src/settings/golden/` that new events move (`logs_catalog.txt`, the logging previews), **regenerated with `LODESTAR_BLESS=1`, never edited by hand**. Also `Cargo.lock`.
- **Decisions:** `docs/decisions/<date>-B-log1b.md`, plus `-r2` and `-r3` for later PRs (`docs/decisions/README.md`).
- **Don't edit:**
  - `crates/lodestar/src/settings/**`, except the regenerated goldens. `follow` and `init_with_writer` already do what you need;
  - `crates/api/src/{logs.rs,events.rs}` and `dto/**`;
  - `crates/sources/**` (where `fetch` is called), `crates/tracker-scrape/**`, `config.rs` and `metrics.rs` (`lodestar_log_records_total{level}` stays a follow-up);
  - `runner/slices.yaml`;
  - `web/**`, `docs/user/**` and `docs/operations.md`. Put the user-facing steps and the new message keys (`logs.event.<id>.label`/`.help`, for S-logs) in the PR.
- **Nearby open PRs:**
  - **#48 M4** (`pipeline/**`, `services.rs`, `fixture.rs`, `api/lib.rs`, `logging/mod.rs` `CATALOGUES`, `logs_catalog.txt`). The runner merges it before you launch. Start from a `main` that has it, and take its new `pipeline/log_events.rs` and its plain lines into your sweep.
  - **#128 B-update** (`main.rs`, `services.rs`, `system/restart.rs`, `windows/service.rs`, the `update` line in `logging::CATALOGUES`, `logs_catalog.txt`).
  - **#133 B11c** (`api/lib.rs`, `openapi.rs`, `dto/mod.rs`, `tests/api.rs`).
  - **#131** (`settings/log.rs`, a test fix).

  Keep shared lines additive. Merge `origin/main` as each lands (never rebase). **Regenerate the goldens after each merge; never hand-merge them.**

## Build
Line numbers are as on `main` at `8b112b6`. **After merging `main` (with #48), list the plain lines again with `log_lint.py`'s `plain_macros`, and convert every plain line in a listed file**, including the ones #48 and #128 add. On `8b112b6` there are 112 lines in 21 files.

1. **Plan the split first.** The sweep alone is about 110 call sites plus their catalogue entries, so expect well over 1,500 changed lines. Plan **three PRs on `slice/B-log1b`**, each security-reviewed and merged before the next opens. Give each one's reason in its description. One crate per commit throughout.
   - **(1/3) Core, bench and hardening:** Builds 2–5 and the TARGETS test (Build 6). No call site changes yet, so the bench's numbers come before any hot event exists.
   - **(2/3) API, auth and the binary:**
     - Build 7 (`api.request`, `auth.*`, the setup token);
     - the `lodestar` crate's sweep (`services.rs` with Build 9, `system/**`, `windows/**` with the service wiring, `fixture.rs`, `insights.rs`, `main.rs`);
     - `filter/lists.rs`;
     - the canary's positive controls for them.
   - **(3/3) Pipeline, search, DHT and fetch:** Build 8, ruled by Build 3's numbers.
   - If a part stays under about 1,500 lines with the next one, you may join them; say so in the description.
2. **Padding and the hot path** (`crates/log`).
   - Today `Event` holds `on: AtomicBool` and `count: AtomicU64` side by side, and the catalogue statics sit next to each other. A `fetch_add` from one crawler thread then invalidates its neighbours' switch and counter on every other core.
   - **Put the counter on its own cache line:** a `#[repr(align(128))]` wrapper (128 covers adjacent-line prefetch on x86 and Apple's 128-byte lines). Keep `on` apart from it, so the switch stays read-mostly. It must stay `const`-constructible for `declare`.
   - **If the bench shows one hot event's single counter is the cost** (many threads, one event), stripe it: a small fixed array of padded counters, indexed by a per-thread slot and summed by `count()`. Do this only if the numbers call for it, and record which you chose.
   - **No new dependency** (no `crossbeam`).
3. **The hot-path bench** (`crates/lodestar/src/logging/bench.rs`). It needs the binary's real `Runtime`, so it lives in the binary crate, as `#[ignore]` tests run in release by one CI step:
   ```
   cargo test --release -p lodestar --bin lodestar logging::bench -- --ignored --nocapture
   ```
   With `LODESTAR_LOG_BENCH_GATE=1` set, the gates below are asserts. If you'd rather put it under `bench/`, record why.
   - **What it measures:**
     - (a) an event that's off: the switch and the counter, single thread;
     - (b) the same with 1, 4 and 8 threads hitting **adjacent** events, and then **one** event, padded and unpadded (keep the unpadded layout as a bench-only type);
     - (c) an event that's on, with its fields typed as the DHT and fetch events will carry them (a `Torrent`, a `Peer`, a `Label`), through the installed `Runtime` under Private: console to a sink, the file in a temp folder, the ring and the fold tables;
     - (d) (c) with **distinct** lines, a new hash each time, which folding can't reduce, at the peak rates below.
   - **Peak rates:** take them from the code and record them:
     - the crawler's Aggressive intensity (`settings/crawler.rs`: packets per second, concurrent fetches);
     - the DHT actor's harvest paths (`harvest_hash`: sample, `get_peers`, announce).
   - **Gates (runner's refinement, §2.19; record them):**
     - (a) ≤ 5 ns per event;
     - (b) padded, 8 threads on adjacent events cost ≤ 2× (a) per event;
     - (c) and (d) reported as ns per line and bytes per line.
   - **The rule that decides `default_on`, written in the decisions file, for every event Build 8 adds or converts.** An event may be `default_on: true` only if, at its peak rate with distinct lines, both hold:
     - its CPU cost (rate × (c)) is ≤ 1% of one core;
     - its file volume (rate × bytes per line × 86,400) is ≤ the default folder cap's daily share: 256 MiB / 14 days, about 18 MiB a day.

     Anything over either is `default_on: false` (it joins at Detailed). If it would still swamp Detailed by the same measure, make it `Trace` (it joins only at Everything). Give it a periodic `Info` summary event instead (for example `dht.harvest_summary`: counts per source every 60 s, no hashes), which is `default_on: true`.
     - The PR lists each high-volume event with its rate, its cost and the verdict.
     - The bench names its cases (`hot_off`, `hot_contended`, `hot_on`, `hot_distinct`) so later slices can re-run it.
4. **URL scrubbing** (`crates/log/src/url.rs`).
   - **Raw `"`, `<`, `>`, a backtick and a space in a URL password.**
     - Today `scrub_text` looks for the userinfo only up to a `hard_stop`, so `https://u:pa ss@host/x` keeps ` ss@host` and part of the password.
     - Make an `@` that follows a `scheme://` on the same line, before any `/`, take everything from the scheme to that `@` as userinfo, within a bounded look-ahead (record the bound, for example 512 bytes). Over-redaction, such as `see https://a.example and me@x`, is accepted: "never too little" (B-log1 round 3).
     - Test each of the five characters in `scrub_text` and in `Url::new`/`Url::request`.
   - **The `Debug` form of `url::Url`/`reqwest::Url`** (`Url { scheme: "https", username: "u", password: Some("p"), …, query: Some("passkey=k") }`) has no `://`, so `scrub_text` lets the password and the query through. A third-party line at `warn` can carry it.
     - Make `scrub_text` treat `password: Some("…")`, `username: "…"`, `query: Some("…")` and `fragment: Some("…")` as secrets.
     - Hide `path: "…"` values that hold a passkey-like run, as the URL rules do.
     - Test with the real `Debug` output of `url::Url::parse("https://u:p@h/ann/<passkey>?passkey=k#f")`. `url` is already locked through `tracker-scrape`; a dev-dependency line in `crates/log/Cargo.toml` is approved.
     - **No swept line may `?`-format anything that holds a URL.** `tracker_scrape::{TrackerUrl, HttpTracker}` derive `Debug` over one; name them in the PR as a follow-up for their owner.
   - **`scrub_text` is quadratic on long lines.** Each loop re-runs `find("://")`, `find_ci(…, "magnet:")` and the hard-stop scan over the whole rest, so a 1 MiB line of `a://` repeats is O(n²).
     - Make it one forward pass, or cache the next match of each needle and only advance it.
     - Check `redact_secrets` and `hide_runs` for the same problem.
     - **Test:** the time for 1 MiB of each adversarial pattern (`a://`, `magnet:`, `token=`, `@`, `://u:p@`) is within 30× the time for 64 KiB (linear is about 16×, quadratic about 256×). Never assert on wall-clock time alone.
   - **History and redacted downloads** re-scrub older lines with these rules, so they get the fixes for free. Check that B-log2's tests still pass.
5. **Windows log-folder hardening** (`logging/**`, using `windows/acl.rs`).
   - **A shared `%SystemRoot%\Temp` log helper** (carry-forward `win-temp-log-helper`), for example `windows/temp_log.rs`, `#[cfg(windows)]`:
     - it opens `%SystemRoot%\Temp\lodestar-service.log` for append through `std::os::windows::fs::OpenOptionsExt::custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)`, with no `unsafe` (the workspace forbids it);
     - it checks **the opened handle**: refuse it, writing nothing, if `file_attributes()` has `FILE_ATTRIBUTE_REPARSE_POINT` or it isn't a regular file. A planted link or junction there would otherwise turn a SYSTEM service's append into a write to any file;
     - `logging/file.rs`'s `service_log` and `windows/service.rs`'s `log_refusal` both call it. Each keeps its own prefix, and its lines still name no path in the logging case.
   - **The owner and ACL decide "Private", not a flag** (carry-forwards `B-log1b-extra` "planted index.json", `win-log-index-restricted` and `win-log-folder-owner-check`).
     - Today, on Windows, an existing folder is `Private` exactly when its `index.json` says `"restricted": true`. So:
       - anyone who can write to a shared folder can plant that line and silence `logging.folder_shared`;
       - a corrupt index rebuilds with `restricted: false`, so the next save turns a folder Lodestar made and restricted into `Shared`.
     - **Fix it:** for an existing folder, read its owner and DACL through `acl.rs` (the `Windows` ops' `Get-Acl`). It's `Private` only if the owner is `trusted` and the DACL passes `check_dacl`; otherwise it's `Shared`.
     - `restricted` becomes a hint for that check only. A rebuilt index keeps the hint as "unknown", and never writes `false` over a folder the check finds private.
     - **The service's logs folder** sits inside the private data folder, made by `logfile` and inheriting its ACL. It must read as `Private` under the check; add a test.
     - **`files.rs::folder()`** (the API's not-yet-opened folder) gets the same check on Windows, returning `not_private` when it fails. Cache the result per folder per run, because `Get-Acl` starts PowerShell.
     - Never change an existing folder's ACL (B-log1 round 2).
     - If this outgrows `logging/**` and `windows/**`, stop and message the runner. Don't reach into `integrations::secret`.
   - **Wire the runtime into the Windows service** (B-log1: "until B-log1b wires the runtime there").
     - `windows/service.rs` replaces its own subscriber with `settings::log::init_with_writer(filter, Box::new(<the LogFile writer>))`. The text console goes to `<data>\logs\lodestar.log`, where M-auth's docs tell users to find the setup token, and the JSONL file opens through `follow` as everywhere else.
     - Keep `logfile.rs`'s rotation for `lodestar.log`.
     - Its four plain lines become events (`service.starting`, `service.stopped`, `service.failed` with `Error`, `service.stopping`, or ids you choose under `System`).
6. **The CI check and `TARGETS`.**
   - **`log_lint.py` fixes:**
     - **`manifests()` parses each `Cargo.toml` with `tomllib`** (Python 3.11+; CI runs 3.12) instead of line regexes. It walks every dependency table: `dependencies`, `build-dependencies`, `target.<cfg>.dependencies` and `target.<cfg>.build-dependencies`, inline tables included (`[target.'cfg(target_os = "windows")'.dependencies.log]`, `dependencies = { log = "…" }`, `log = { package = "x" }` and a rename to `package = "log"`). It refuses `log` and a renamed `tracing` anywhere but dev-dependencies. A manifest that doesn't parse is a finding (fail closed).
     - **`--base` fails closed:** if the base's copy exists but `listed_in` can't read a `NOT_YET_SWEPT` list from it, that's a finding, not `None`.
     - **`EXEMPT` may only shrink too:** with `--base`, a `(path, rule)` key the base's copy doesn't have is a finding.
     - Each rule gets a case in `test_log_lint.py`.
   - **`NOT_YET_SWEPT` ends empty.** Take each file off as you convert it, in the same commit. If a file can't be swept (only `main.rs` if B-update still holds it), it stays listed, and the PR and the decisions file say why and name the follow-up. When the list is empty, keep the variable and the shrink-only check, so a revert can't grow it back silently.
   - **The TARGETS completeness test** (in `logging/tests.rs`):
     - every workspace member's crate name (from `crates/*/Cargo.toml` `package.name`, `-` → `_`) has a `TARGETS` entry, and every entry names a crate that exists. `lodestar_update` is missing today: add it under `System`;
     - `bench/` (`lodestar_bench`, linked into `serve --fixture`) is covered too, or is exempted by name with a reason;
     - every catalogue a crate declares (`log_events.rs` with `catalogue!`) is in `CATALOGUES`. A crate's catalogue that isn't registered fails, because its events would then never show in `/logs/catalog`.
7. **API and auth events, and the setup token** (PR 2/3).
   - **`auth.setup_token`:**
     - `services.rs:201` becomes `log_event!(AUTH_SETUP_TOKEN, token = SetupToken(&token), address = Listen(bind))`. `Listen` renders as a label; the console text must still read `… enter setup token XXXX-…`, as `tests/serve.rs` and the M-auth docs expect;
     - `auth/http.rs:231` becomes `auth.setup_complete` (no fields);
     - **retire the plain-line special case** (B-log2): `entry.rs`'s `redact_setup_token` on untyped lines goes. Keep `view.rs`'s use on messages for older files, and keep a test that an old plain line still reads `[redacted]` in history and redacted downloads.
   - **`auth.*`** (an `AUTH` catalogue in `auth/log_events.rs`, component `Auth`):
     - sign-in succeeded and failed (`auth.sign_in`, `auth.sign_in_failed`, with `client: Client` and a fixed `reason` label);
     - rate limited;
     - password changed;
     - other sessions revoked;
     - proxy sign-in on and off (never the proxy user, B12);
     - an API key or feed token refused (scope, revoked, unknown: a label, never the key, its prefix or its hash);
     - the key's last use not recorded (`guard.rs:255`);
     - the three `auth.rs` configuration warnings (:168, :213, :236), with the field name as a label.

     Client addresses are the `Client` kind (on in Private, §12.1). Name nothing a user typed except through a wrapper whose kind is right for it. If key create, regenerate or revoke events need `keys.rs`, which you don't own, declare them, list the call sites for a follow-up, and say so.
   - **`api.request`** (`request_log.rs`, one layer in `lodestar_api::router`, outermost so it sees the final status):
     - **Fields:** `method` (a label: the standard methods, else `other`), `route`, `status`, `took_ms` (`Size`) and `client` (`Client`, the TCP peer from `peer_ip`; never a forwarded-for header).
     - **`route` is the matched route template** (`/torrents/{hash}`, `/search`), never the raw path or query, so no hash, search query, `apikey` or `token` can reach it. An unmatched request logs `unmatched`.
       - `Label` takes `&'static str`. Get a `'static` template without an unbounded leak, for example by interning `MatchedPath` into a set bounded by the router's routes, or a new `crates/log` wrapper rendered as a label. Record which.
     - **If you also log the URI** (not required), it goes only through `Url::request`, under the `Url` kind. **The `apikey` and `token` query values are removed whatever URLs are set to** (carry-forward `request_log_redaction`, B12). `q=` and other query parameters must not leak a search query under the `Url` kind; drop the query, or show only its parameter names.
     - **No header is ever logged.**
     - **Level and default:** `Debug`, `default_on: false`, unless Build 3's rule says otherwise for its measured rate with the UI open. `/logs/stream` and `/events` are logged at open only, and never per frame.
   - **Canary positive controls** (`log_canary.rs`). Today run 4 only checks that secrets are absent, which passes even if nothing was logged.
     - For each flow, assert **the line that must appear** in the file (and the console where its kinds allow it), in the same runs that assert the secrets are absent:
       - `auth.setup_token` before setup, `auth.setup_complete` after it;
       - `auth.sign_in` and `auth.sign_in_failed`;
       - `auth.password_changed`;
       - `auth.key_refused` for a bad key in each of its three forms and for a bad feed token;
       - the proxy sign-in;
       - `api.request` for a request carrying `?apikey=` and one carrying `?token=`. The line is there, and the value isn't;
       - each new `system`, `service` and `filters` event the flows reach.
     - Each new event that run 4 can't reach (DHT, fetch, the Windows service) is covered by a unit test with a capture sink that renders it under Full plus both opt-ins and finds no secret. The PR lists which.
8. **The sweep, one crate per commit** (PRs 2/3 and 3/3). **Rules for every line:**
   - no format string: every dynamic part is a declared field of the right kind;
   - errors only through `Error`;
   - paths through `Path`, hashes and fetch keys through `Torrent` (kind `Hash`), peers through `Peer`, URLs through `Url`, addresses the operator must open through `Listen`;
   - counts and sizes through `Size`/`Count`;
   - no `?` Debug of anything that holds a name, path or URL.
   - **Per file** (Build's preamble has the full list):
     - **`crates/api`:** `auth.rs` (3), `auth/guard.rs` (1) and `auth/http.rs` (1), in Build 7.
     - **`crates/filter`:** `lists.rs:580`. It becomes `filters.ip_lists_broad` with the coverage as a whole percentage (`Size`).
     - **`crates/lodestar`:**
       - `services.rs` (7): the setup token as above. `:363` becomes `app.serving` with `Listen` and the data folder as `Path`. `:433` is `app.stopping`. **`:453` becomes the required `app.stop`, which nothing emits today;** the fold flush before it (`mod.rs:836`) must still run.
       - `system/address.rs` (19) and `system/restart.rs` (2): `:129` is the required **`app.restart`** (nothing emits it today). **`:173` logs the executable as a `Path` field** (carry-forward), never inside a message. The address lines name the address the operator must open as `Listen`, so a Private console still tells a headless operator where to go.
       - `windows/{ports,priority,service}.rs` (10), in Build 5.
       - `fixture.rs` (10): the folder as `Path`, and never the claimed password.
       - `insights.rs` (12): counts and error types only, and never a search query from the query log.
       - `main.rs` (3), if free: `:129` and `:159` become `app.command_failed` with `Error`, and `:216` is an import progress event with counts.
     - **`crates/pipeline`** (24, plus #48's):
       - `worker.rs`'s `key = %key` is a **fetch key, so identifying: it becomes `Torrent`**;
       - `writer.rs:233` logs a snapshot path: `Path`;
       - **`lib.rs:391` logs filter notes, which can name rule terms, at every setting.** It becomes a non-detail event carrying the rule id and a fixed note kind. The term goes only into `filters.change_detail`'s opt-in, if anywhere; never name an item a rule sealed;
       - `dht.rs:133`'s `?node.local_addrs()` becomes `Listen` fields or a count.
     - **`crates/search`:** `rebuild.rs` (15), `index.rs` (2) and `live.rs` (2). Index versions and attempts are labels or counts; paths are `Path`; `index.rs:958`'s `id` is a torrent row id, so `Size` or `Torrent` as fits.
     - **`crates/dht` and `crates/peer-wire/src/fetch.rs`** (new events, after Build 3):
       - at least `dht.key_harvested` (`Torrent`, source label, peer count) at `harvest_hash`, and `fetch.failed` (`Torrent`, `Peer`, `Error`, attempt label) at `fetch`'s result;
       - plus whatever else Build 3's rule allows, and the periodic summaries;
       - **a `Peer` beside a `Torrent` is peer activity:** it shows only with that opt-in, never on the console (B-log1's rendering rule). Test it per output;
       - **the actor and the fetch loop never block on logging.** Check that `emit` takes no lock the DHT actor could wait on for long, and say so in the PR.
   - **Each crate that declares events** adds its catalogue to `logging::CATALOGUES` (one line each). Each new id is unique (`check_ids`), and the TARGETS test passes.
9. **A bounded graceful shutdown** (`services.rs` `Services::run`, carry-forward `bounded-graceful-shutdown`). This is the one change to `Services::run`'s logic.
   - **Today:** a client that holds a log download or an export open without reading keeps `with_graceful_shutdown` waiting forever, so `handles.shutdown()` (queue and DHT state saved, last batch committed, index stopped) never runs.
   - **Change:**
     - when the signal fires, after the existing `hub.close(); jobs.close(); logs.close();`, start a **10 s** timer;
     - race the serve future against it (`tokio::select!`, with the signal shared through a `watch` or `Notify`; no new dependency);
     - when the timer wins, log a new non-required `app.shutdown_forced` (`waited_s`) and drop the serve future;
     - then `events.close()` and `handles.shutdown()` as now.
   - **Check what dropping axum 0.8's `serve` future does to connection tasks it already spawned,** and record it. They must not hold anything `handles.shutdown()` waits on. `search.stop()` already works through a shared handle.
   - **Test** (`crates/lodestar/tests/shutdown.rs`, Unix, against the real binary as `tests/serve.rs` does):
     1. Seed the log folder with a rotated file larger than the socket buffers (several MiB of valid lines; 0700 folder, 0600 file), so the download can't finish into the kernel's buffers.
     2. Set up, sign in, and start `GET /logs/files/<it>?redact=false` without reading the body. Also start an export the same way, if it's cheap.
     3. Send `SIGTERM`.
     4. **The process exits 0 within 10 s + a margin (30 s in all)**, and the file shows `app.shutdown_forced` and then `app.stop`.

     A second case: with no stalled client, shutdown doesn't wait the 10 s.

## Tests
- **Hot path:**
  - the padded `Event` keeps `count` on its own cache line (`align_of`, and the offset of `on` against `count`);
  - off events still count;
  - the bench's ignored tests run in CI, and gate with `LODESTAR_LOG_BENCH_GATE=1`.
- **URL scrubbing:**
  - the five raw characters in a password (free text and typed);
  - the `Debug` forms of `url::Url` and `reqwest::Url`;
  - the linearity test for each adversarial pattern;
  - a property test that `scrub_text` never panics and never lets a planted `u:<secret>@` or `?passkey=<secret>` through, whatever the surrounding characters.
- **Windows** (on Windows CI):
  - the temp-log helper refuses a symlink and a junction named `lodestar-service.log`, and appends to a regular file;
  - a planted `"restricted": true` in a shared folder still logs `logging.folder_shared`;
  - a corrupt index in a folder Lodestar made stays `Private` after a save and a restart;
  - the service's logs folder under the private data folder is `Private`;
  - `files.rs::folder()` refuses a configured folder owned by another account;
  - the service's runtime writes `lodestar.jsonl` and `lodestar.log` holds the setup token's line.
- **Lint:** each new rule in `test_log_lint.py` (target tables, inline tables, renames, an unparseable manifest, `--base` against an unreadable list, a grown `EXEMPT`), and the tree passes with an empty (or justified) `NOT_YET_SWEPT`.
- **TARGETS and CATALOGUES:** the completeness test, and a deliberately missing entry fails it.
- **Events:**
  - every converted line renders under Private with no name, path, hash, URL, query or peer;
  - each kind renders per output as B-log1's tests do;
  - **the canary's run 2 gains positive controls:** for each kind a reachable event now carries (`Path` from the snapshot purge or the fixture, `Torrent` from the pipeline, and any others), the value appears while that kind is on and not otherwise. "Narrow canary limits" lists only what's still unreachable, by name.
- **API:**
  - `api.request` carries the template, never the raw path or query;
  - `apikey`, `token` and `q` values never appear with every kind on;
  - no header ever appears;
  - an unmatched route logs `unmatched`.
- **Shutdown:** both cases in `tests/shutdown.rs`.
- **Canary:** all four runs, with the positive controls of Build 7.

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass after merging `origin/main`, on Linux and on Windows CI.
- `python3 .github/scripts/test_log_lint.py` and `python3 .github/scripts/log_lint.py --base origin/main` pass. **`NOT_YET_SWEPT` is empty**, or the PR says which file stays and why.
- The bench step is green, and its numbers and the `default_on` verdicts are in the PR.
- Goldens are regenerated with `LODESTAR_BLESS=1` after the last merge of `main`, and `cargo run -p lodestar --bin openapi` + `cd web && npm run api:types` leave no diff.
- `/security-review` has been run on each PR's branch; its findings and fixes are in that PR.
- **The PRs are titled:**
  - `B-log1b (1/3): Logging core: padded event counters, hot-path bench, URL scrub fixes, Windows log-folder hardening, lint and TARGETS checks`;
  - `B-log1b (2/3): Logging sweep: api.request, auth events, setup token, lodestar crate, bounded shutdown`;
  - `B-log1b (3/3): Logging sweep: pipeline, search, DHT and fetch events`.

  If you join parts, the title says which.

## Report
Each PR description lists:
- **The events it adds:** ids per component, levels, `default_on`, fields and their kinds, and which are required. The new message keys (`logs.event.<id>.label`/`.help`) are for S-logs and H-zh.
- **The call sites converted,** file by file, and what's left in `NOT_YET_SWEPT` with the reason.
- **The bench's numbers** (a)–(d), the peak rates used and where they came from, padded against unpadded, whether counters are striped, and **each high-volume event's verdict** under the `default_on` rule.
- **The URL rules added:** the raw-character handling and its look-ahead bound, the `Debug` forms, and the linear `scrub_text`.
- **The Windows changes:**
  - the temp-log helper and its two callers;
  - the ACL check that replaces the `restricted` flag;
  - the service's runtime wiring;
  - what `lodestar.log` holds now.
- **The lint changes** (`tomllib`, fail-closed `--base`, shrink-only `EXEMPT`) and the TARGETS and CATALOGUES test.
- **`api.request`'s shape:** how `route` is made `'static`, what is never logged, and the canary's positive controls.
- **The shutdown change:** the 10 s bound, what dropping the serve future does to axum's connection tasks, and the test.
- **Every shared-file line** (`api/lib.rs`, `peer-wire/lib.rs`, `filter/lib.rs`, the three `Cargo.toml`s, `ci.yml`, `services.rs`, `main.rs`).
- **Follow-ups:**
  - `tracker_scrape`'s `Debug` over URLs;
  - key-management events in `keys.rs`, if not done;
  - `lodestar_log_records_total{level}` (`metrics.rs`);
  - the Windows Event Log sink;
  - anything the canary still can't reach.
- **The user-facing steps for the docs follow-up** (S-logs, the guide and `docs/operations.md`):
  - where the setup token shows on each platform, unchanged;
  - that sign-ins and API requests can now be logged, and what they show;
  - that DHT and fetch detail joins only at Detailed or Everything.
- **The §2.19 refinements:**
  - the bench gates and the `default_on` rule;
  - over-redaction of ambiguous userinfo;
  - ACL-based folder privacy;
  - the 10 s shutdown bound;
  - `api.request` logging route templates only.

**Message the runner** (steward §2) when each PR is ready, with its head. Also message the runner if `main.rs` is still held by B-update when PR 2/3 is otherwise ready, or if the Windows ACL check doesn't fit in `logging/**` and `windows/**`.

## Runner decisions (2026-10-23, apply over anything above)
1. **Ownership:** this brief's extra paths are yours. They are `crates/log/**`, `crates/lodestar/src/system/**`, `search/src/{index,live}.rs`, `filter/src/lists.rs`, the single lines in filter's and peer-wire's `lib.rs`/`Cargo.toml`, `api/src/request_log.rs` with its two `api/src/lib.rs` lines, and the `fixture.rs`/`insights.rs` call sites. The runner adds them to slices.yaml.
2. **`main.rs`:** add its call-site lines only after B-update's #128 has merged, in PR 2 or later. If B-update part 3 is open then, keep the lines minimal and merge main before pushing. Until then, `main.rs` stays in `NOT_YET_SWEPT` with the reason stated.
3. **`services.rs`:** confirmed. You get its 7 call sites plus the bounded shutdown.
4. **Bench gates, approved as runner refinements (§2.19):**
   - an event that's switched off costs ≤ 5 ns;
   - padded at 8 threads, ≤ 2× that;
   - `default_on` only if CPU at the peak rate is ≤ 1% of a core and the file adds ≤ ~18 MiB/day; otherwise the event is off by default with a periodic summary event;
   - a 10 s bound on shutdown;
   - the bench lives as ignored release tests in the binary crate, plus one `ci.yml` step.
5. **`api.request`:** log the matched route template only, never the raw path or query, at Debug level with `default_on: false`. If a URI is ever added, it goes through `Url::request` with the apikey and token redacted.
6. **`tracker_scrape::{TrackerUrl, HttpTracker}` `Debug` over passkey URLs:** a runner carry-forward for that crate's owner. Not yours.
7. **The Windows service's `lodestar.log`:** confirmed. It stays, fed by the runtime's console output, and replaces M6-win's stand-in subscriber.
8. **Launch timing:** this slice launches after M4 (#48) merges and the owner lifts the usage freeze.
