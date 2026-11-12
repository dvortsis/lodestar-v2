# X-consent-gate-lift decisions (r1)

Spec:
- `runner/slices.yaml`, entry `X-consent-gate-lift`, and the runner's brief of 2026-11-12.
- Owner §2.49, architecture §18.7.
- `docs/decisions/2026-11-06-SRC-15b.md`, `2026-11-07-SRC-15c.md`, `2026-11-06-X-consent-gate-polish.md` and `2026-11-07-X-sources-api-spec.md`.

This is r1. It opens External, Restricted and Overlay, and nothing else. Account stays closed until r2 (below).

## The gate
- **`control::consent_text_approved` passes None, External, Restricted and Overlay.** Account is refused. SRC-15b's dialogs show the §2.49 texts for the three acknowledged classes; Account's dialog is SRC-15c's and its condition is r2's.
- **Passing the gate opens nothing by itself.** The class condition still has to hold, so a source is turned on only when all of these hold:
  - the gate passes its class;
  - no purge of it is under way;
  - for External, Restricted and Overlay, an accepted acknowledgement of exactly the current text version and address list (`consent::version`). It's either stored or named exactly in the request, which also names the class;
  - for Restricted and Overlay, the admin password, on every enable.

  Anything else is refused: Account, a stale text version, another class's acknowledgement, a declined record, a record this build can't read, or none.
- **One function decides.** The checks moved out of `Control::update_source` into `control::enable_decision`, unchanged in order: the gate, the purge, `need`, the password. The matrix test exercises it directly over class × gate × stored acknowledgement × sent acknowledgement × password × credentials × purge (4,000 cases). Its expected answer is written separately and is "refused" unless one of its allow rules matches.
- **An unknown class can't reach the gate.** `Consent` is a closed enum, and a stored record naming a class this build doesn't know doesn't decode (`Ack::decode`), so it counts as no record.
- **The start check is the same.** `switch_off_unapproved` keeps an accepted, current source on across a restart, switches an Account source off (`consent_text_unapproved`), and switches off one whose record no longer covers what's shown (`consent_lapsed`). The records are kept.
- **A re-registration keeps acceptance.** A new plugin version re-registers its row. An acknowledgement covers the class, text version and addresses, not the plugin's version, so it still holds. A plugin update that contacts a new address asks again.

## The text version is now 2 (a tightening; §2.19 flag)
- **`consent::text_version` is 2 for External, Restricted and Overlay** (it was 1). SRC-15b decided no bump was needed because the approved texts equal SRC-4's drafts. Version 1 is still what a build before the consent gate could record, through the API, with no dialog showing any text. The gate was closed before any dialog shipped, so no version-1 record was given against the approved dialog.
- **So a version-1 record covers nothing now.** A source with one asks again, and one left enabled by an older build is switched off at the next start (`consent_lapsed`) until the user accepts the text in the dialog. Nobody in v0.5.2 could turn one of these sources on, so no one who accepted in the dialog has to accept again.
- **This supersedes** SRC-15b's "`consent::text_version`: no change" for these three classes. Account stays 0.
- **Tests:** a bump re-closes the gate for that class until the new version is accepted, at every level. That's the unit (`consent.rs`, `control.rs`), the pipeline start (`tests/control.rs`), the API (`ops/tests.rs`) and the binary (`consent_gate.rs`, which seeds a version-1 record and checks the restart switches the cache off and says why).

## Step 1: `need` follows the declared credential spec (§2.19 flag)
- **`consent::need_with(manifest, spec, ack, credentials)` and `need_in_with`** take the plugin's `Policy::credentials`. An Account source's condition:
  - **Required credentials declared:** every one of them is stored. No other name stands in, declared or not.
  - **Only optional ones declared:** at least one declared name is stored. An undeclared name, such as a typo, doesn't count.
  - **None declared:** at least one name that isn't an egress name, which was the rule before specs existed.
  - Egress names (`egress.socks5.*`) never count.
- **Call sites pass the spec:**
  - `control.rs`: the start check, the source view, the enable and the credential write's lapse check;
  - `adult_enrich.rs`: the enricher's consent condition, through a new `check_with`;
  - `lib.rs`: first registration.

  `need` and `need_in` stay, as `need_with` with an empty spec, for callers outside this slice's owns (`stashdb.rs`, `tpdb.rs`, the torrent cache's tests). Those are a plugin's own view of itself or External, so the spec changes nothing for them.
- **`check_spec`'s `NotEnough` now checks against `need_with`,** so it can catch a spec whose required set doesn't satisfy it.
- **Nothing a user can reach changes yet:** Account stays closed. This is the owner flag the runner moved here from X-sources-api-spec.
- **Not changed here:** `crates/pipeline/src/identify.rs` still calls `need_in`, which ignores the spec. It's outside this slice's owns. Identification plugins aren't Account today, and Account is closed. r2 should move it to `need_in_with`.

## Adult enrichment keeps its own gates
- **Consent is one condition of several.** `adult_enrich::check_with` still refuses while Show adult content is off, while the kill switch is tripped, or while the source is off, whatever consent says. The new test gives an Overlay source an accepted acknowledgement and checks that it offers nothing with Show adult off (`AdultHidden`) and nothing with the kill switch tripped. With both clear, it gets past consent.

## For r2 (Account)
- **Open Account in `consent_text_approved`** once SRC-15c's credentials dialog ships the §2.49 Account text. Decide `text_version(Account)`: today Account has no acknowledgement record, and its consent is its credentials.
- **Ask for the admin password again when the gate opens for Account sources** (SRC-15c's note). Today an Account enable never asks for it.
- **Refuse undeclared credential names** on `PATCH /sources/{id}/credentials` (X-sources-api-spec, item 2).
- **Decide the download clients' Account condition.** They declare no credentials of their own, so under `need_with` they need some plugin credential stored, and their credentials are Integrations'.
- **Move `identify.rs` to `need_in_with`** (above).

## Files outside the slice's owns
See the PR description: the runner was asked (2026-11-12) about the Legal Notice §2 facts test, the shipped-plugins safety test, the API's `patchSource` description and `services.rs`'s comment, all of which describe the closed gate.
