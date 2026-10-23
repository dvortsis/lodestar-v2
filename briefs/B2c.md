# B2c: /system, restart, HTTP-address keep-or-revert timer, licences.json
Covers `/system` (build and install info for About), restart, the HTTP-address keep-or-revert timer, and `licenses.json` generated per shipped target with overrides and a CI allowlist. There is no update check here: B-update owns it.

**Model:** Opus. This is core work:
- it replaces the running process;
- it guards against locking the user out of their own UI;
- it adds a supply-chain gate to CI.

**Branch:** `slice/B2c`. **`/security-review` is required** before merge, because `POST /system/restart`, the re-exec and the address revert are lockout and availability paths. **Follow `.claude/skills/steward/SKILL.md` from your first push until the PR merges.**

**Spec:**
- `runner/standards.md`.
- `docs/owner-decisions.md`:
  - §2.12: AGPL-3.0-or-later, and the dependency allowlist, which the settings session detailed, not the owner;
  - §2.13 and §2.23, only to stay out of them: About's update status is B-update's signed check;
  - §2.14 (the user-facing steps go to the docs follow-up);
  - §2.19 and §2.22.
- Architecture:
  - **§10.10.6:** the restart banner, Restart now, the restart flow, lockout protection and recovery without the UI;
  - **§10.10.9:** the `GET /system` and `POST /system/restart` bullets, and the server-side test "the HTTP-address timer reverts when nobody confirms";
  - **§10.10.13:** everything About shows comes from one `GET /system`, and Licences and credits is the contract for `licenses.json`;
  - §10.10.7 (Disruptive), §10.10.8 (Overview, General and About read `/system`), §11 (deployments, PID 1, `fsdetect`).
- `docs/architecture.md` §16, the settings-session entries:
  - "About screen", with its slice line "B2c owns `/system` … and the licence generation";
  - "One update check. About uses the signed manifest; B2c no longer calls GitHub's releases API";
  - "Licences are generated per shipped target, with a reviewed overrides file…".
