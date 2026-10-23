# B-update: Update engine, signed manifest, key chain, overlay supervisor
Covers §12.2 end to end for Docker, Unraid and the Linux tarball/systemd unit: the signed manifest and key chain, trust state, the single signed update check, download and verify, the overlay supervisor with probation and journaled rollback, the self-test, history, the update policy settings, the updates API and the `updates` SSE topic. Windows gets check and notify only: the MSI helper and the signed manifest in `release.yml` land with E-M6.

**Model:** Opus. This is the supply chain:
- it parses untrusted network input (the manifest envelope, signatures, statements) and decides what code runs next;
- it writes an on-disk trust state whose rules can't be undone by a rollback;
- it becomes PID 1 in the container and replaces the running program;
- its rollback renames files over the live store.

**Branch:** `slice/B-update`, for all three PRs (below): after a PR merges, `git fetch origin main && git merge origin/main` into the same branch (merge commits; never rebase) and open the next PR from it. **`/security-review` is required on every PR** (supply chain, standards §5.7), on top of the fresh Opus review. **Follow `.claude/skills/steward/SKILL.md` from your first push;** when a PR merges, don't stop: message the runner and start the next part.

**Spec:**
- `runner/standards.md`.
- `docs/owner-decisions.md`:
  - **§2.13** (off until chosen; Check and notify or Install automatically at Fixes only / Fixes and features / All releases; Always install security fixes; Stable / Beta / Nightly; never automatic with manual steps; every update signed, verified, in a window, health-checked, rolled back on failure; nothing sent about the install; the release key `02910813433E81E0` and the offline recovery key `67922DDF005FD310`);
  - **§2.21** "Release signing uses separate keys by role" (release key for Stable and Beta, nightly key trusted only on Nightly, freshness key that signs only freshness statements, owner creates the nightly and freshness pairs) and "Recovery key: an accepted risk" (no rotation statement);
  - **§2.23** (no Authenticode certificate; the Ed25519 manifest and its SHA-256 and size pins are the trust anchor; Authenticode checked only on artifacts that carry a signature);
  - §1.2.3 (offline by default), §2.14 (user-facing steps go to the docs follow-up), §2.19 (refinements you record and the runner flags).
- `docs/architecture.md`:
  - **§12.2, all of it**, including the "B-update must resolve" checklist and the Tests list. Every bullet there is a requirement unless this brief says otherwise;
  - §10.10.14 (what the Updates screen needs from the API: status card steps, notices, history, Roll back, Pause, the "can't update itself" reasons);
  - §10.10.13's Updates bullet ("Check now" is the signed check) and "Image 0.5.2 · running 0.5.3 (updated in place)" (§12.2);
  - §10.10.10 (the wizard asks; default Off), §10.10.6 (the restart overlay is reused for the restart step), §10.10.7 (Roll back is a Danger-zone action; no update action is in the Security row);
  - §11 (Docker `distroless/static`, `ENTRYPOINT ["/lodestar"] CMD ["serve"]`, PUID/PGID, Unraid, Windows service and portable, systemd), §12 (Upgrades: the automatic snapshot, `lodestar rollback`, "No telemetry, ever").
