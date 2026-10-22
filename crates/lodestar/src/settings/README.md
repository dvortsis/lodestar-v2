# The settings registry

`lodestar::settings` holds the runtime settings the app shows and changes (architecture §10.10, §11). `crates/api/src/settings.rs` serves it under `/api/v1/settings` through the `SettingsApi` trait.

## Layers

From lowest to highest: the built-in **default**, **`config.toml`**, a value **saved in the app** (the `settings` table, under `settings.<section>.<field>`, as JSON text), and a **`LODESTAR_*` environment variable**. Each field's effective value carries its `source` (`default`, `file`, `db`, `env`).

- A field the environment sets is read-only in the app. `PATCH` refuses it with `409 set_by_environment`, naming the variable, and a saved value never overrides it.
- A saved value that's no longer valid (an older build's, a hand edit) is skipped with a warning at start, never fatal.
- At start, `Services::start` applies the saved values to the `Config` every service is built from (`Registry::effective_config`).

## Live and restart-only fields

Each section has a `tokio::sync::watch` channel (`Registry::subscribe::<T>()`), which a patch publishes on only when the section actually changes.

- **Live:** `general.log_level` (`log::follow` reloads the filter) and `crawler.intensity` (`crawler::follow_intensity` hands it to the running DHT node; the fetch pool's socket count follows at the next start).
- **Restart-only:** every other field is `restart_required`. A section's `restart_required` list names the fields whose saved value differs from the running one.

## Out of bounds

- Filtering state (rules, the locked floor, packs, blocklist subscriptions) belongs to the filters API (§20.7, owner's decisions §2.7–2.9). No section exposes it, and the registry only ever reads and writes keys under `settings.`.
- Secrets never pass through here: credentials live in the integrations `SecretStore`.
- No field moves files at runtime. `general.data_dir` is shown read-only.

## Adding a section

A later slice (Integrations, Notifications, Appearance, Insights & digest…) adds:

1. **A module** `settings/<name>.rs` with one struct deriving `Serialize`, `Deserialize`, utoipa's `ToSchema` and `#[serde(deny_unknown_fields)]`, plus a `Default`, and an `impl Section`:
   - `SECTION`: the name in `/settings/{section}` and in saved keys. Never rename it: saved values are keyed by it.
   - `FIELDS`: one `Field` per struct field, in display order. Give it `.file("table.key")` if `config.toml` sets it, `.env("LODESTAR_…")` if a variable does, `.restart()` unless a subscriber applies it live, `.read_only()` if the app must never change it, and `.range(min, max)` for numbers. Labels and help are the message keys `settings.<section>.<field>.label` and `.help`.
   - `from_config` / `apply`: how it reads and writes `Config`. A section no config key backs (like `transport`) returns its `Default` and applies nothing.
   - `check`: any validation beyond type and bounds.
   - Mark enum fields `#[schema(inline)]` and leave them undocumented, so the schema has no `$ref` (forms can't follow one) and no `oneOf` wrapper.
2. **One registration line** in `Registry::load`, in display order.
3. **A subscriber** for each live field, started in `services.rs` with `registry.subscribe::<T>()`.
4. **A new environment variable** goes in `config::ENV_VARS` too, or its source is reported as the file's.

The tests in `tests.rs` walk every registered section, so a new one is covered for its schema, message keys and sources. Add its own validation and live tests beside them, and a golden case in `api_tests.rs` (`LODESTAR_BLESS=1 cargo test -p lodestar settings::api_tests`).
