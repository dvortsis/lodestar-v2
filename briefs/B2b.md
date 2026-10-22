# B2b: Settings API for the screen
Covers schema hints, revisions, multi-section PATCH, preview, test, preferences, the `settings` SSE topic, and export/import.

**Model:** Opus (core: settings persistence, validation, concurrency, secrets). **Branch:** `slice/B2b`.

**Spec:**
- `runner/standards.md`.
- `docs/architecture.md` §10.10, the Settings design. Read all of it, especially:
  - §10.10.4 The field;
  - §10.10.5 Saving, validation and conflicts;
  - §10.10.6 Restart, recovery and portability;
  - §10.10.9 API additions.
- §12 (auth; same-origin writes) and §20 (filter state never travels in settings export).
- §16: B2's entry (the registry, its layering, env read-only, `watch` per section), B4's (the SSE hub, the topic bitset), B17's (the `content` section and its `confirm` hook), and the settings-session entries.
- `docs/owner-decisions.md` §2.11–2.14.
- The code:
  - `crates/lodestar/src/settings/{README.md,mod.rs,*.rs}`;
  - `crates/api/src/settings.rs`;
  - `crates/api/src/events.rs` and `crates/api/src/dto/events.rs`;
  - `crates/api/src/auth/*`.

## Own
- `crates/lodestar/src/settings/**`. B14 (#79) adds `crawler.paused` in `settings/crawler.rs`; merge `main` if it lands first.
- `crates/api/src/settings.rs`, `crates/api/src/dto/settings.rs`.
- **The `settings` topic** in `crates/api/src/events.rs` and `dto/events.rs`: only the variant and its publish hook. B4b is changing the `torrents` count there now; merge `main` after it lands.
- `crates/api/tests/settings*.rs`, the regenerated `openapi.json` + `schema.d.ts`, and a §16 entry.
- **Out of scope:** `/system`, restart and the HTTP-address keep-or-revert timer belong to B2c. The `logging` section belongs to B-log1. Updates belong to B-update.

## Build (§10.10.9 is the contract; this is the order)
1. **Schema hints** on each field: `x-group`, `x-order`, `x-advanced`, `x-unit`, `x-widget`, `x-auto`, `x-preview`, `x-keywords` (message keys), `x-danger`, the `config.toml` key and the `LODESTAR_*` name, and `writeOnly` for secrets.
   - Every existing section gets correct hints.
   - The server still validates everything.
2. **Revisions.**
   - A `revision` per section in `GET`, stored as a `settings.` key with no migration.
   - `PATCH` accepts `if_revision` and answers `409 stale_revision` with the current values.
3. **Multi-section `PATCH /settings`** (`{section: {field: value}}`).
   - One transaction and one lock, then each changed section's `watch` channel publishes.
   - Errors carry `section` and `field`.
   - A multi-section patch with one bad field saves nothing.
   - B17's `content` confirm hook (`confirm_adult`) must still apply in the batch form.
4. **`POST /settings/preview`** runs exactly the same validation as `PATCH` without saving. Per field, it returns:
   - live or restart;
   - warnings;
   - estimates, where the section can compute them.

   Add a **property test** that preview accepts exactly what `PATCH` accepts.
5. **`POST /settings/test`** runs connection tests for integration drafts. Use `crates/integrations`' existing test or ping functions, with the SSRF guard.
6. **The `preferences` section**: theme, mode, density, motion, illustrations, shortcuts, language, time zone.
   - Check §10.10.9 and §12 for whether it is per account. Lodestar has one admin account today: store one set, keyed so a later multi-account change is a migration of the key only. Say so in §16.
   - **Show adult content isn't here;** it stays `content.show_adult`.
7. **The `settings` SSE topic** (`{section, revision}`) after every save. It carries no values.
   - Respect the hub's `u8` topic bitset; 7 topics after this and `updates`.
8. **`GET /settings/export` and `POST /settings/import`** (`dry_run=true` returns the diff).
   - Secrets are never exported.
   - §20 filter state is never exported.
   - Env-locked fields are reported and not applied on import.
   - Import validates through the same path as `PATCH` (all or nothing).
   - Export and import round-trip.

**Security:**
- Every route is behind `auth::protected`, and writes are same-origin JSON.
- Secrets never appear in `GET`, export, preview, errors or logs.
- Import is size-bounded and parsed safely.

## Tests
- Everything in §10.10.9's "Server-side tests" except the HTTP-address timer (B2c).
- The preview = PATCH property test.
- Stale revision; multi-section atomicity; env-locked fields; SSE `settings` events after saves; the `confirm_adult` path in a batch.
- API goldens, 401, 403 and 415.

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass after merging `origin/main`.
- `cargo run -p lodestar --bin openapi` + `cd web && npm run api:types` leave no diff.
- **The PR** is titled "B2b: Settings API for the screen". Its description lists:
  - the endpoints and fields;
  - the hint vocabulary;
  - the revision and conflict rules;
  - what export and import include and exclude;
  - anything §10.10.9 asks for that's deferred, and why.
