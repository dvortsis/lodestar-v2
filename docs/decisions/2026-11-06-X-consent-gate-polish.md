# X-consent-gate-polish decisions (#553)

Spec:
- `runner/slices.yaml`, entry `X-consent-gate-polish`.
- `docs/decisions/2026-11-06-X-consent-gate-052.md` (#537).
- `docs/decisions/2026-11-06-runner-sources-split.md` (#550).
- Owner §2.49.
- `docs/plan/2026-11-06-v052-feature-manifest.md` §3.4 and Q4.

This PR tightens the gate. It loosens nothing.

**What it supersedes:**
- In `docs/decisions/2026-11-06-X-consent-gate-052.md`: that an Account source with stored credentials can be turned on, and stays on at a start.
- The same file's "the log event carries no reason field".
- In `docs/plan/2026-11-06-v052-feature-manifest.md` §7: "keep the current wording" for the Known Issues line on the torrent cache. The runner approved the new wording (below).

## Account is closed too
- **`control::consent_text_approved` now passes only None.** Owner §2.49 lets a class through only once its dialog shows the approved text. Account's dialog (the credentials form, SRC-15c) hasn't shipped, so Account now waits like External, Restricted and Overlay. #537 had let it through once its credentials were stored, because it has no consent text to acknowledge.
  - **`PATCH /sources/{id}` refuses `enabled: true` for an Account source before its credentials are looked at.** The answer is the same `400 invalid_setting` on `enabled` with `CONSENT_GATE_REASON`, whether or not credentials are stored. `409 credentials_required` can't be reached in this build. The gate stays the first check, so every closed class answers the same way, and "add your credentials" isn't suggested when adding them wouldn't turn the source on.
  - **Credentials are still required.** `GET /sources` still says `needs: "credentials"` until they're stored. Storing and removing them works as before. Removing the credentials of an enabled Account source still switches it off in the same write. Once X-consent-gate-lift opens Account, `consent::need_in` applies unchanged.
  - **The start check is the same function,** so an Account source an earlier build turned on is switched off at the next start before it runs (`consent_text_unapproved`). Its credentials are kept.
- **Nothing changes for users.** `crates/lodestar/src/services.rs` registers three sources:
  - the DHT (`crates/pipeline/src/dht.rs`, class None);
  - the torrent cache (External, already closed);
  - the Sonarr/Radarr webhooks (`crates/sources-plugins/src/arr.rs`, None).

  The Account plugins in `crates/sources-plugins` (Prowlarr, StashDB, TPDB) are built and tested there, but none is registered.

## The start switch-off says why
- **When a start turns a source off, `pipeline.source_state_changed` carries `reason`:**
  - `consent_text_unapproved`: its class isn't `consent_text_approved` in this build.
  - `consent_lapsed`: its class condition no longer holds (`consent::need_in`, §18.7, §18.9). The text or endpoints changed since it was acknowledged, its credentials are gone, or it was enabled some other way.
  - A change made through the API logs no reason, as before.
- **The class check comes first,** so a source that fails both is logged once, as unapproved. In this build only None can pass the class check, and None never lapses. `consent_lapsed` therefore can't happen until X-consent-gate-lift opens a class, and a unit test (`off_reason`) covers it instead.
- **Both checks now run in `control::switch_off_unapproved`.** The condition check used to be its own loop in `prepare` (`crates/pipeline/src/lib.rs`). The order (class first, then `consent::need_in`), the store writes, the kept acknowledgements and the one event per source switched off are unchanged.
- **The field is a `Label`, so the logs catalogue golden doesn't change.** It lists only non-label kinds. It was re-blessed with `LODESTAR_BLESS=1` to confirm that, and `npm run event-names` gives the same file. The en and zh help lines for the event now say that a start names its reason.

## The v0.5.2 release notes
- **They now say the torrent cache can't be turned on yet** (Known Issues), and that a source needing consent, one a pre-release build turned on included, stays off until a later version asks (Changed). Before, the notes said it stays off "until their checks pass", and that a pre-release build's source "asks again". This build refuses it, so nothing can ask again.
- **The word cap:** the `material-composer` gate (`lint_material.py --style`) reports `WORD_LIMIT` 500 words of 450. `main` was already at 508 before this PR, and these two bullets cut 8. The rest is over the cap outside these bullets, so this PR leaves it for the release-notes owner (P-v052-cut).

## Files outside the slice's owns
- **`crates/pipeline/src/lib.rs`:** only the start's condition loop is deleted, because it moved into `control::switch_off_unapproved`. The one-line call stays. Runner, 2026-11-06T22:05Z.
- **`docs/releases/v0.5.2.md`:** the two bullets above, as worded to the runner. Runner, 2026-11-06T22:19Z.

## OpenAPI wording
- **`Source.consent_required` and `SourcePatch.consent`** now say two things:
  - This build refuses to enable any source whose class isn't `none` (`400 invalid_setting` on `enabled`), whatever the request sends.
  - Once a later version ships the class's approved text, the field is used as it was described before.

  `openapi.json` and `schema.d.ts` were regenerated with the repo's tooling. #537 left this as a follow-up.

## Tests
- **`crates/lodestar/tests/consent_gate.rs`:** at the restart, the torrent cache with a current record has `needs == null`. It's off because of the gate, not because its consent lapsed. The log line carries `consent_text_unapproved`, and no line says `consent_lapsed`.
- **`crates/pipeline/tests/control.rs`:**
  - `a_restart_switches_off_a_source_whose_condition_no_longer_holds`: `acct_ready` (credentials stored, enabled) is now switched off and never runs. A None source, on by default, is what the test waits for. Its credentials are kept.
  - `an_account_source_needs_credentials_and_losing_them_disables_it`: enabling is refused before and after the credentials are stored, and `need` goes from `Credentials` to none. The credential checks are unchanged (a bad name, the stored name, the value never shown, removal and the limit of 16). The credential-removal leg seeds an enabled state, as an earlier build could leave it.
  - `losing_a_credential_stops_the_running_plugin`: enabling is refused. A seeded enabled state is switched off at the start, so the plugin never runs. Removing the credential leaves it off.
- **`crates/lodestar/src/ops/tests.rs`, `no_class_can_be_enabled_past_its_condition`:** Account gets the gate's `400` with and without credentials. `needs` goes from `credentials` to null. The credential write, the bad-name refusal, the export and plaintext checks, and the removal that switches an enabled (seeded) source off are unchanged.
- **The `control.rs` unit test** asserts that only None is approved, and that every refused class has a dialog to show.
- **No test was deleted.**

## For X-consent-gate-lift: what to bring back as each class opens
- **External, Restricted and Overlay** (with SRC-15b's dialogs): the acceptance assertions #537 replaced, as its decisions file lists them.
- **Account** (its second PR, with SRC-15c's dialog):
  1. `crates/pipeline/tests/control.rs`, `an_account_source_needs_credentials_and_losing_them_disables_it`: enabling without credentials is `SourceError::CredentialsRequired` again. With them, `update_source` succeeds with `SourceState::Enabled`. Drop the seeded `enable_source` before the removal.
  2. `crates/pipeline/tests/control.rs`, `losing_a_credential_stops_the_running_plugin`: enabling through `update_source` succeeds. After the restart, wait until the ticker has sent at least 3 times. Removing the credential stops the running plugin at once (`running` false). Drop the seeded state and the "never ran" asserts.
  3. `crates/pipeline/tests/control.rs`, `a_restart_switches_off_a_source_whose_condition_no_longer_holds`: `acct_ready` stays on and runs. The other six are off.
  4. `crates/lodestar/src/ops/tests.rs`, `no_class_can_be_enabled_past_its_condition`: `409 credentials_required` before the credentials are stored, `200` after. Drop the seeded enable.
  5. `crates/pipeline/src/control.rs`, `only_a_class_with_no_consent_dialog_can_be_turned_on`: Account goes back to `true`.
- **`consent_lapsed` gets an end-to-end test** once any class other than None is approved. An opened source whose acknowledgement is stale is switched off at the start with that reason.
- **The OpenAPI wording** for `consent_required` and `SourcePatch.consent` drops the "this build refuses" paragraph for the classes the lift opens.
