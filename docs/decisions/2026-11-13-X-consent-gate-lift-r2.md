# X-consent-gate-lift r2 decisions

Spec:
- `runner/slices.yaml`, entry `X-consent-gate-lift`, and its r2 notes.
- The runner's messages of 2026-11-13T03:38Z and 03:41Z.
- `docs/decisions/2026-11-12-X-consent-gate-lift.md` (r1, #807), `2026-11-07-SRC-15c.md`, `2026-11-06-SRC-15b.md` and `2026-11-07-X-sources-api-spec.md`.
- Owner §2.49, architecture §18.7.

This PR tightens. It opens nothing.

## Account stays closed (runner, 2026-11-13T03:41Z, option b)
- **`consent_text_approved(Account)` is still false.** Legal Notice §2 says "Prowlarr or Jackett can't be turned on in this version either", and `crates/lodestar/tests/legal_facts.rs` checks that sentence against the gate. The wording is the owner's, so the runner took a proposed sentence to the owner.
- **Once the owner approves it,** the follow-up X-consent-gate-lift-account flips the gate. In the same PR it changes both Notice copies, the `docs/legal/README.md` table and `legal_facts.rs`.
- **This PR does the rest of r2,** so that the flip is the only change left.

## Account enables ask for the admin password (from SRC-15c's note)
- **`consent::needs_password` now covers Account,** as it already did Restricted and Overlay. An Account source's credentials can reach services the user runs, such as an indexer manager, so turning one on asks for the password every time. It's never remembered (§20.11).
- **What changes now:** no Account source can be turned on, because the gate refuses Account first. One thing does show (runner review I1):
  - `GET /sources` now says `password_required: true` for an Account source.
  - So `setupNeed` (`web/src/lib/features/sources/sources.ts`) returns `'password'` for a disabled Account source whose key is stored, such as Prowlarr. Its row reads "Needs setup", "Needs your password to be turned on", instead of "Off".
  - The Account dialog has no password field, and the gate refuses the enable either way.
  - **A known UI wording gap.** X-sources-restructure fixes it in `web/`, which owns `sources.ts`. This PR changes nothing in `web/` beyond the regenerated API files.
- **For X-consent-gate-lift-account:** SRC-15c's Account dialog sends `enabled: true` without a password today. The follow-up gives the Account branch of `ConsentDialog.svelte` the password field the Restricted and Overlay branches have. Otherwise the server's `428 confirm_required` would be the answer after the gate opens.
- **The enable matrix's oracle** (`control::enable_decision`) now expects Account to need the password as well as its credentials.

## Only declared credential names are stored
- **`PATCH /sources/{id}/credentials` refuses a value for a name the plugin doesn't declare** in `Policy::credentials`. The two SOCKS5 names are always allowed. This is `consent::check_declared` and the new refusal `CredentialRefusal::Undeclared`.
  - The answer is `400 invalid_credentials`. Its message names neither the name nor the value.
  - A typo can no longer sit unused in the store, or stand in for a declared credential under the no-spec rule of `need_with`.
- **Removing any name is still allowed,** so a name stored by an older build can be cleared.
- **The real plugins are unaffected:** Prowlarr, StashDB and ThePornDB declare `api_key`, the only name they read (`crates/sources-plugins/tests/credential_spec.rs`).
- **Test plugins that store `api_key`** now declare it: `Probe` and `Ticker` in `crates/pipeline/tests/control.rs`, and the Account plugin of `gated()` in `crates/lodestar/src/ops/tests.rs`.
  - `Probe` also declares 17 optional names, so the 16-credential cap can still be reached through declared names.

## `identify.rs` judges consent against the declared spec
- **The identification gate now uses `consent::need_in_with`** with the plugin's `Policy::credentials`, through `identify::check_with`, as `adult_enrich` did in r1. So the identification runtime and `Control` can't disagree about an Account identifier.
- **`identify::check` stays** as the empty-spec form, because the pipeline's own tests call it.

## The download clients' Account condition
- **The download-clients plugin** (`crates/sources-plugins/src/clients.rs`) is Account class. It declares no credentials of its own, because its client and credentials are Settings → Integrations'. It isn't registered in `crates/lodestar/src/services.rs`.
- **Decision: it stays unsatisfiable, so it fails closed.**
  - With nothing declared, the credentials route now takes only the SOCKS5 pair for it, and egress names never count toward `need_with`. So nothing the API can store satisfies its condition.
  - With the gate closed for Account anyway, it can't be turned on by any path.
- **The slice that registers it** must give it a condition of its own: a download client configured in Settings → Integrations, checked when it's enabled and at every start, as `switch_off_unapproved` does. It must also go through the same Account gate and password. That's a new `need` input, so it's that slice's design, not a rule bent here.

## `text_version(Account)` stays 0
- **Account's consent is its credentials and the admin password, asked on every enable.** SRC-15c's dialog shows the §2.49 Account text each time the switch is turned on. No acknowledgement is recorded for Account (`needs_ack` is false), so there's no stored version to compare. Raising the number would change nothing.
- **The limit:** a later change to the Account text in substance wouldn't switch off an Account source that is already on. It would only be shown at the next enable.
  - If that's ever wanted, Account gets an acknowledgement record like the other classes, with `text_version(Account)` above 0. That's a new design for the owner, not a number to raise.
  - This settles SRC-15b's "Account needs no bump".

## Files beyond the slice's owns (runner, 2026-11-13T03:41Z)
- `crates/pipeline/src/identify.rs`.
- `control::update_credentials` is in `crates/pipeline/src/control.rs`, which is already owned.
- `crates/api/src/dto/ops.rs`: `password_required`'s doc names `account`.
- `crates/api/src/ops.rs`: the credentials route's doc.
- `web/src/lib/api/openapi.json` and `schema.d.ts`, regenerated with `cargo run -p lodestar --bin openapi` and `npm run api:types`.