- `docs/architecture.md` §16: "Auto-update (owner, 2026-10-22…)", "Review fixes (runner review of PR #86)" rounds 1–3 (every update bullet), "About screen" (the update check is B-update's), the **B4 decisions** (topics, `Topic::bit`, the `u8` set) and the **D-puid decisions** (the drop happens first, single-threaded; `no_new_privs`).
- Decision files:
  - **`docs/decisions/2026-10-23-B2c.md`**: Restart ("main.rs's `serve` races the stop signals with `Restart::requested()`", "A signal wins", `reexec` on `current_exe()`, the `(deleted)` fallback to `/proc/self/exe`, "runs as the user PUID/PGID dropped to", "A temporary fixture… can't restart", "B-update's supervisor may replace the re-exec with a supervised relaunch. It keeps `requested()` and the method enum"), and "/system": `updates` is null until B-update;
  - **`docs/decisions/2026-10-22-B2b.md`**: the settings registry, schema hints, `null` resets, "The hub has 6 topics; a compile-time check guards the `u8` set (`updates` will make 7)";
  - **`docs/decisions/2026-10-22-M6-win.md`**: the service entrypoint, "Exit codes" (`NO_ERROR`, service-specific 1 on error, 3 refused for safety), the data-folder rules (never repair; no links beneath), "the later `update-helper` (§12.2) is a separate process";
  - **`docs/decisions/2026-10-22-M6-cli.md`**: exit status 0/1/2/3, "B-update's journal" (the CLI refuses while `<data>/bin/rollback.json` exists and never writes in `<data>/bin`), "Two `serve` instances are kept apart only by the search index's lock… a gap for B-update's `<data>/bin` lock to close", and "`lodestar rollback` for a whole update… should go through its journal";
  - `docs/decisions/2026-10-23-B12.md` (the fresh-password primitive `auth::require_fresh_password`; no update route uses it, see Build 3.7), `docs/decisions/2026-10-23-B16.md` and M6-cli's "Any later service that reaches out must check the same `fixture::OFFLINE` gate", `docs/decisions/2026-10-23-S-snapshots.md` (snapshot names, purge, `snapshots::settle`, "B-update's journaled rollback keeps every process off the store the same way");
  - **P-release** (#92, open): read `docs/decisions/2026-10-22-P-release.md` **on `origin/slice/P-release`**: "Release signing: environments and owner setup" (`RELEASE_SIGNING_KEY` in `release`, `NIGHTLY_SIGNING_KEY` in `nightly`), "Checksums and their signature" (`SHA256SUMS.minisig`, made with Ubuntu's `minisign -S`), "Release builds leave out `fixture`", and "Not done: … release-manifest signing (B-update)".
- `packaging/keys/` (`README.md`, `lodestar-release.pub`, `lodestar-recovery.pub`): **minisign, Ed25519**. Only the release and recovery public keys exist; `lodestar-nightly.pub` and `lodestar-freshness.pub` are "(to be generated)" by the owner.
- `runner/slices.yaml`'s B-update entry, including its notes (trust state in `<data>/bin/trust.json`; each overlay keeps its full signed manifest and key chain; the 1-day-ahead refusal; the 30-day expiry cap; journaled rollback; `history.json`; "Owns main.rs because serve becomes the supervisor"; the `updates` topic, 7 of 8 bits; the trust floor).
- **The code:**
  - `crates/lodestar/src/main.rs`: `main()` (the `privdrop::run` scope match, `Command::Serve { fixture: None }`, `Command::ImportBitmagnet`), `serve()` (the B2c restart race), `healthcheck()`, `shutdown_signal()`, `unix_now()`;
  - `crates/lodestar/src/system/restart.rs`: `Restart::{global, started, listening, request, was_requested, requested, refuse, available, info}`, `METHOD`, `reexec()`, the private `program()`; `crates/api/src/dto/system.rs` `RestartMethod { Reexec, Manual }` and `RestartInfo`; `crates/lodestar/src/system.rs` (`System::connections`, the `ConnectionKind::Updates` row, null today); `crates/lodestar/src/system/info.rs` (`build()`, `channel()`, `deployment_here()`, `LODESTAR_BUILD_{TARGET,CHANNEL,COMMIT}`);
  - `crates/lodestar/src/cli/mod.rs` (`Command::Serve { fixture }`, `RollbackArgs`), `crates/lodestar/src/cli/util.rs` (`rollback_journal`, `refuse_during_update_rollback`, `schema_version`, `open_read_only`, `quick_check`, `sync_file`, `sync_dir`, `create_new_private`, `side_files`, `Failure::Refused` = exit 3), `crates/lodestar/src/cli/rollback.rs`, `crates/lodestar/tests/cli.rs` (the `bin/rollback.json` refusal test);
  - `crates/lodestar/src/services.rs`: `Services::{start, run, shutdown}`, the registry's first `Store::open` (where migrations run), `EventHub::start`, `system::address::bind`, `.layer(Extension(system))`;
  - `crates/lodestar/src/privdrop.rs` (`run`, `Scope::{DataDir, ProcessOnly}`, `Report`), `crates/lodestar/src/fixture.rs` (`active()`, `unless_offline`, the temporary-fixture `Restart::refuse`), `crates/lodestar/src/windows/service.rs` (line ~86: `Some(crate::cli::Command::Serve { fixture: None })`);
  - `crates/store/src/schema.rs` (`migrate_with`: `VACUUM INTO` straight to `snapshot_path(path, current + 1)`, then one transaction per migration; `Error::SchemaTooNew`), `crates/store/src/snapshots.rs` (`list`, `parse_name`, `Kind::PreMigration`, `settle`), `Store::max_id`, `Store::checkpoint`;
  - `crates/api/src/dto/events.rs` (`Topic`, `Topic::ALL` = 6, `bit()`, the `u8` assert, `Event`, `Event::topic`), `crates/api/src/events.rs` (`EventHub::publish`, the "Six topics now" test at line ~1030), `crates/api/src/system.rs` (the `SystemApi` trait as an `Extension`: the pattern to copy), `crates/api/src/auth.rs` (`protected`), `crates/api/src/extract.rs` (`same_origin_json`);
  - `crates/integrations/src/net.rs`: `GuardedClient::{new, with_resolver, send, send_following}`, `NetPolicy::{notification, download_client}`, `check_url`, `read_limited`, `MAX_REDIRECTS`; `crates/integrations/src/lib.rs` `USER_AGENT` (it carries the version: never use it here);
  - `crates/lodestar/src/settings/` (`README.md` "Adding a section", `Registry::load`, `Field`, `Auto::Str`, `screen_tests.rs`' `units` and formats lists), `crates/lodestar/src/config.rs` (`ENV_VARS`, the `LODESTAR_SHOW_ADULT` pattern);
  - `crates/lodestar/tests/system.rs` (`system_reports_and_restarts_in_place`: same PID after a restart, then SIGTERM exits 0), `crates/lodestar/tests/serve.rs`, `crates/lodestar/tests/privdrop.rs`;
  - `fuzz/` (`Cargo.toml`, `README.md`, `fuzz_targets/lists.rs` as the pattern), `.github/scripts/ci_changes.py` (fuzz targets and crates are discovered), `packaging/docker/Dockerfile` (the build stage copies only `Cargo.toml Cargo.lock rust-toolchain.toml crates bench`).

**Not on `origin/main` yet:** everything under `crates/update/` and `crates/lodestar/src/update/`, `crates/api/src/{updates.rs,dto/updates.rs}`, `crates/lodestar/src/settings/updates.rs`, and every name this brief marks **(new)** (`Services::probe`, `lodestar_store::latest_schema_version`, `RestartMethod::Supervised`, `Topic::Updates`, `UpdatesEvent`, `UpdatesApi`, `update::Engine`, `LODESTAR_BUILD_VERSION`, `LODESTAR_SUPERVISOR`, the `LODESTAR_UPDATES*` variables). `log_event!`, `logging::TARGETS` and `.github/scripts/log_lint.py` are on #119's branch only. `lodestar-nightly.pub` and `lodestar-freshness.pub` don't exist anywhere.

## The split: three PRs on `slice/B-update`, each reviewed and `/security-review`ed on its own
| PR | Title | Scope | Size target |
|---|---|---|---|
| 1 | `B-update: Update engine, signed manifest, key chain, overlay supervisor (1/3: trust core)` | the new pure crate `crates/update`: minisign, envelope, manifest, statements, trust state, verification, fuzz target | ≲ 2,000 lines, about half tests |
| 2 | `… (2/3: supervisor and rollback)` | `serve` as supervisor, overlays, probation, control channel, rollback journal, history writes, self-test, `--child`, B2c restart composition, the store's snapshot sequence | ≲ 2,500 lines. **Stated reason** for going over 2,000: the journal, probation accounting and control channel are one safety unit; neither half is testable or reviewable alone |
| 3 | `… (3/3: check, install, policy, API)` | the signed check, download and staging, policy and window, the `updates` settings section, `/updates`, the `updates` topic, history API, the `/system` row | ≲ 2,000 lines |

PR 1 changes no behaviour. PR 2 is inert until PR 3 (nothing stages an overlay outside tests), except that `serve` on Unix now supervises a child. If a PR would exceed its target by more than 20%, stop and tell the runner with a proposed cut; don't silently grow it.

## Own
**PR 1:**
- `crates/update/**` (new crate `lodestar-update`, not in slices.yaml's owns: the runner widens it). It must be a library so `cargo fuzz` can reach the parsers; `crates/lodestar` is a binary.
- One line in the root `Cargo.toml` `[workspace.dependencies]`: `lodestar-update = { path = "crates/update" }`; `Cargo.lock`.
- `fuzz/fuzz_targets/update.rs` (new), its `[[bin]]` and dependency lines in `fuzz/Cargo.toml`, `fuzz/Cargo.lock`, one row in `fuzz/README.md`.
- `packaging/keys/README.md`: the exact statement and signing format, in the "Replacing a key" steps (owner-facing; developer doc, exempt from `user-docs`).
- `docs/decisions/<date>-B-update.md`.

**PR 2:**
- `crates/lodestar/src/update/**` (new): `mod.rs`, `layout.rs`, `control.rs`, `supervisor.rs`, `pending.rs`, `overlay.rs`, `journal.rs`, `history.rs`, `probation.rs`, `testing.rs`, with `tests` submodules.
- `crates/lodestar/src/main.rs` (owned): `mod update;` placed between `mod system;` and `mod web;` (away from #119's `mod logging;` and #121's `mod percolator;`), the supervisor dispatch, the `--child` path, the refusals, the restart exit.
- **Hook lines, each named in the PR:**
  - `crates/lodestar/src/cli/mod.rs`: a hidden `child: bool` on `Command::Serve` (`#[arg(long, hide = true)]`), `--update` on `RollbackArgs`, and the arms they need; `crates/lodestar/src/cli/rollback.rs`: the `--update` branch calling `update::journal` (nothing else there);
  - `crates/lodestar/src/windows/service.rs`: the one pattern at line ~86 gains `child: false`;
  - `crates/lodestar/src/system/restart.rs`: the supervised method (Build 2.8); `crates/api/src/dto/system.rs`: `RestartMethod::Supervised` (one variant and its doc);
  - `crates/lodestar/src/services.rs`: one accessor for the self-test (`Services::probe()`, new: the bound HTTP address, whether the DHT is bound, a `SearchHandle` clone) and nothing else;
  - `crates/store/src/schema.rs` (**scope exception**, on-disk format): the pre-migration snapshot through a temporary file, fsync, rename and a directory fsync, plus `pub fn latest_schema_version() -> i64` re-exported from `crates/store/src/lib.rs`;
  - `crates/lodestar/Cargo.toml`: `lodestar-update.workspace = true`; `Cargo.lock`;
  - `crates/lodestar/build.rs`: `LODESTAR_BUILD_VERSION` (Build 2.9), beside B2c's `LODESTAR_BUILD_*` lines.
- `crates/lodestar/tests/supervisor.rs` (new, `cfg(unix)`), the regenerated `web/src/lib/api/openapi.json` + `schema.d.ts`, `docs/decisions/<date>-B-update-r2.md`.

**PR 3:**
- `crates/lodestar/src/update/{engine,check,download,install,policy,window,trust_file,api}.rs` (new), `crates/lodestar/src/settings/updates.rs` (new) with its golden in `settings/golden/` and cases in `settings/api_tests.rs`.
- `crates/api/src/updates.rs` and `crates/api/src/dto/updates.rs` (new); one registration line each in `crates/api/src/{lib.rs,openapi.rs,dto/mod.rs}`; `crates/api/tests/updates.rs` (new) with goldens; `tests/openapi.rs` `ROUTES`; the 401 list in `tests/api.rs`; regenerated `openapi.json` + `schema.d.ts`.
- **Hook lines, each named in the PR:**
  - `crates/api/src/dto/events.rs`: `Topic::Updates` (last, so the existing bits keep their values), its `ALL` entry and `name`, `UpdatesEvent`, `Event::Updates` and its `topic` arm; `crates/api/src/events.rs`: the "Six topics now" assertion becomes 7 (nothing else);
  - `crates/lodestar/src/settings/mod.rs`: one `registry.register::<updates::Updates>(config)?;` line before `preferences`, `pub mod updates;`, and the vocabulary doc comment (`days`, `time-range`); `settings/screen_tests.rs`: `"days"` in `units`, `"time-range"` in the formats list;
  - `crates/lodestar/src/config.rs`: an `[updates]` table, its `LODESTAR_UPDATES*` parsing and `ENV_VARS` entries;
  - `crates/lodestar/src/services.rs`: building `update::Engine` (skipped in fixture mode), one `.layer(Extension(..))` beside `system`'s, its shutdown;
  - `crates/lodestar/src/system.rs`: the `ConnectionKind::Updates` row from the engine's status (one field on `System`, set in `System::start`);
  - `crates/lodestar/Cargo.toml`: `lodestar-integrations.workspace = true` (already in the binary through `lodestar-api`) and, if you use it, `jiff` (Build 3.6); `Cargo.lock`; regenerated `web/licenses/rust.json`.
- `docs/decisions/<date>-B-update-r3.md`.

**Don't edit:**
- `crates/lodestar/src/logging/**`, `crates/lodestar/src/settings/{logging,log}.rs`, `crates/log/**` (#119 B-log1, then B-log1b);
- `crates/lodestar/src/windows/**` beyond the one pattern line (M6's restart hook is theirs), `crates/api/src/auth/**`, `crates/integrations/**` (use `GuardedClient` as it is), `crates/search/**`, `crates/store/**` beyond the `schema.rs`/`lib.rs` lines;
- `.github/workflows/release.yml`, `packaging/docker/**`, `packaging/linux/**`, `packaging/windows/**` (#92), `packaging/systemd/**`, `packaging/unraid/**` (#119), `packaging/keys/*.pub`;
- `web/src/**` except the two generated API files; `docs/user/**` (#91) and `docs/operations.md`. Put user-facing steps in each PR for the docs follow-up.

**Nearby open PRs** (check `gh api repos/dvortsis/lodestar-v2/pulls?state=open` again before each PR):
- **#119 B-log1** (review): `main.rs` (`mod logging;`; its diff shows B2c's `serve()` body as removed only because it branched before #117: keep B2c's lines and #119's), `settings/{mod.rs,README.md,api_tests.rs,tests.rs,screen_tests.rs,general.rs}`, `privdrop.rs`, `crates/api/src/{lib.rs,openapi.rs,dto/mod.rs,error.rs}`, `tests/{api,openapi}.rs`, both `Cargo.toml`s and `Cargo.lock`, `packaging/systemd`, `packaging/unraid`. **Expect to merge its `main.rs` and keep its logging init** (`mod logging;`, `init_logging` → `settings::log::init`). Once it merges, `.github/scripts/log_lint.py` refuses plain `tracing` macros and `%e`/`?e` fields in new files: log through `log_event!` with your events declared in a catalogue (component `System`), and register any new target in `logging::TARGETS` as a named hook line, coordinating with B-log1b if the catalogue lives in `logging/events.rs`.
- **#121 B9** (review): `main.rs` (`mod percolator;`), `services.rs`, `dto/events.rs` and `events.rs` (alerts payload; no new topic), `crates/api/src/{lib.rs,openapi.rs,dto/mod.rs}`, `tests/{api,openapi}.rs`.
- **#92 P-release** (running): `release.yml`, `packaging/docker/Dockerfile`, `packaging/linux/**`, `packaging/windows/**`.
- **#48 M4** (`claude/relaxed-knuth-vexgae`): `config.rs` (+227 lines), `services.rs`, `cli/mod.rs`, `settings/api_tests.rs`, `tests/serve.rs`, `crates/api/src/lib.rs`, `tests/{api,openapi}.rs`, `crates/lodestar/Cargo.toml`, `Cargo.lock`.
- **B11b** (running, no PR yet): owns `crates/lodestar/src/cli.rs` (now `cli/mod.rs`) for `rollup --repair`, and `crates/api/src/insights.rs`.
- #91 DOC-install, #103 W9 and #122 don't overlap.

Keep hook lines additive and away from theirs, and merge `main` as each lands.

## Build
### PR 1: trust core (`crates/update`, pure, no tokio, no network, no logging)
1. **Dependencies** (approved here, pinned exactly; every one is already in `Cargo.lock`, so no crate is new to the tree): `ring = "0.17.14"` (Ed25519 verify; Ed25519 signing in tests), `blake2 = "0.11.0"` (minisign's prehash), `sha2 = "0.11.0"`, `base64 = "0.22.1"`, `semver = "1.0.28"`, `serde` (derive) and `serde_json` as the workspace uses them; dev: `proptest = "1.11.0"`. **Not** `minisign-verify` (its own curve code, a new crate) and not OpenPGP: the key IDs in `packaging/keys` are minisign key numbers.
2. **minisign, written to the format** (`minisign.rs`):
   - **Public key:** the base64 line of 42 bytes, `Ed` + key number (8) + Ed25519 key (32). The key ID shown (`02910813433E81E0`) is the key number read as a little-endian `u64`, in upper-case hex; test both real `.pub` files decode to their documented IDs.
   - **Signature file:** exactly four lines (LF or CRLF; nothing else): `untrusted comment: …`, the base64 of 74 bytes (`ED` + key number + 64-byte signature), `trusted comment: …`, the base64 global signature (64 bytes). Each comment ≤ 512 bytes of printable UTF-8; the file ≤ 1 KiB.
   - **Only prehashed `ED` is accepted** (Ed25519 over BLAKE2b-512 of the message; minisign ≥ 0.10's default, and what #92's `minisign -S` writes). Legacy `Ed` is refused **(§2.19)**.
   - **Verify:** key number matches, then the signature over the prehash, then the global signature over `signature ‖ trusted comment bytes`. All three or nothing. Strict base64 (standard alphabet, padding, no whitespace).
3. **Verify, then parse.** Every signed document is the **exact bytes** that were signed, carried as `{"body": <base64>, "minisig": <text>}`. Nothing in a body is read before its signature verifies under a key allowed for it. Then it's parsed as strict JSON: UTF-8, no BOM, `#[serde(deny_unknown_fields)]`, serde's duplicate-field refusal, integers only (no floats), and each body has a **`type`** field so one kind can never be read as another (`lodestar-manifest`, `lodestar-key`, `lodestar-freshness`). This also keeps the release key's `SHA256SUMS.minisig` from being taken as a manifest.
4. **The documents** (the format is the contract E-M6's `release.yml` and the weekly re-sign must produce; write it into the crate's README and the decisions file):
   - **The envelope** (one file per channel, unsigned, so a check is **one request**, §12.2): `{"manifest": {body, minisig}, "freshness": {body, minisig} | null}`.
   - **Manifest body:** `{type, format: 1, channel, timestamp, expires, statements: [{body, minisig, cosig?}], releases: [{version, date, level: "fix"|"feature"|"major", security, manual_steps, min_from, withdrawn, migrates, reindex, reindex_bytes, notes, notes_url, artifacts: [{target, kind: "binary"|"msi"|"portable", url, sha256, size}]}]}`. An unknown `format` is refused with a "needs a newer Lodestar" reason; publishers keep format 1 until every supported version reads its successor.
   - **Key statement body:** `{type, seq, role: "release"|"nightly"|"freshness", key: <minisign public-key line>, timestamp}`. `minisig` is the recovery key's; `cosig` (the old key of that role) is optional and never sufficient. The owner can make one with the minisign CLI over the body file.
   - **Freshness body:** `{type, channel, manifest_sha256, timestamp, expires, statements: [...]}`, signed by the freshness key.
   - **Caps** (refuse, don't truncate): envelope 512 KiB, manifest body 384 KiB, freshness body 64 KiB, statement body 2 KiB; 64 statements, 64 releases, 8 artifacts per release; notes 16 KiB; versions strict semver ≤ 64 bytes, unique; `url` ≤ 512 bytes, `https` only, host on the download allowlist (Build 3.2); `sha256` 64 lower-case hex; `size` 1 byte to 256 MiB; `target` one of the shipped triples (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-pc-windows-msvc`).
5. **Embedded keys** (`keys.rs`, `crates/update/keys/`):
   - `lodestar-release.pub` and `lodestar-recovery.pub` are **copied** into `crates/update/keys/` and embedded with `include_str!`, because the Docker build context has only `crates/`. A test reads `../../packaging/keys/*.pub` when present and fails on any difference, and fails if `packaging/keys/lodestar-{nightly,freshness}.pub` exists without a matching embedded copy.
   - **Generation 0:** recovery, and release (allowed on Stable and Beta). Nightly and freshness have no key until the owner creates them (§2.21). Then either their `.pub` is copied in (a one-line change, the default) or a recovery-signed statement introduces the role. Until then, Nightly verifies nothing and freshness statements are refused; the check reports both as reasons, never as a silent failure.
   - `crates/update/keys/statements.json`: the key statements this build knows of (empty now), embedded; the trust floor's first layer.
6. **The chain and the trust floor** (`chain.rs`):
   - A statement is accepted only with a valid **recovery** signature, `timestamp` ≤ now + 1 day, and `seq` exactly one above the current sequence (one sequence for all roles). Accepting it revokes the previous key **of that role only**. A role with no key is introduced by its first statement.
   - **Contiguity and equivocation:** a document's statements at or below the current sequence must equal the accepted ones byte for byte; a different statement with an accepted `seq` refuses the document and raises a notice. A gap refuses it.
   - **The floor** is the newest valid chain among the embedded statements, `trust.json`'s, and those inside any verified overlay (§12.2). Revoked keys are derived from it.
7. **Trust state** (`trust.rs`, the `trust.json` model; the file's I/O is PR 3's): `{format, seq, statements, revoked, channels: {<channel>: {hwm_ts, hwm_sha256, fresh_ts}}}`, strict, ≤ 256 KiB.
   - **High-water mark per channel,** on the manifest's own signed timestamp. An older manifest is refused. **The same timestamp is accepted only with the same SHA-256** (a refetch); a different body at the mark is refused **(§2.19)**.
   - **The freshness mark is kept per channel (§2.19):** §12.2 names one mark, but a single mark would refuse the other channel's statement after a channel switch; per channel keeps the same replay protection.
   - **Time limits:** a manifest or freshness statement dated more than **1 day** ahead of the local clock is refused; `expires` must be after `timestamp` and at most **30 days** after it. A manifest is **current** while it, or its newest accepted freshness statement, hasn't expired; one that isn't current blocks installs only.
   - **Freshness acceptance:** the current, non-revoked freshness key signed it; channel and `manifest_sha256` match; the manifest verifies under a non-revoked key allowed on that channel; its timestamp is newer than the channel's freshness mark; the manifest isn't older than the high-water mark. It extends expiry only.
   - **Rotations:** a release or nightly rotation sets the high-water mark of the channels that key signs to the statement's timestamp; a freshness rotation resets only the freshness marks.
   - **Forward only:** a new state replaces the old only if its sequence is higher, or equal with no channel's marks lower. A merge function enforces it; nothing else writes a state.
   - **Clock notices:** genuine manifests from more than a day ahead (`clock_behind`), and a clock past the newest accepted manifest's expiry while checks succeed (`clock_ahead`).
8. **Verification entry points** (`verify.rs`; names are suggestions):
   - `check(envelope_bytes, channel, now, &TrustState) -> Result<Checked>`: everything above. Returns the parsed manifest, whether it's current, the next trust state and notices.
   - `install(&Checked, version, target) -> Result<Pin>`: not withdrawn, the artifact for this target and kind, its pins.
   - `overlay(manifest_bytes, minisig, binary_sha256, binary_len, version, target, &Embedded) -> Result<Overlay>`: **from the built-in recovery key alone** (embedded keys and statements plus the overlay's own statements); no clock, no expiry, no high-water mark (§12.2). It returns the signing key and whether the **embedded** statements revoke it (refuse) versus only a later one (start, with a notice).
   - Role rules: Stable and Beta manifests need the release key; Nightly needs the nightly key. A manifest naming another channel than asked is refused.
9. **No panics on any input,** no allocation beyond the caps, and no `unwrap` on parsed data.

### PR 2: the supervisor and rollback (`crates/lodestar/src/update/`, Unix)
**Layout under `<data>/bin`** (`layout.rs`): created 0700; refused if it's a symlink or owned by another user. Every file is written by temporary file, fsync, rename and a directory fsync; opened with `O_NOFOLLOW`. Overlay folder names are validated versions, never paths.

| File | Writer | When |
|---|---|---|
| `.lock` | supervisor (`rustix::fs::flock`, exclusive, for its life) | always; also taken by `lodestar rollback --update`. It closes M6-cli's "two `serve` instances" gap |
| `<version>/` = `lodestar`, `manifest.json`, `manifest.json.minisig`, optional `migration.json` | child (PR 3 stages it; PR 2's tests stage it directly) | the overlay is inert until `active` names it |
| `active` (`{version, previous}`, `previous` a version or `"image"`) | supervisor only | |
| `pending` (`{format, version, previous, kind: "install"|"restored", attempts, used_ms, index_before}`) | supervisor, only while no child runs; the child **deletes** it when its self-test passes | |
| `probation.json` (`{run_id, phase: "migrating"|"starting", at, from, to, baseline_max_id, stored_since}`) | child | heartbeat |
| `request.json` (`{run_id, action, version?, lost_torrents?}`) | child, just before a reserved exit | |
| `rollback.json`, `skipped` (reason `failed`), `halted.json` | supervisor | |
| `history.json`, `skipped` (reason `user`), `trust.json`, `updater.json` | child (PR 3) | |

The two writers never run at once: the supervisor writes only while no child runs. Record that rule in the decisions file.

1. **`lodestar serve` on Unix is the supervisor** (§12.2), always, even with updates Off, so there's one code path to test. `serve --fixture` is never supervised, and Windows never is.
   - In `main()`, after `privdrop::run` (still single-threaded) and the config load: take the lock (held → "another Lodestar is using this data folder", exit 3), finish any `rollback.json`, honour `halted.json`, choose what to run, then run the loop on a current-thread tokio runtime.
   - **The supervisor never opens the store,** never reads `trust.json`, never starts the logging file sink (console only, prefixed `supervisor:`), never touches the network. It costs a few MB.
   - **Init duties as PID 1:** forward `TERM`, `INT`, `HUP`, `QUIT`, `USR1`, `USR2` to the child; reap every child with `rustix::process::wait` (`waitpid(-1, WNOHANG)`) on each `SIGCHLD`. Spawn with `std::process::Command`, never `tokio::process`, and never call `Child::wait` (the reaper would steal its status). The child inherits stdin, stdout and stderr, so Docker's log shows both.
   - **What to run:** with no `active`, or one older than or equal to this build's version, run **its own code** as the child (`current_exe()` through B2c's `program()`, made `pub(crate)`; `(deleted)` → `/proc/self/exe`), and, when no `pending` or journal exists, remove `active` and the older overlays: a newer image wins. Otherwise verify the active overlay (step 3) and run it.
   - **The child's command line:** the supervisor's own arguments plus `--child`, and `LODESTAR_SUPERVISOR={"pid","run_id","version","notices"}` (≤ 4 KiB; `run_id` 16 random hex digits per start). Like B2c's hints it isn't a setting, so it stays out of `config::ENV_VARS`.
   - **Exit:** outside probation, when the child exits, the supervisor exits with its status (128 + signal when killed), so Docker's or systemd's restart policy decides (§12.2).
2. **The control channel** (§12.2 "must resolve"): **reserved exit codes plus a run-bound request file.** The workspace forbids `unsafe`, so passing an extra descriptor (`pre_exec`) is out.
   - `control.rs`: `EXIT_RESTART = 80`, `EXIT_SWITCH = 81`, `EXIT_ROLLBACK = 82` (clear of 0–3, clap's 2, panics' 101 and 128 + n).
   - The child writes `request.json` with its own `run_id`, then exits with the code after the graceful shutdown. A reserved code without a matching, fresh request is an ordinary exit (or a failed attempt in probation).
   - `serve --child` checks that `getppid()` is the `pid` in `LODESTAR_SUPERVISOR`. If not, it serves but reports updates as unable to install (`not_supervised`). It refuses while `rollback.json` exists, like every store opener.
3. **Verifying an overlay before each start** (`overlay.rs`): `lodestar_update::verify::overlay` over the folder's manifest, signature and the binary's SHA-256 and size (hashed from a file opened with `O_NOFOLLOW`), for this build's `LODESTAR_BUILD_TARGET`.
   - **No expiry check** at start, so an installed version keeps starting offline after it migrated the store.
   - **Revoked by this build's embedded statements** → refused; run its own code instead.
   - **Active overlay fails verification** (§12.2 "must resolve"): don't run it and don't delete it (rename it `<version>.bad` for diagnosis), run its own code, and pass a `bad_overlay` notice. If the store was migrated past what this build reads, `Store::open`'s `SchemaTooNew` refuses without harm, and the console error says to pull the current image.
   - Optional hardening, if cheap: run the inode you hashed (`/proc/self/fd/<n>` of the verified descriptor on Linux). Otherwise record the residual (write access to `<data>` already means full control, §12.2).
4. **Probation** (`pending.rs`):
   - **Switch:** on `EXIT_SWITCH` with a valid request, verify the named overlay, list `index/` into `index_before`, write `pending` (`kind: install`, `attempts: 0`), then `active`, then start it. **If `pending` can't be written,** don't switch: restart the current version with a `pending_unwritable` notice in `LODESTAR_SUPERVISOR` (no disk needed).
   - **Attempts:** charge one in `pending` (fsynced) before each start. Refund it only when the child exited after a stop the supervisor forwarded (recorded in memory, written after the child exits), or after a valid `EXIT_RESTART`. A hard kill in between leaves it charged; say so in the decisions file.
   - **Migration phase:** while `probation.json` says `migrating`, the child beats every 10 s. Fail if beats stop for 5 minutes, or if the phase passes `MIGRATION_CAP` (6 h, a new number: **(§2.19)**, recorded). The 5-minute probation timer starts at `starting`.
   - **Cumulative deadline:** `used_ms` adds up the time spent `starting` across starts; past **5 minutes** in total, stop the child (SIGTERM, **30 s**, SIGKILL) and count a failure.
   - **Failure:** the child exits on its own, or the deadline passes → restart, up to **3 attempts**, then roll back (step 6).
   - **Success:** the child deleted `pending`. Keep `active` and its previous overlay; delete older overlays.
5. **The child's side** (`probation.rs`, `--child` only):
   - **Before `Services::start`** (main.rs, so before the registry's first `Store::open`): read the schema version on a read-only connection (`cli::util::schema_version`); if it's below `lodestar_store::latest_schema_version()`, write `<version>/migration.json` `{from, to, started_at}`, start the heartbeat thread, and write `probation.json` `migrating`.
   - **The store's snapshot sequence** (scope exception in `schema.rs`, §12.2 "must resolve"): `VACUUM INTO <snapshot>.tmp`, fsync it, rename it to the `pre-v<N>[.<k>].db` name, fsync the directory, then migrate. A crash leaves only a `.tmp` (remove stale ones at the next migration), never a partial snapshot. `cli::util` and `snapshots::list` keep working unchanged.
   - **After `Services::start`:** `starting`, then the **self-test** (§12.2): the store opened with migrations complete (`Services::start` returned); the search index answers a query (a fixed one through `Services::probe()`'s `SearchHandle`); the HTTP port is bound, and the DHT port unless it's turned off; `/healthz` is green (`healthcheck()` against the bound address, retried until the deadline). Then delete `pending` and `probation.json`, fsync the directory. History is PR 3's.
   - **Lost torrents** (§12.2 "must resolve"): record `baseline_max_id` (`Store::max_id`) at `starting`, and `stored_since` every 60 s and at shutdown. A rollback reports that count, or "unknown" when none was recorded. The supervisor never opens the store to count.
   - **Previous index directories:** the new version leaves them alone until probation passes (B5b's cleanup already keeps other versions' directories; check, and say so).
6. **The rollback journal** (`journal.rs`, §12.2's steps; each step idempotent; the journal is updated and fsynced after each):
   1. Stop the child: SIGTERM, 30 s, SIGKILL, wait.
   2. Write `rollback.json`: `{format, failed, previous, reason, snapshot: {path, sha256} | null, remove_index: [...], lost_torrents, steps_done}`. The snapshot is chosen only when `<failed>/migration.json` exists **and** the newest `pre-v<from+1>` snapshot verifies (`snapshots::settle`, `quick_check`, schema = `from`); its SHA-256 is recorded before step 4.
   3. Append the outcome to `history.json` (keyed by the journal, so a repeat doesn't duplicate it).
   4. Restore the store, only with a chosen snapshot: delete `-wal` and `-shm` (`cli::util::side_files`), rename the snapshot over `lodestar.db`, fsync the directory. On a repeat, a missing snapshot whose recorded SHA-256 matches the store counts as done. A failed restore cleans up its partial state, keeps the journal, starts neither version, and prints what failed and where the snapshot is.
   5. Remove the `index/` entries not in `pending.index_before`.
   6. Repoint `active` at `previous` (remove it when `previous` is `"image"`); add the failed version to `skipped` with reason `failed`.
   7. Delete the failed overlay, then the journal.
   8. Start the previous version under `pending` with `kind: restored`.
   - **At every start the supervisor finishes a journal first.** A stop signal during a rollback finishes the current step only, leaves the journal, never starts the previous version, and exits non-zero. Docker's default 10 s stop timeout is shorter than the 30 s grace; the journal makes that safe.
   - **One level, once per failed version:** a `restored` version that fails its 3 attempts writes `halted.json` and exits 1 with a console error naming both versions and pointing to the recovery steps (docs follow-up). While `halted.json` exists, the supervisor prints the error and exits 1, unless this image is newer than both versions (a newer image wins). A version in `skipped` with reason `failed` is never installed again, even by Install now **(§2.19)**.
   - `lodestar rollback --update` (CLI, server stopped): takes the lock (held → exit 3), runs the same journal with reason `manual` and lost torrents "unknown", and starts nothing. Plain `lodestar rollback [SNAPSHOT]` is unchanged.
7. **Every store opener refuses while `rollback.json` exists:** add `refuse_during_update_rollback` to `serve --child` and `import-bitmagnet` in `main.rs`; the CLI already refuses (M6-cli).
8. **B2c's restart, composed** (`system/restart.rs`): under `--child`, `Restart::global()` reports `method: "supervised"` and `available: true` (a setter called from `main.rs` before `Services::start`; `available()` accepts `Supervised`). `serve()` keeps its race; after the graceful shutdown, a requested restart writes `request.json` and returns `EXIT_RESTART` instead of calling `reexec()`. The supervisor restarts the same version and charges no attempt. A signal still wins. `reexec()` stays for an unsupervised Unix run (`serve --fixture DIR`); the temporary-fixture refusal is unchanged. The address timer's revert restarts the same way.
9. **This build's version** (`build.rs`): `LODESTAR_BUILD_VERSION` from `LODESTAR_VERSION` when it's strict semver whose `major.minor.patch` is `CARGO_PKG_VERSION`'s (else a build warning), otherwise `CARGO_PKG_VERSION`. Nightlies (`0.5.3-nightly.<date>`) need it to compare. **Follow-up for E-M6/#92:** set `LODESTAR_VERSION` in `release.yml` and the Dockerfile. A `dev`-channel build never installs.
10. **Windows** (`cfg(not(unix))`): no supervisor, no `--child`. Name in the decisions file what E-M6's `lodestar update-helper` reuses: `lodestar-update`'s verification, the `pending`, `rollback.json` and `history.json` formats, `<data>\updates\` for the cached MSI, the service set to manual start during a rollback, M6-win's exit codes, Authenticode checked only when the artifact carries a signature, and the `Restart::global()` hook in `windows/service.rs` (still M6's).
11. **Debug-only test overrides** (`update/testing.rs`), one accessor that returns `None` unless `cfg(debug_assertions)`: a test trust root replacing the embedded keys, a pretend version, a short probation and cap, and a fault point (`journal:<k>`, `child:crash|hang|selftest`). Name each in the decisions file; `/security-review` checks they can't reach a release build.

### PR 3: the check, install, policy and API
1. **The `updates` settings section** (`settings/updates.rs`, per the settings README; all live; the engine subscribes):

   | Field | Values | Default | Env (locks it) |
   |---|---|---|---|
   | `mode` | `off`, `notify`, `auto`, `image` (`.widget("radio")`) | `off` | `LODESTAR_UPDATES` |
   | `level` | `fixes`, `features`, `all` | `fixes` | `LODESTAR_UPDATES_LEVEL` |
   | `security_fixes` | `auto`, `on`, `off` (`.auto(Auto::Str("auto"))`) | `auto` | `LODESTAR_UPDATES_SECURITY_FIXES` |
   | `channel` | `stable`, `beta`, `nightly` (advanced, `.danger(Danger::Disruptive)`) | `stable` | `LODESTAR_UPDATES_CHANNEL` |
   | `wait_days` | 0–14 (`.unit("days")`) | 3 | `LODESTAR_UPDATES_WAIT_DAYS` |
   | `window` | `HH:MM-HH:MM`, may wrap midnight (`.format("time-range")`) | `03:00-05:00` | `LODESTAR_UPDATES_WINDOW` |
   | `allow_reindex` | bool | true | `LODESTAR_UPDATES_ALLOW_REINDEX` |
   | `notify_before`, `notify_after`, `notify_failure` | bool | false, true, true | `LODESTAR_UPDATES_NOTIFY_*` |

   - `security_fixes: auto` means on under `auto` and off under `notify` (§12.2's mode-dependent default; no field may be nullable, so a sentinel). It never applies under `image` or `off`. `GET /updates` gives the effective value.
   - `check`: `image` only where `deployment_here()` is Docker or Unraid; a window whose start equals its end is refused; times are 00:00–23:59.
   - The same keys in `config.toml`'s `[updates]`, every variable in `ENV_VARS`.
   - **Notify me** is stored; delivery waits for the notification channels to be stored (B2c: `integrations` is null). Name the follow-up.
2. **The single signed check** (`check.rs`, §12.2 "Checking"):
   - **Where:** `https://github.com/<owner>/<repo>/releases/download/update-manifest/<channel>.json`, from `CARGO_PKG_REPOSITORY`, which must be a `https://github.com/<owner>/<repo>` URL, or updates report `no_release_source`. The rolling `update-manifest` release is a contract for E-M6 (decisions file). A 404 is "nothing published for this channel yet", not an error.
   - **How:** `lodestar_integrations::net::GuardedClient::new(NetPolicy::notification(false), …)`, public addresses only. Each hop goes through `send`, with redirects (301, 302, 303, 307, 308) followed by hand, at most 5, `https` only, to `github.com`, `objects.githubusercontent.com` and `release-assets.githubusercontent.com` only. `send_following` follows only 307/308, and GitHub's release downloads answer 302. Bodies through `read_limited` (512 KiB). Timeouts: 10 s to connect, 60 s overall.
   - **What it sends:** `User-Agent: Lodestar-Updater` set on each request, overriding the client's versioned `USER_AGENT`; no cookies, no conditional headers, no query string. Test the built request's headers.
   - **When:** with `mode` notify, auto or image, at most every 6 hours at a random minute (the last and next check in `updater.json`); `POST /updates/check` at any mode, `off` included, at most once a minute (`429 rate_limited`, `Retry-After`), one at a time. Egress settings (§5) don't exist yet; the check takes them when they land (name it).
   - **Then:** `lodestar_update::verify::check`, and `trust_file.rs` writes the next `trust.json` (merge, temporary file, fsync, rename, directory fsync). A missing `trust.json` is rebuilt from the floor, with high-water marks from the newest verified overlay. **A corrupt one blocks installs** (notice `trust_damaged`) and is never overwritten.
   - **Fixture mode:** no engine, no request (`fixture::active()`; `/updates` says `unavailable: fixture`).
3. **Policy** (`policy.rs`, a pure function of settings, manifest, trust, running version, deployment and now; table-tested):
   - **Candidates:** not withdrawn, newer than the running version, not in `skipped`. Automatic installs also need: no manual steps, a level within `level` (fix ≤ feature ≤ major), `allow_reindex` when the release reindexes, the wait passed since its `date`, not paused, and a current manifest.
   - **Always install security fixes:** a security release installs even when the level wouldn't, without the wait; under `notify` it's the one automatic install.
   - **Every automatic install waits for the window,** security fixes included (§2.13: "every update … waits for an install window"); the window is at most a day away. Install now skips the window and the wait.
   - **The upgrade path:** below a release's `min_from`, install the newest release whose `min_from` the running version meets, then go on.
   - **Running a withdrawn release:** offer Roll back; never roll back silently. **Running under a key revoked later:** the notice, and Install now skips the level and wait rules.
   - **Can this installation update itself** (each a reason code for the UI): supervised child (`not_supervised`), not `dev` (`dev_build`), not Windows (`windows_helper`: until E-M6), `<data>/bin` not `noexec` (`rustix::fs::statvfs`, `StatVfsMountFlags::NOEXEC`) and writable, not a package-managed install (`LODESTAR_DEPLOYMENT`), mode not `image`, trust not damaged, a key for the channel. Otherwise Install automatically behaves as Check and notify, with the reason.
   - **Before installing:** free space for the store snapshot (when the release migrates), the binary and `reindex_bytes`; no rebuild (`SearchHandle::progress`) or import running; otherwise defer to the next window.
4. **Download and stage** (`download.rs`, `install.rs`):
   - The artifact `url` passes the same host allowlist. Stream to `<data>/bin/<version>.part`, created new (`create_new`, `O_NOFOLLOW`, 0600). Count bytes (stop past the pinned size), hash as it goes, fsync, then compare SHA-256 and size. Remove stale `.part` files at start. One download at a time; no resume.
   - Stage `<version>.tmp/` (binary 0700, the manifest bytes and signature as fetched), fsync, rename to `<version>/`, fsync `<data>/bin`.
   - **Just before switching,** verify again against the current `trust.json` (§12.2 "At install time"), then set the switch intent and request the graceful shutdown through `Restart::global().request()`, so the switch takes B2c's path. `main.rs` turns the intent into `request.json` and `EXIT_SWITCH`.
   - **After probation passes,** the new version appends `installed` to `history.json`.
5. **History** (`history.json`, §12.2): the last **20** updates `{version, from, level, at, result: installed|rolled_back|failed|skipped, reason, lost_torrents | null}`. Clearing it is allowed (§12.2 "Clear history is there anyway").
6. **The window and time zone** (`window.rs`): local time is the host's (`TZ`, else `/etc/localtime`; distroless/static ships tzdata, so check it in the image). **Approved if you need it:** `jiff`, the latest 0.2.x at least two weeks old, pinned exactly, `default-features = false`, features `std` and `tz-system` (Unlicense OR MIT, which passes through MIT); record the stripped binary's size change. Or write a small TZif reader, with property tests. With no zone found, use UTC and say so in `/updates` (`window_zone`). DST gaps and repeats get tests.
7. **`/updates`** (session only through `auth::protected`, writes same-origin JSON; API keys never; `UpdatesApi` trait as an `Extension<Arc<dyn UpdatesApi>>`, as B2c did `SystemApi`; `503 updates_unavailable` without it):
   - `GET /updates`: status (state, checked_at, next_check_at, last error code), running (`version`, `channel`, `image_version` from `LODESTAR_SUPERVISOR`, `overlay`, `revoked_key`, `withdrawn`), capability (`can_install`, `reason`), effective policy (with `next_window_at`, `paused_until`, `window_zone`), available releases (badges, notes, `notes_url`, `eligible`, `blocked_by`, `installs_at`), the pending install, rollback (`available`, `to_version`, `lost_torrents` or null), notices (`revoked_key`, `clock_behind`, `clock_ahead`, `trust_damaged`, `pending_unwritable`, `bad_overlay`, `rolled_back`, `manifest_lapsed`, `channel_unavailable`), history.
   - `POST /updates/check`; `POST /updates/install {version, when: "now"|"window"}` (a manual-steps release needs `"confirm_manual": true`, else `422 manual_confirmation_required`, the B17 pattern; `409 install_unavailable` with the reason, `404 unknown_version`, `409 version_skipped`); `POST /updates/skip {version}`; `POST /updates/pause {days: 1|7|30|0}` (0 resumes); `POST /updates/rollback` (`202` then `EXIT_ROLLBACK` once the answer is out, like B2c's restart; `409 rollback_unavailable`); `POST /updates/history/clear` **(§2.19, new route)**.
   - **No route needs the fresh password:** §10.10.7 puts Roll back in the Danger zone (the dialog with the loss count), not the Security row; nothing installs that isn't signed.
   - Release notes are signed text but rendered by S-updates as plain text or sanitized Markdown; say so in the DTO doc.
8. **The `updates` SSE topic** (B4): `Topic::Updates` is the **7th of 8 bits** (the `u8` assert stays; the 9th topic widens it). `UpdatesEvent {state, version, bytes_done, bytes_total, error}` with `state` one of `checking`, `available`, `downloading`, `verifying`, `waiting_window`, `installing`, `restarting`, `checking_health`, `done`, `rolled_back`, `failed`. Publish on each change; progress at most once a second (the ring holds 512 events). The new version publishes `checking_health` while `pending` exists, then `done`; the restored one publishes `rolled_back` once, from history.
9. **`/system`:** the `Updates` connection row: on unless `mode` is off, with `last_checked_at`.

## Tests
- **PR 1:**
  - **Golden vectors** in `crates/update/tests/vectors/`, signed with fixed **test** keys (seeds in the test code, marked TEST ONLY; Ed25519 is deterministic), regenerated with `LODESTAR_BLESS=1`: good; bad signature; bad global signature; legacy `Ed`; wrong key number; another channel; Nightly signed by the release key; Stable signed by the nightly key; expired; expiry over 30 days; more than a day ahead; older than the mark; same timestamp with another body; replayed freshness; freshness for an older manifest; freshness by a revoked key; withdrawn; needs-manual; a skipped-version chain; a two-rotation chain; a gap; an equivocating statement; an introduction statement.
  - **At least one vector from the real minisign tool** (generate it, or take `minisign-verify`'s public test vector with attribution), and both real `.pub` files decode to their documented IDs.
  - **Key changes:** a recovery-signed statement is accepted and the old key's manifests are refused afterwards; a release-key-only statement is refused; a lower or equal `seq` is refused; a rotation resets the marks of the channels its role signs and only those.
  - **The floor:** with `trust.json` gone, embedded statements and then verified overlays still refuse a revoked key; forward-only merge never lowers (sequence, mark); an older supervisor verifies an overlay signed by a newer key from the overlay's statements alone.
  - **Property tests:** arbitrary bytes into every parser never panic; mutating any byte of a signed body or signature fails verification; serialise → parse round-trips the trust state; merges are monotonic.
  - **`cargo fuzz` target `update`**: the envelope, manifest, statement, freshness, minisign and trust-state parsers, plus `verify::check` against a fixed test trust. No panics; a document that verifies re-verifies identically.
- **PR 2** (pure state-machine tests with a fake spawner and clock, plus `crates/lodestar/tests/supervisor.rs` on the real binary with the debug overrides):
  - the supervisor: a binary that crashes on start, one that hangs without confirming, and one that fails its self-test each end with the snapshot and previous version restored, the overlay deleted and the version skipped;
  - a forwarded stop during probation uses no attempt; an `EXIT_RESTART` uses none; a migration longer than 5 minutes that keeps beating passes; one that stops beating fails; the cap fails it; the cumulative deadline holds across restarts;
  - **crash at every journal step** (the fault point, then a restart, for each k): the rollback finishes, the store is intact, the WAL is never applied to the restored file, history has one entry; a stop signal mid-rollback leaves the journal and starts nothing;
  - the previous version also failing writes `halted.json` and stops, instead of looping; a newer image then wins;
  - a full disk prevents `pending`, so the new version doesn't start and the current one runs with the notice;
  - an installed overlay starts with no network after its manifest has expired; one revoked by embedded statements is refused; a tampered binary falls back to its own code with `bad_overlay`;
  - a newer image runs its own code and removes older overlays; an equal one does too;
  - while `rollback.json` exists, `serve`, `serve --child`, `import-bitmagnet` and every CLI subcommand refuse (extend `tests/cli.rs`);
  - a second supervisor on the same data folder exits 3;
  - signals reach the child, orphans are reaped (a grandchild left behind is reaped), and the exit status passes through (0, 1, 128 + n);
  - **B2c's tests still pass unchanged:** `system_reports_and_restarts_in_place` (same supervisor PID, `restart.method` now `supervised`), `serves_status_and_stops_cleanly`;
  - the snapshot sequence: a crash mid-`VACUUM INTO` leaves no `pre-v` file; existing `schema.rs` snapshot tests pass;
  - `lodestar rollback --update` runs the journal offline and refuses while the server holds the lock.
- **PR 3:**
  - a fake release source (a `Fetch` trait with an in-memory server for unit tests; for the binary test, the debug-only base URL with the download-client policy) serving: good, bad signature, wrong hash, wrong size, oversized body, downgrade, replay, expired, withdrawn, needs-manual, a skipped-version chain, a redirect to a host off the allowlist, an `http` hop, six redirects;
  - trust survives a rollback: after a rotation shipped in a release that fails probation, the restored store doesn't bring back the revoked key, and the marks stay;
  - a corrupt `trust.json` blocks installs and isn't overwritten; a deleted one is rebuilt from the floor;
  - the policy table: each mode × level × security × wait × window × pause × manual × reindex; a `noexec` volume and the other reasons fall back to notify; an install window that holds while a rebuild runs; Install now skips window and wait but never a failed version;
  - the window across midnight and DST changes, and UTC when no zone is found;
  - the check sends exactly `User-Agent: Lodestar-Updater` and no identifier; fixture mode makes no request;
  - `/updates`: goldens for each route (volatile fields masked), 401 without a session, API keys refused, 403 cross-origin, 415 not JSON, the `429` on check, the manual-steps `422`;
  - the settings section: schema hints, `null` resets, environment locks, `image` refused off Docker, the golden;
  - SSE: one `updates` event per change, throttled progress, `?topics=updates`; `Topic::ALL` has 7.
  - A log-capture test over a check and an install finds no URL with a query string, no key material and no signature text.

## Done when
- For each PR, after merging `origin/main`: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass; CI's `test` job stays green on every OS in its matrix, Windows included (supervisor code is `cfg(unix)`), and so does the root-only `privdrop` step.
- `cargo +nightly fuzz run update -- -max_total_time=60` runs clean (PR 1; CI's `fuzz` job runs it after).
- PRs 2 and 3: `cargo run -p lodestar --bin openapi` + `cd web && npm run api:types` leave no diff; in `web/`, `npm ci`, `npm run check` and `npx vitest run` pass. If an exhaustive switch over `Topic` in `web/src` breaks, name it for its owner rather than editing it.
- Whenever `Cargo.lock` changes: `python3 .github/scripts/licenses.py --check` passes (regenerate `rust.json`). Once #119 has merged: `python3 .github/scripts/log_lint.py` passes.
- `/security-review` has been run on each PR's branch; its findings and fixes are in that PR.
- **The PRs** are titled `B-update: Update engine, signed manifest, key chain, overlay supervisor (n/3: …)`, and the last one closes the slice. Their descriptions list:
  - **PR 1:** the formats (envelope, manifest, statements, freshness, trust state) with every cap, the minisign rules, the time and mark rules, the embedded keys and what happens until the nightly and freshness keys exist, and the contract for E-M6's `release.yml` and the weekly re-sign;
  - **PR 2:** the `<data>/bin` files with their writer, the exit codes and request file, probation accounting with its numbers, the journal steps and recovery cases, how restart composes with B2c, every hook line in a shared file, the store scope exception, the debug overrides, the Windows contract for E-M6, and the follow-ups (`LODESTAR_VERSION` in `release.yml` and the Dockerfile; `KillMode=mixed` for the systemd unit; `packaging/docker/check-ids.sh` and `tests/privdrop.rs` checking the child too);
  - **PR 3:** every endpoint and field, the settings and variables, the policy rules, the download allowlist, the `update-manifest` release contract, the SSE payload, the `/system` row, and the follow-ups (Unraid's `LODESTAR_UPDATES=image` template option, the Notify me delivery, egress settings);
  - **each:** the user-facing steps for the docs follow-up (turning updates on, what each mode does, Container image updates on Unraid, reading a rollback notice, "Update trust data is damaged", the clock notices, recovering from `halted.json`), and the **(§2.19)** refinements: only prehashed signatures, per-channel freshness marks, the same-timestamp rule, the reserved exit codes, the 6-hour migration cap, a failed version never reinstalled, security fixes waiting for the window, the `security_fixes` sentinel, the history-clear route, the manual-steps confirmation, and `lodestar rollback --update`.
