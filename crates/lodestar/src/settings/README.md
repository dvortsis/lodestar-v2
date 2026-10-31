# The settings registry

`lodestar::settings` holds the runtime settings the app shows and changes (architecture §10.10, §11). `crates/api/src/settings.rs` serves it under `/api/v1/settings` through the `SettingsApi` trait.

## Layers

From lowest to highest: the built-in **default**, **`config.toml`**, a value **saved in the app** (the `settings` table, under `settings.<section>.<field>`, as JSON text), and a **`LODESTAR_*` environment variable**. Each field's effective value carries its `source` (`default`, `file`, `db`, `env`).

- A field the environment sets is read-only in the app. `PATCH` refuses it with `409 set_by_environment`, naming the variable, and a saved value never overrides it.
- **`null` resets a field.** In a `PATCH`, it removes the field's saved value in the same transaction as the rest of the patch. The file's value or the default then applies again, and the change is published like any other. This is how a saved value that turns out wrong is undone, so no section field is itself nullable: `crawler.max_sockets` uses 0 for "follow the intensity".
- A saved value that's no longer valid (an older build's, a hand edit) is skipped with a warning at start, never fatal. A saved value that shadows a different `config.toml` value is logged once at start (`section.field` only, never the value), with how to reset it.
- At start, `Services::start` applies **only the saved fields** to the `Config` every service is built from (`Registry::effective_config`). File and environment values reach the services exactly as written. For example, two DHT sockets on different ports keep their ports unless a port was saved.
- `lodestar healthcheck` probes the effective HTTP bind: the environment's, else a saved one (read from the store, read-only), else the file's (`network::effective_bind`).

## Revisions, batches and preview

