# X-restart-required-creds decisions (a pinned credential saved since the start shows `restart_required`)

Slice X-restart-required-creds, assigned by the runner on 2026-11-11 to close self-review I1 of X-prowlarr-settings (#658, `2026-11-11-X-prowlarr-settings.md`): a plugin that keeps the credential it was built with (Prowlarr's `KeyAtStart`) went on showing `restart_required: false` after its key changed. The code is `crates/pipeline/src/control.rs`, one trait method in `crates/sources/src/plugin.rs`, Prowlarr's opt-in, and one `Monitor` method in `crates/pipeline/src/lib.rs` (a shared edit, below).

## Opt-in
- **`SourcePlugin::pins_credentials`, false by default.** A plugin returns true when it reads its credentials once, when it's built, and uses those until the next start. Only those report a credential saved since. Plugins that read theirs fresh at each run need nothing and never show it.
- **Prowlarr returns true only when built by `Prowlarr::at_start`,** the build that pins its key. `Prowlarr::new` (the fixture contract tests) reads the key each run and returns false.

## Comparison: the sealed values, recorded at the start
- **`Control::start` records each pinning source's credentials as stored:** each name its `Policy::credentials` declares, with its sealed value or none. `Control::sources` reads them again, and any difference sets `config_pending`, which the API already shows as `restart_required` (`crates/lodestar/src/ops.rs`). No field, DTO or OpenAPI change, and no store change or migration.
- **Why the sealed value, not a counter or a fingerprint:**
  - The store has no per-source credential revision. Adding one without a migration would mean a new `settings` key written in `Store::update_source_secrets`, which is the store crate's and outside this slice.
  - A keyed fingerprint would have the control open the secret, or the plugin hold another derived value of it.
  - The sealed value is what the store already holds and returns: ciphertext under the install's key, which is outside the database. It's never the secret or a hash of it, never logged, serialized or returned, and `Control`'s `Debug` doesn't print it.
- **Any write shows it, even of the same value.** Every write seals afresh with a new random nonce (XChaCha20-Poly1305, SRC-4). So saving a key, then saving the start's key back, still shows `restart_required`, as a revision counter would. That's correct for a pinning plugin, which can't tell either write from a real change without opening the key, and a restart costs nothing then. The test covers it.
- **A key removed since the start shows it too:** the plugin is running on a key the store no longer has. Removing an Account source's last credential also turns it off (SRC-4), so it stops anyway.
- **When it's recorded:** after the pipeline has started and before the API serves, so nothing can write a credential between the plugin pinning its key and the record. Prowlarr pins in `at_start`, just before that. The comment where the record is taken says so, so a later reorder of start-up doesn't break it silently.
- **It never leaves `Control`:** it isn't in `SourceView` or any DTO, `Control`'s `Debug` doesn't print it, and nothing logs it. The test plants a fake key and checks every view's and `Control`'s `Debug`.
- **Clearing:** the next start records what's stored then, so a restart on the new key clears it.

## For the runner
- **`2026-11-09-CMP-5g.md`'s "after a hand-over the Prowlarr source shows `restart_required`"** isn't edited here. It becomes true once X-prowlarr-register registers the plugin through `at_start`. Until then the plugin isn't registered. The hand-over itself is tested here over a stand-in pinning plugin: the same settings saved again and a new key.
- **Shared edit, test:** `crates/sources-plugins/tests/prowlarr.rs` gets `only_the_start_build_pins_its_key` (`at_start` pins; `new` and `unconfigured` don't). The runner approved it (2026-11-11).
- **Shared edit:** `crates/pipeline/src/lib.rs` gets `Monitor::pins_credentials(id)`, beside `running_config`, so the control can ask a source's plugin. It isn't in the brief's owns; **the runner approved it** (2026-11-11). The runner suggested a `SourceStatus` field; the method in the same file is smaller. A field on `SourceStatus` instead would also have touched `crates/lodestar/src/metrics.rs`, whose test builds one; a `Policy` flag would have touched 66 policy literals.