- Open PRs whose decisions you build on:
  - **B12** (#112, `origin/slice/B12`, about to merge): `docs/decisions/2026-10-23-B12.md`, the "Recovery" bullet (`LODESTAR_PROXY_AUTH=off`, `forced_off`) and "What B2c's keep-or-revert timer needs";
  - **M6-win** (#98, `origin/slice/M6-win`): `docs/decisions/2026-10-22-M6-win.md`, "Exit codes" (a service-specific code 1 on an error stop, so the MSI's recovery actions apply) and the MSI notes (`FailureActionsOnNonCrashFailures`);
  - **B16** (#114, `origin/slice/B16`): `docs/decisions/2026-10-23-B16.md` (fixture mode, `fixture::OFFLINE`).
- **The code:**
  - `crates/api/src/settings.rs`: `SettingsApi::{index, section, save, patch}`, `Saved.changed`, the private `announce` and `save_and_announce` (line ~333), and how `patch`/`patch_many` take `hub: Option<Extension<Arc<EventHub>>>`;
  - `crates/api/src/ops.rs`: `save()` (line ~213), `patch_crawler`, `set_paused`, `pause`, `resume`, and the `ops_crawler_*` goldens;
  - `crates/api/src/events.rs` (`EventHub::settings_saved`), `crates/api/src/auth/http.rs` (`protected`), `crates/api/src/extract.rs` (`same_origin_json`);
  - `crates/lodestar/src/main.rs`: `serve()`, `shutdown_signal()`, the `Command::Serve` arm and `healthcheck`;
  - `crates/lodestar/src/services.rs`:
    - `Services::start`: the registry load, the `TcpListener::bind(config.http.bind)` line, and `ApiState`;
    - `Services::run`: `.layer(Extension(events.clone()))` and the graceful shutdown;
  - `crates/lodestar/src/settings/network.rs` (`Network.http_bind`, `effective_bind`), `crates/lodestar/src/settings/README.md` (layers, `null` resets, `restart_required`, keys outside `settings.` are not the registry's);
  - `crates/lodestar/src/cli/util.rs::schema_version`, `lodestar_search::rebuild::IndexVersion::CURRENT` (`v{schema}-t{tokenizer}`), `SearchHandle::progress`;
  - `crates/lodestar/src/ops.rs` (`dir_size`), `crates/lodestar/src/fsdetect.rs`, `crates/lodestar/src/privdrop.rs` (a no-op when not started as root), `crates/lodestar/src/web.rs` (the embedded `_app/version.json`), `crates/lodestar/build.rs`;
  - `packaging/docker/Dockerfile` (the web stage copies only `web/` and `brand/source`), and `.github/workflows/ci.yml` and `.github/scripts/ci_changes.py` (the `openapi` drift job, `RUST_ALSO`, `ci-ok`).

## Own
- `crates/lodestar/src/system.rs`, and `crates/lodestar/src/system/**` if you split it (`info`, `restart`, `address`).
- `crates/api/src/system.rs` and `crates/api/src/dto/system.rs` (new). Add one registration line each in `crates/api/src/{lib.rs,openapi.rs,dto/mod.rs}`.
- `crates/api/tests/system.rs` (new) with goldens, `tests/openapi.rs` `ROUTES`, the 401 list in `tests/api.rs`, and the regenerated `openapi.json` + `schema.d.ts`.
- **Carry-forward (a):** `save()` and the three crawler handlers in `crates/api/src/ops.rs`; `save_and_announce` becomes `pub(crate)` in `crates/api/src/settings.rs` (nothing else there).
- **Hook lines, each named in the PR:**
  - `crates/lodestar/src/main.rs`: `mod system;` placed between `mod subscriptions;` and `mod web;`, so it isn't next to #114's `mod fixture;` or #98's `mod windows;`. Also the restart lines in `serve()` (step 3). `main.rs` is B-update's later.
  - `crates/lodestar/src/services.rs`:
    - building `system::System`;
    - one `.layer(Extension(..))` beside the hub's in `run`;
    - the bind line, replaced by `system::address::bind(..)` (step 4);
    - starting the address follower.
  - `crates/lodestar/build.rs`: the build-info `cargo:rustc-env` lines.
  - `pub(crate)` on `dir_size` in `crates/lodestar/src/ops.rs`.
  - If B16 (#114) has merged when you start: a `pub fn active() -> bool` beside `OFFLINE` in `crates/lodestar/src/fixture.rs`.
- **Licences:**
  - `web/licenses/**` (new): the generated `rust.json`, the reviewed `overrides.json` and `allow.json`;
  - `web/scripts/licenses.ts` + `licenses.test.ts` (the Vite plugin), and its one line in `web/vite.config.ts`;
  - `.github/scripts/licenses.py` + a test;
  - one step in `ci.yml`'s `openapi` job;
  - one pattern in `ci_changes.py` `RUST_ALSO`;
  - optional `ARG`/`ENV` pass-through lines for `LODESTAR_COMMIT` and `LODESTAR_CHANNEL` in `packaging/docker/Dockerfile`.
- `docs/decisions/<date>-B2c.md`.
- **Don't edit:**
  - `crates/lodestar/src/settings/**`: B-log1 runs beside you and owns `settings/{logging,log}.rs`, and edits `general.rs` and `mod.rs`;
  - `crates/lodestar/src/logging/**` (B-log1);
  - `crates/lodestar/src/windows/**` (#98 M6-win), `crates/lodestar/src/fixture.rs` beyond the one accessor (#114 B16);
  - `crates/api/src/auth/**` (#112 B12), `.github/workflows/release.yml` (#92 P-release), `crates/lodestar/src/config.rs` (#48);
  - `docs/user/**` (#91) and `docs/operations.md` (#48, #91). Put the user-facing steps in the PR for the docs follow-up.
- **Nearby open PRs:**
  - #114 B16 (`main.rs`, `services.rs`, `cli/mod.rs`, `fixture.rs`, `crates/lodestar/Cargo.toml`);
  - #98 M6-win (`main.rs`, `windows/**`);
  - #112 B12 and #111 B10a: `services.rs`, `api/{lib.rs,openapi.rs,dto/mod.rs}`, and every `ApiState { .. }` literal in `crates/api/tests/*.rs`, `crates/lodestar/src/{ops/tests.rs,settings/api_tests.rs}`;
  - #115 B10b and #102 B7 (`lib.rs`, `openapi.rs`, `dto/mod.rs`, `tests/openapi.rs`, `tests/api.rs`);
  - #48 M4 (`services.rs`, `config.rs`, `http.rs`, `metrics.rs`);
  - B11a, running without a PR yet (`services.rs` wiring, the `api` registration lines).

  Keep hook lines additive and away from theirs, and merge `main` as each lands.

## Build
1. **`GET /system`** (session only, `auth::protected`; one request feeds About, the Overview and General).
   - **Wiring:** reach the binary through a `SystemApi` trait in `crates/api/src/system.rs`, passed as an `Extension<Arc<dyn SystemApi>>` layered in `Services::run`, as the hub is. Don't add an `ApiState` field: that touches every test literal four open PRs are changing.
   - **Fields, shaped for §10.10.13:**
     - `version` and `channel` (`stable`, `beta`, `nightly` or `dev`);
     - `build`: `commit`, `date`, `target`, `rustc` and `ui_build_id`;
     - `repository` (`CARGO_PKG_REPOSITORY`, so a fork links to itself);
     - `data_formats`: `store_schema` (`cli::util::schema_version` on a read-only connection), `index` (`IndexVersion::CURRENT.dir_name()`) and `rebuild` (progress, or null);
     - `deployment`: `kind`, plus `image_tag` and `image_digest` when the image sets them;
     - `host`: `os`, `arch`, `cpus`, `memory_bytes` and `time_zone`;
     - `started_at` and `uptime_s`;
     - `data`: `dir`, the sizes of `database`, `index`, `cold` and `logs`, `free_bytes`, and `filesystem_warning` (`fsdetect`);
     - `connections[]`: what Lodestar connects to, each with its state and the settings section it links to: the DHT, sources, blocklist subscriptions and integrations. Updates, companions and workers are null until B-update and v0.6;
     - `restart` (step 3), `restart_required` (`section.field` names, from each section's `SectionValues.restart_required`) and `address_change` (step 4);
     - `diagnostics`: the names of changed settings and their sources, never values. `logging` and `recent_errors` are null until B-log1 provides them;
     - `fixture` (bool), cheap only if B16 has merged. Otherwise leave it out and name the follow-up.
   - **Null, never 0,** for anything not measured on this platform. Memory and free space on Windows stay null until M6. Use `rustix::fs::statvfs` on Unix, which is already a dependency.
   - **Never a secret, URL credential or torrent detail:** connections give counts and states, not URLs.
   - **Directory sizes** run on the blocking pool and are cached for 60 s, so About's single request stays fast.
   - **Build info:** `build.rs` emits `TARGET` and `rustc -V`; the commit from `LODESTAR_COMMIT`, else git, else null; the date from `SOURCE_DATE_EPOCH` or the commit date, never "now", so builds stay reproducible; the channel from `LODESTAR_CHANNEL`, default `dev`.
     - `build.rs` must not rerun on every build.
     - `ui_build_id` comes from the embedded `_app/version.json` (`web.rs`).
     - Name the follow-ups that set `LODESTAR_COMMIT` and `LODESTAR_CHANNEL` in `release.yml` (P-release #92) and, if you don't add them, in the Dockerfile.
   - **Deployment:** a `LODESTAR_DEPLOYMENT` hint wins (packaging sets it later; name the follow-ups for Unraid, the MSI and portable). Otherwise detect: Docker by `/.dockerenv` or the cgroup, systemd by `INVOCATION_ID`, else `tarball`/`unknown`. Like B12's `LODESTAR_PROXY_AUTH`, it isn't a setting, so it stays out of `config::ENV_VARS`.
2. **Carry-forward (a): crawler saves publish `settings`.** `ops.rs` `save()` calls `SettingsApi::patch` directly, so `POST /crawler/pause`, `/crawler/resume` and `PATCH /crawler` save without the `settings` SSE event.
   - Route them through `crate::settings::save_and_announce` with a one-section `SettingsBatch`.
   - Take `hub: Option<Extension<Arc<EventHub>>>` in the three handlers.
   - Map `SettingsFailure` to the same `ApiError` as today, so every `ops_crawler_*` golden stays byte-identical.
   - A no-op pause (already paused) publishes nothing, because `Saved.changed` is empty.
3. **Restart** (`POST /system/restart`: session, same-origin JSON).
   - **Answer first:** reply `202 {"restarting": true, "estimated_seconds": N}`, then restart once the response is flushed.
     - N is this run's measured start time, from process start to listening.
     - When the server can't restart itself, answer `409 restart_unavailable` with the hint.
   - **`restart` in `/system`:** `{available, method, hint, estimated_seconds}`.
   - **Mechanism:** `system::restart` holds a request flag and a `requested()` future.
     - `main.rs` `serve()` runs `Services::run` with `shutdown_signal()` or `requested()`, whichever comes first, so a restart takes the same graceful shutdown (queue, DHT state, final commit).
     - After that, if a restart was requested, flush the logs (B-log1's file sink, if it has merged) and re-exec.
   - **Linux, macOS, Docker, Unraid and systemd: `method: "reexec"`.**
     - Re-exec with `std::os::unix::process::CommandExt::exec` on `current_exe()` with the same arguments. That's safe std, with no `unsafe`.
     - The PID is kept, so the container's PID 1 survives, and descriptors close on exec (std and tokio set `CLOEXEC`).
     - Started as root, the process already dropped PUID/PGID, and the drop is a no-op for a non-root start (`privdrop`).
     - Fixture mode re-execs with its own arguments.
     - If `exec` fails, log it and exit non-zero, so the container's restart policy takes over.
   - **Windows: `available: false`, `method: "manual"`** until M6 wires the hook. Name it in the PR and the decisions file:
     - **As a service:** M6-win's `windows/service.rs` `serve()` races `system::restart::requested()` with its stop channel, then stops with a service-specific exit code (1, or a dedicated code M6 picks) so the SCM recovery action the MSI configures (`FailureActionsOnNonCrashFailures`, #92/#98) starts it again.
     - **Portable:** after the graceful shutdown, start a new copy with the same arguments, then exit. The new copy retries the data-directory lock for a few seconds.
     - B-update's supervisor (§12.2, which owns `main.rs` next) may replace re-exec with a supervised relaunch; keep `requested()` and the method enum so it can.
4. **The HTTP-address keep-or-revert timer** (§10.10.6, §10.10.9).
   - **Record the change:**
     - A follower on `registry.subscribe::<Network>()` remembers the running `http_bind` and its source. When a save changes it, it writes `{previous: saved value, or "reset" if none was saved, new, state: "awaiting_restart"}` under a `system.` key in the store's settings table.
     - That key isn't under `settings.`, so the registry never sees it, and no migration is needed.
     - A second change while one is pending keeps the original `previous`.
     - An environment-set bind (`LODESTAR_HTTP_BIND`) is read-only, so it never arms the timer.
   - **Start the window:** at start, `system::address::bind` replaces the bind line. If a change is pending and the effective bind is `new`, the state becomes `awaiting_confirmation`, with a deadline 5 minutes after the listener binds. The deadline is persisted, so a crash doesn't reset it.
   - **Confirm:** `POST /system/address/keep` (session, same-origin) clears the record.
     - It counts only when this process is serving on the new bind, whatever the `Host` header says: reverse proxies rewrite it, and B12's guidance binds to `127.0.0.1` behind a proxy.
     - A cookie session or, with B12 merged, a proxy-header session counts.
     - `POST /system/address/revert` lets the user go back at once.
   - **Revert:** when the deadline passes unconfirmed, save `previous` back through the registry (`null` if it was "reset"), log it, and restart (step 3).
     - If the server can't restart itself, the saved value is restored and applies at the next restart; `/system` says so.
     - **A deadline already past at start reverts before binding,** and **a pending address that fails to bind reverts at once** and binds the previous one. A typo'd address mustn't crash-loop the container.
   - **`address_change` in `/system`:** `{state, url, previous_url, deadline}`, with URLs built like `ops::ui_address`.
   - **Generic over what it reverts.** Build it as a `KeepOrRevert` record with a `kind`. Wire only `http_bind` now.
   - **B12's proxy auth (carry-forward b):** today a proxy change can't lock anyone out of a cookie session (#112's decisions), so don't wire it. Document the hook for when a proxy-only mode lands (M-security-settings or a B12 follow-up):
     - take `Auth::proxy()` before `PUT /auth/proxy` applies;
     - on expiry, call `Auth::set_proxy(previous)` (no password: the user already confirmed);
     - `LODESTAR_PROXY_AUTH=off` stays the recovery that needs no UI.
5. **`licenses.json`** (§10.10.13, owner-decisions §2.12). Nothing exists yet: no `cargo-about`, `cargo-deny` or script, in Cargo or in `web/`. Build it with no new dependency, from Python's standard library and Node built-ins.
   - **Rust** (`.github/scripts/licenses.py`):
     - For each shipped target — `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` and `x86_64-pc-windows-msvc`, as #92's `release.yml` builds them, kept in one list with a comment to keep them in sync — run `cargo metadata --locked --format-version 1 --filter-platform <target> --no-default-features --features web`. That's the release feature set: B16's default-on `fixture` feature pulls `bench` and isn't shipped.
     - Walk `resolve` from the `lodestar` package over normal dependency edges only (no dev or build). Proc-macros count as shipped, because they generate shipped code; record that.
     - Write `web/licenses/rust.json`: each crate's name, version, SPDX expression, repository and the targets it ships on, plus its licence texts, read from the package folder and deduplicated by SHA-256.
     - `--check` regenerates the file and fails on drift or on the allowlist.
     - It must live under `web/`, because the Docker web stage copies only `web/`.
   - **npm** (`web/scripts/licenses.ts`, a Vite plugin):
     - At `vite build`, take the packages actually bundled from the Rollup module graph (JS and CSS, lazy chunks included). Read their licences from `package-lock.json` and the texts from each package's files.
     - Merge `rust.json` and `overrides.json`.
     - Emit a content-hashed `licenses-<hash>.json`, and export its URL from a virtual module for S-about to fetch lazily. Dev serves the same JSON.
     - The site's first-load size must not change.
   - **`overrides.json`** (reviewed):
     - `missing`: packages with no licence field, recording the licence file they ship (`svelte-toolbelt`, through bits-ui, is MIT);
     - `extra`: shipped code and assets that aren't in the module graph — the Atkinson Hyperlegible Next and Mono fonts (OFL-1.1, files from `@fontsource-variable/*` via `scripts/fonts.mjs`), Phosphor icons (MIT, compiled in by `unplugin-icons` from `@iconify-json/ph`), daisyUI's generated CSS, and the generated Paraglide runtime;
     - the brand artwork notice (`brand/LICENSE.md`).
     - If cheap, add the theme palettes' credits from `web/themes/*.json` `license`, as credits outside the allowlist check.
   - **`allow.json`, from §2.12:**
     - MIT, MIT-0, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, Zlib, 0BSD, BSL-1.0, BlueOak-1.0.0, Unicode-3.0, MPL-2.0, CC0-1.0, OFL-1.1;
     - GPL-3.0-only, GPL-3.0-or-later, AGPL-3.0-only and AGPL-3.0-or-later;
     - refused, named for the error: GPL-2.0-only, SSPL-1.0, and non-commercial or proprietary licences.

     Evaluate SPDX expressions properly: `OR` passes if any branch passes, `AND` needs all, and parentheses count. Read the old `A/B` form as `OR`, and the deprecated `GPL-3.0` as `GPL-3.0-only`. A `WITH` exception passes when its base does. A shipped component with no licence and no override fails.
   - **Unclear points; don't widen the list silently:**
     - These aren't on §2.12's list but are permissive and AGPL-compatible: `Unicode-DFS-2016`, `Unlicense` on its own, `CDLA-Permissive-2.0` (`webpki-root-certs` or `webpki-roots`, if `reqwest`'s rustls stack pulls them in on a shipped target) and `LLVM-exception`. You may add one only as a **(§2.19)** entry in your decisions file for the runner to flag.
     - Stop and message the runner for anything copyleft or unusual: LGPL-*, EPL, CDDL, or no licence and no evident file. §2.12's "GPL-3.0/AGPL-3.0 family" doesn't say whether LGPL-3.0 counts, so flag it if you meet it.
     - The Okabe–Ito theme's "free to use with credit" isn't SPDX; it's a credit, not a dependency.
   - **CI:**
     - `ci.yml`'s `openapi` job (Rust-gated, inside `ci-ok`) runs `licenses.py --check`;
     - `ci_changes.py` `RUST_ALSO` gains `^web/licenses/`, so editing the inventory runs it;
     - the npm side fails `npm run build`, which `web.yml` and the Docker build run.
   - Record `rust.json`'s size and the crate count in the PR.

## Tests
- **`/system`:**
  - a golden, with the volatile fields masked;
  - 401 without a session; keys are refused when B12 has merged;
  - a walk of the JSON finds no seeded password, cookie, setup token or key;
  - unmeasured values are null;
  - the sizes are cached.
- **Crawler SSE:** pause, resume and intensity each publish one `settings` event `{section: "crawler", revision}`; a no-op pause publishes none; the `ops_crawler_*` goldens are unchanged. Use a new test file: `tests/ops.rs` is in #112 and #111.
- **Restart:**
  - 202, then the request future fires after the response;
  - 409 `restart_unavailable` where the method is manual;
  - 403 cross-origin, 415 not JSON.
  - A Unix integration test (beside `crates/lodestar/tests/serve.rs`): sign in, `POST /system/restart`, `/healthz` drops and returns under the same PID, the data is intact, and `restart_required` is empty afterwards.
- **Timer** (an injected clock; keep the 5-minute constant, and if the binary test needs a short window, use an override only debug builds read, and say so):
  - recorded on save; armed only once serving on the new bind; keep clears it; revert restores the saved value or resets it;
  - the deadline survives a restart; a deadline already past at start reverts before binding; a bind failure on the new address reverts at once;
  - an environment-set bind never arms it; a second change keeps the first `previous`;
  - §10.10.9's "reverts when nobody confirms".
- **Licences:**
  - the SPDX evaluator: `OR`, `AND`, `WITH`, parentheses, the slash form and deprecated ids; GPL-2.0-only refused;
  - no licence fails unless overridden;
  - `--filter-platform` drops a platform-only crate (a recorded `cargo metadata` fixture);
  - the Vite plugin over a fake module graph lists only bundled packages and keeps dev-only tools out;
  - the drift check is green on the branch.

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass after merging `origin/main`.
- `cargo run -p lodestar --bin openapi` + `cd web && npm run api:types` leave no diff, and `python3 .github/scripts/licenses.py --check` passes.
- In `web/`: `npm ci`, `npm run check`, `npx vitest run`, `npm run build` (with `licenses-<hash>.json` emitted) and `npx size-limit` pass.
- `/security-review` has been run on the branch; its findings and fixes are in the PR.
- **The PR** is titled `B2c: /system (build and install info for About), restart, HTTP-address keep-or-revert timer, licences.json generation per shipped target with overrides and a CI allowlist (no update check: B-update owns it)`. Its description lists:
  - the endpoints and every `/system` field, with its source and null rule;
  - the restart method per platform, and the Windows service and portable hooks M6 must wire;
  - the timer's rules, persistence and recovery cases, and how it interacts with B12's proxy auth and `LODESTAR_PROXY_AUTH`;
  - the crawler SSE fix;
  - the build-info sources and the `release.yml`/Dockerfile follow-ups;
  - the licence pipeline: targets, files, overrides, the allowlist, any licence met outside it, and `rust.json`'s size;
  - every hook line in a shared file;
  - the user-facing steps for the docs follow-up: "I can't reach the UI", changing the port under Docker also needs the port mapping, and Restart now versus restarting the container;
  - the §2.19 refinements, such as allowlist additions, proc-macros counted as shipped, revert on bind failure, and the timer where the server can't restart itself.