- **Each section has a revision**, saved as `settings.<section>#revision` (the `#` keeps it out of the section's `settings.<section>.` prefix; no migration). It starts at 0 and goes up by one with every save that changes the section's saved values; a save that changes nothing leaves it.
- **`if_revision`** in a section's patch names the revision the edit started from. If the section has moved on, the save is refused with `409 stale_revision` and the current values of every stale section in the batch (`SettingsError::Stale`), so the UI can merge (§10.10.5). A save without it isn't checked.
- **A batch** (`PATCH /settings`, `{section: {field: value}}`) is planned whole under the registry's write lock: unknown sections, then every revision, then each section's fields and confirmations, in display order. One refusal anywhere saves nothing, and the error names its `section` and `field`. Then every changed section's values and revision are written in one transaction, and each changed section's `watch` channel publishes. The single-section `PATCH /settings/{section}` is a batch of one.
- **The preview** (`POST /settings/preview`) runs exactly the same planning and saves nothing (a property test in `screen_tests.rs` holds that it accepts exactly what a save accepts). For each patched field it returns the value it would have, whether it applies now or at a restart, and the section's `Section::preview` estimates and warnings.
- **The `settings` event** (`{section, revision}`, no values) goes out on the `/events` hub after every save that changed a section, from the API's routes (`EventHub::settings_saved`).

## Export and import

`portable.rs`. Export writes the values saved in the app as TOML, one table per section, each key with its `config.toml` key and `LODESTAR_*` variable in a comment: never a secret, a value the environment sets or a read-only field, and `preferences` only when asked. Filter state isn't in any section, so it can't be exported. Import reads such a file (64 KiB at most), lists what it leaves alone (`unknown`, `read_only`, `secret`, `set_by_environment`) and what it changes, and saves the changes through the batch planning: all or nothing, with `confirm_adult` and `if_revision` as a batch takes them.

## Live and restart-only fields

Each section has a `tokio::sync::watch` channel (`Registry::subscribe::<T>()`), which a patch publishes on only when the section actually changes.

- **Live:** every `logging` field but `file_folder` (`log::follow` applies the section: event switches, data kinds per output, the outputs, the file's limits and the filter for untyped lines, and wakes at each deadline so a time-limited kind turns off with no save), `crawler.intensity` (`crawler::follow_intensity` hands it to the running DHT node; the fetch pool's socket count follows at the next start), `content.show_adult` (`content::follow` gives the API a reader over the channel, read at each request) and every `preferences` field (the UI applies them; the server only keeps them).
- **Restart-only:** every other field is `restart_required`. A section's `restart_required` list names the fields whose saved value differs from the running one.

## Out of bounds

- Filtering state (rules, the locked floor, packs, blocklist subscriptions) belongs to the filters API (§20.7, owner's decisions §2.7–2.9). No section exposes it, and the registry only ever reads and writes keys under `settings.`.
- **Secrets are sealed at rest** with the integrations `SecretStore` (`secrets.key` in the data directory, opened the first time a secret is saved or read). A `.secret()` field, or a list field with `.secret_keys(&[…])`, is saved as the JSON string `"sealed:<hex>"`, sealed with the context `settings.<section>.<field>` so it can't be moved onto another field. One that doesn't open (the key file lost, a value never sealed) is skipped at start like any invalid value.
  - A secret field's value is never returned (`value: null`; `source` says whether one is set), exported, imported, previewed or quoted in an error or log line.
  - A list field's items (`integrations.clients`) are saved as plain JSON with each secret key sealed in place (context `settings.<section>.<field>.<item id>.<key>`), and come back without their secret keys; `secrets_set` lists the ones set, as `<item id>.<key>`. An item saved without its secret, or with `null`, keeps the saved one when its `id`, `kind` and `url` origin match; `""` removes it. The whole field is skipped by export and import.
  - `""` for a secret field resets it. `Section::SECRET_ENDPOINTS` names the fields that say where a secret is sent: a patch that moves one to another server without the secret resets the secret.
  - Hold secrets in a section's struct as `SecretText`, whose `Debug` is `***`; the registry's own `Debug` names sections only.
- No field moves files at runtime. `general.data_dir` is shown read-only.
- `network.secure_cookies` is read-only too, set in `config.toml` or `LODESTAR_SECURE_COOKIES`. Turned on in the app over plain HTTP, browsers would drop the session cookie, and nobody could log in to turn it off.
- Read-only fields have `readOnly: true` in their section's schema.

## Adding a section

A later slice adds:

1. **A module** `settings/<name>.rs` with one struct deriving `Serialize`, `Deserialize`, utoipa's `ToSchema` and `#[serde(deny_unknown_fields)]`, plus a `Default`, and an `impl Section`:
   - `SECTION`: the name in `/settings/{section}` and in saved keys. Never rename it: saved values are keyed by it.
   - `FIELDS`: one `Field` per struct field, in display order. Give it `.file("table.key")` if `config.toml` sets it, `.env("LODESTAR_…")` if a variable does, `.restart()` unless a subscriber applies it live, `.read_only()` if the app must never change it, and `.range(min, max)` for numbers. Labels and help are the message keys `settings.<section>.<field>.label` and `.help`.
   - **Form hints** (schema `x-…` keys, §10.10.9; the server checks everything regardless): `.group("id")` (required; the heading is the message `settings.group.<id>`), `.advanced()`, `.unit("seconds" | "mebibytes" | "connections" | "days" | "files")`, `.widget("radio" | "slider" | "theme" | "shortcuts" | "language" | "time-zone" | "log-events" | "log-components" | "log-kinds" | "duration")` (`duration` is a deadline in Unix seconds: 0 off, -1 until turned off), `.format("socket-addr" | "log-filter" | "time-range")` (`time-range` is `HH:MM-HH:MM`, wrapping midnight), `.auto(Auto::Int(0))` for the "automatic" value, `.preview()` when `Section::preview` has estimates or warnings for it, `.keywords()` for search synonyms (the message `settings.<section>.<field>.keywords`), `.danger(Danger::…)` (§10.10.7), `.secret()` and `.secret_keys(&["password"])` (see Out of bounds). `x-order`, `x-config` and `x-env` come from the field's position, `.file` and `.env`. A test holds every field to this vocabulary.
   - `from_config` / `apply`: how it reads `Config`, and how one saved field is written back into it. A section no config key backs (like `transport`) returns its `Default` and applies nothing.
   - No field may be nullable: `null` in a patch means reset. Use a sentinel value instead (`max_sockets: 0`).
   - `check`: any validation beyond type and bounds. It runs on the section the whole patch makes, so it may compare fields.
   - `CONFIRMATIONS` / `confirm`: patch keys that aren't fields but confirm a change (`content`'s `confirm_adult`), checked against the whole patch after the fields' own checks and never saved. `if_revision` is reserved in every section, and so are the section names `preview`, `test`, `export` and `import` (the API's own routes).
   - `preview`: estimates (`sockets`, `url`, `memory_mib`…) and warnings for the fields marked `.preview()`.
   - Mark enum fields `#[schema(inline)]` and leave them undocumented, so the schema has no `$ref` (forms can't follow one) and no `oneOf` wrapper.
2. **One registration line** in `Registry::load`, in display order.
3. **A subscriber** for each live field, started in `services.rs` with `registry.subscribe::<T>()`.
4. **A new environment variable** goes in `config::ENV_VARS` too, or its source is reported as the file's.

The tests in `tests.rs` and `screen_tests.rs` walk every registered section, so a new one is covered for its schema, message keys and sources. Add its own validation and live tests beside them, and a golden case in `api_tests.rs` (`LODESTAR_BLESS=1 cargo test -p lodestar settings::api_tests`).
