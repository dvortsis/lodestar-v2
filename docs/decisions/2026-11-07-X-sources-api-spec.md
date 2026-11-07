# X-sources-api-spec decisions (PR pending)

Spec:
- `runner/slices.yaml`, entry `X-sources-api-spec`.
- `docs/decisions/2026-11-06-runner-sources-split.md` (#550).
- `docs/decisions/2026-11-06-SRC-15b.md`.
- `docs/decisions/2026-11-06-X-consent-gate-polish.md` (#553).
- Architecture §18.7, §18.8 and §10.10.7.

This PR adds read-only API surface and one read-only store query. It opens no consent class and loosens nothing. The gate stays closed in v0.5.2 (#550).

## The credential spec lives in `Policy`, not `Manifest`
- **`Policy.credentials: &'static [CredentialSpec]`** holds each credential's `name`, `label`, `kind` (`Secret` or `Username`) and `required`. `Policy::STRICT` gives `&[]`.
- **Why `Policy`:**
  - It is the manifest's v2 half (§18.1), and `config_schema` already lives there.
  - `SourceStatus.policy` already carries it to `crates/lodestar/src/ops.rs`, so the pipeline needed no change.
  - A `Manifest` field would have broken about 35 struct literals across the pipeline, the binary and their tests, because `Manifest` has no default.
  - Plugins without credentials need no edit, apart from the one-line `..Policy::STRICT` below.
- **The types are re-exported from `lodestar_sources::consent`**, since the crate's `manifest` module is private and `lib.rs` is outside this slice's owns.

## What each plugin declares
- **Prowlarr, ThePornDB and StashDB:** `api_key`, labelled "API key", kind `secret`, required. These are the only names their code reads (`credentials.get(ID, API_KEY)`).
- **No plugin's `CONFIG_SCHEMA` lists `api_key` any more** (runner, 00:29Z for Prowlarr, 01:00Z for the other two: one rule everywhere).
  - Prowlarr's `Config::from_json` already refused it as a setting, so a form built from the schema would have asked for a field the server rejects.
  - StashDB's and ThePornDB's schemas listed nothing else, so they are now the empty object.
  - `tests/{prowlarr,stashdb,tpdb}.rs` each now assert it's absent.
  - `credential_spec.rs` checks that no plugin's schema shares a name with its declared credentials or lists a write-only field.
- **All the others declare none.** They're built without a credential store, so they can't read one. That includes the download clients (`clients.rs`): their credentials are Settings → Integrations', handed to the plugin in its `ClientConfig`.
- **Four plugin files gained only `..Policy::STRICT`:** `full_scrape.rs`, `academic.rs`, `nyaa.rs` and `archive_org.rs`. Their `POLICY` listed every field, so the new field didn't compile without it. Prowlarr, which also listed every field, now sets `credentials` itself (its declaration), so it needs no `..Policy::STRICT`. Runner approval: see "Files outside the slice's owns".

## Checked against `consent::need` and what the plugins read
- **`consent::check_spec(manifest, spec)`** checks four things:
  - each name passes `check_name`;
  - no name is an egress name (`egress.socks5.*`, the egress setting's);
  - no name is declared twice, and each has a label;
  - there are at most 16, and storing every required one satisfies `need` for an Account source.
- **`crates/sources-plugins/tests/credential_spec.rs`** runs Prowlarr, ThePornDB and StashDB against a recording credential store and fails if any of these happen:
  - a plugin asks for a name it doesn't declare;
  - it declares one it never asks for;
  - it reads under another plugin's id;
  - its schema lists a write-only property, or one named as a declared credential;
  - a plugin in the catalogue isn't checked.
- **`need` itself is unchanged in this PR.** An Account source is still satisfied by any non-egress credential stored.
  - The runner agreed to tighten it: a declared required set must be stored exactly, and with nothing declared today's fail-closed rule applies.
  - The spec lives in `Policy`, and `need`/`need_in` take only the `Manifest`, so every caller would have to pass the spec. There are six sites in `crates/pipeline`, outside this slice's owns, so the runner deferred it to X-consent-gate-lift (2026-11-07T01:00Z). That is the lift's step 1, below.
  - Account stays closed in this build, so today's rule changes nothing a user can reach.

## `GET /sources`: `credential_spec` and `config_schema`
- **Both are new required fields of `Source`.** `credential_spec` lists the declared fields in order. `config_schema` is the plugin's JSON Schema, parsed.
- **The listed schema has no secrets in it.** Before this PR, the three API-key plugins listed `api_key` in their schema as `"writeOnly": true`, as `Policy.config_schema`'s doc asked. None does now (above).
  - `GET /sources` drops every `writeOnly` property and every property named as a declared credential, together with their `required` entries.
  - That way a form generated from the schema never asks for a secret as a setting, whatever a plugin's constant says. It's the second guard behind the plugin test, and `crates/lodestar/src/ops.rs` tests it on a schema that has both kinds.
- **A schema that doesn't parse** reads as `EMPTY_SCHEMA`. That would be a plugin bug, and the plugin tests parse every schema.
- **No value is ever listed.** The spec describes fields. `credentials` still lists only the names stored. `crates/api/tests/sources_spec.rs` stores a value and checks four places never quote it: the write's own response, both `GET` routes and the purge preview. It also checks that a malformed write's refusal doesn't quote it.

## The purge preview: `GET /sources/{id}/purge/preview`
- **Session-only, no password, no body (runner, 2026-11-07T00:29Z).** It is a `GET`, behind the session guard like every Sources route, so an API key or feed token gets `401`. It returns counts only to a signed-in admin.
  - The password is asked once, by the commit (`POST /sources/{id}/purge`), as before. Asking for it at preview time would make SRC-15c's dialog either ask twice or keep the password in the page's memory between the two calls.
  - A `GET` fits the router's CSRF model: it writes nothing, and `GET /sources` is read with the same session cookie. `POST` on the preview answers `405`.
- This session first proposed a `POST` carrying the password. The runner changed it.
- **It changes nothing.** The source keeps its state, data, credentials and settings, and no job starts.
- **`Store::purge_preview(source)` shares its rules with `purge_source`** (the runner's condition), so the two can't drift apart.
  - One function, `fate(sources, bit)`, decides each row: this source's bit alone deletes it, another bit keeps it, no bit leaves it alone. The purge's unverified and torrent stages both use it.
  - The page queries and the per-source table filters (claims, plugin state, the persisted queue) are shared macros: the purge deletes with them, and the preview pages and counts with them.
  - Each page is its own short read, not one snapshot over the whole scan, so a purge step's WAL checkpoint (`scrub_wal`) never waits on a long preview (this PR's self-review). One snapshot would buy nothing: the counts are only exact while nothing writes anyway.
  - It counts unverified items deleted and kept, claims deleted, torrents deleted and kept, plugin state entries and persisted queue keys. Each count equals the sum of the `PurgeStep` field of the same name over a whole purge run, while nothing else writes. `crates/store/tests/unverified.rs` asserts this for every field, on the existing purge fixture.
- **The binary runs it on a read-only connection of its own** (`Store::open_reader`), as `finish_purge` does, so the scan of the unverified items and torrents never holds the shared store's lock.
- **`settled`** is true while the source is Disabled or Purged and its plugin stopped. Paused isn't settled: its pending keys are still fetched, and a purge refuses it. Then a purge started next removes exactly the counts, unless another source changes the store meanwhile: a torrent the DHT also finds is kept, not deleted.
  - An enabled source keeps finding until the purge turns it off, so the purge may remove more.
  - Keys still in memory waiting to be fetched aren't counted. The purge job reports them as `keys_dropped`.
- **Size isn't reported yet.** §10.10.7 asks for "the exact count and size". SQLite can't attribute bytes on disk to rows cheaply, and the sum of the torrents' content sizes would read as disk space freed, which it isn't. The dialog shows counts; the button names the torrent count ("Delete 1,204,331 torrents found only by Nyaa", §10.10.7's own example). If a size is wanted, it's a later slice.

## `patchSource`'s description
- It now says what this build does:
  - only a `none` source can be turned on;
  - every other class gets `400 invalid_setting` on `enabled` before its consent, acknowledgement or credentials are looked at;
  - a password sent with the enable is still checked first;
  - `needs` and `credential_spec` say what the source will need later;
  - credentials can be stored and removed meanwhile.
- It no longer says an Account source gets `409 credentials_required`, which can't be reached in this build (#553), or describes the consent flow as live (`docs/decisions/2026-11-07-runner-followups.md`). The module doc says the same.

## OpenAPI
`web/src/lib/api/openapi.json` was regenerated with `cargo run -p lodestar --bin openapi`, and `schema.d.ts` with `npm run api:types`. Neither was edited by hand.

## For SRC-15c
- **The Account dialog** asks for `credential_spec`'s fields in order: a masked input for `secret`, a plain one for `username`. The label is the spec's English label until its messages are added. It sends `PATCH /sources/{id}/credentials` with `{name: value}` and never shows a stored value. `credentials` says which are stored, so a stored one shows as "Saved", with Replace and Remove.
- **Generated settings forms** come from `config_schema` and are sent as `config` (only when `editable` lists `config`). The schema never holds a credential field.
- **The purge dialog** calls `GET /sources/{id}/purge/preview` once when it opens (each call scans the store), shows the counts, and asks for the password only when you commit (`POST /sources/{id}/purge`). When `settled` is false, say the source is still on, so the purge may remove a little more.
- **Networks still have no display names** in the manifest (SRC-15b's follow-up). This slice didn't add them.
- **`SourcePatch.password` is still missing from the schema** (SRC-15b's follow-up). It's outside this slice.

## For X-consent-gate-lift
1. **Tighten `need` for Account to the spec** (runner, 2026-11-07T00:29Z, deferred here at 01:00Z): with any required credential declared, exactly those must be stored, not just any non-egress name. Keep today's rule for a plugin that declares none, which fails closed. `need`/`need_in` gain the plugin's `Policy::credentials`, and every caller passes it:
   - `crates/pipeline/src/control.rs`: line 635 (`switch_off_unapproved`), 1140 (the view's `need`), 1291 (enable) and 1434 (the credential write's lapse check);
   - `crates/pipeline/src/adult_enrich.rs:342`: the enricher `consent_met` gate;
   - `crates/pipeline/src/lib.rs:1259`: the enabled-by-default check.

   Line numbers are as of this PR's base. `consent::check_spec`'s `NotEnough` branch then has something to catch.
2. **Refuse undeclared names** on `PATCH /sources/{id}/credentials` (`control::update_credentials`), egress names apart, so a typo can't satisfy `need` or sit unused in the store.
3. **The download clients are Account with nothing of their own to store:** their credentials are Integrations'. Under today's `need` they can never be enabled. Decide their Account condition (for example, a configured client) before opening Account.
4. **Prowlarr's schema allows `agents` up to 8, but its `max_agents` is 1** (SRC-14a). This isn't from this slice; noted because SRC-15c's generated form will show the field.

## Files outside the slice's owns
Runner approval, in the runner's replies to this session (2026-11-07T00:29Z, 00:30Z and 01:00Z):
- `crates/sources-plugins/src/{prowlarr,stashdb,tpdb}.rs`: the `api_key` declaration, and `CONFIG_SCHEMA` without `api_key`.
- `crates/sources-plugins/tests/{prowlarr,stashdb,tpdb}.rs`: the one assert each about `api_key` in the schema.
- `crates/lodestar/src/ops/tests.rs`: the end-to-end preview test.
- `crates/sources-plugins/src/{full_scrape,academic,nyaa,archive_org}.rs`: `..Policy::STRICT`.
- `crates/sources-plugins/tests/credential_spec.rs`: new.
- `crates/store/src/unverified.rs`: `PurgePreview` and `Store::purge_preview` only.
- `crates/store/tests/unverified.rs`: the preview assertions in `purge_removes_only_what_the_source_alone_found`.
- `crates/api/tests/ops.rs`: the two new fields in `dht_source()`.
- `crates/api/tests/golden/ops_*.txt`: re-blessed for the two fields.
- `crates/api/tests/openapi.rs`: the new route in `ROUTES`.
- `web/src/lib/features/sources/fixtures.ts`: the two fields' defaults.
