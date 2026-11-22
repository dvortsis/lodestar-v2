# Runner decisions: the SRC-15b split, the consent-gate lift, and new follow-up slices (#550)
These are the runner's calls, auto-approved as best practice (`docs/owner-decisions.md` §2.33) and flagged to the owner (§2.19). None of them loosens an owner decision or weakens security or content safety.

## SRC-15b narrowed; SRC-15c and X-sources-api-spec added (runner, 2026-11-06T21:30Z)
- **The API can't drive two of SRC-15b's parts yet.** Plugins declare no credential spec at all, and the manifest's `config_schema` isn't exposed by `GET /sources`.
  - SRC-15b's session proposed generic name/value rows and a JSON editor.
  - **Refused:** those ask our users to know internal key names, which is below the user-docs bar (owner §2.14).
- **What SRC-15b ships:**
  - the External, Restricted and Overlay consent dialogs, with the §2.49 texts verbatim;
  - the purge flow;
  - a strings test over **all four** approved texts, Account included.
- **The new slices:**
  - **X-sources-api-spec** (Opus): adds a credential spec to the manifest, exposes `config_schema`, and adds a purge preview count.
  - **SRC-15c** (Sonnet): the Account dialog, generated config forms and purge counts. It comes after X-sources-api-spec.
  - Until SRC-15c merges, an Account source keeps SRC-15a's "needs setup" state.
- **The order is X-consent-gate-polish → X-sources-api-spec,** because both edit `crates/api/src/dto/ops.rs` and the OpenAPI files.
- **Until SRC-15c, SRC-15b's purge dialog shows no count before the run.** It names what is deleted and shows the job's counts afterwards. That is a known gap against §10.10.7's "exact count and size" (architecture §10.10.7), closed by X-sources-api-spec's preview and SRC-15c.
- **The approved texts live in owner §2.49.** `docs/decisions/2026-11-02-SRC-4.md` still heads them "DRAFT". The README's rule keeps another slice's file unedited, so this file is the pointer: the SRC-4 section, with §2.49's Account change applied, is the approved text.

## The consent gate stays closed in SRC-15b and in v0.5.2; X-consent-gate-lift lifts it after the cut (runner, 2026-11-06T22:00Z)
- **SRC-15b is a Sonnet slice, so it doesn't change the server gate.** CLAUDE.md keeps security-sensitive code off Sonnet, and the gate is X-consent-gate-052's `consent_text_approved` in `crates/pipeline/src/control.rs`.
- **v0.5.2 keeps every consent source off.** The v0.5.2 manifest (`docs/plan/2026-11-06-v052-feature-manifest.md` §3.4, the torrent cache row and Q4) says no source that needs consent can be turned on in v0.5.2. The lift is therefore its own slice, **X-consent-gate-lift** (Opus), which depends on SRC-15b, X-consent-gate-polish and P-v052-cut, so it lands in v0.6.
  - It lets External, Restricted and Overlay through once SRC-15b's dialogs ship their §2.49 texts.
  - It opens Account, and settles `text_version(Account)`, in a second PR once SRC-15c's dialog ships.
  - It comes after X-sources-api-spec, which also edits `crates/sources/src/consent.rs`.
- **P-v052-cut now depends on X-consent-gate-polish,** so v0.5.2 is cut with Account closed too.
- **Account is closed until its dialog ships.** Today `consent_text_approved(Account)` is true. §2.49 lets a class through only once its dialog shows the approved text. **X-consent-gate-polish** (before the cut) closes Account too. No Account plugin is registered in `crates/lodestar/src/services.rs` today, so nothing changes for users.
- **The result:** no source class opens before its approved dialog has merged and the lift has been reviewed, and none opens in v0.5.2.

## Purge asks for the admin password
- **The server requires it:** `POST /sources/{id}/purge` answers `428 confirm_required`/`401` without it (`auth::confirm_fresh_password` in `crates/api/src/ops.rs`; the field is `PurgeRequest.password` in `crates/api/src/dto/jobs.rs`).
- **The plan's wording is looser:** plan Q13 and §10.10.7 say purge needs a confirmation and no password.
- **The server's behaviour stands.** It's the stricter side, and a purge can't be undone. SRC-15b follows the server.
- **This file supersedes, for source purge, plan Q13's "Purge is already settled by §10.10.7 (confirmation, no password)" and `docs/decisions/2026-11-01-P-v06-sources-plan.md`'s "Purge follows §10.10.7: … and no password".**

## New follow-up slices
- **X-fetch-pool-bytes** (Opus, v0.5.2):
  - The fetch pool's finished fetches aren't byte-capped. The worst case is about 6.3 GiB at today's defaults.
  - This is pre-existing and also in v0.5.1. It's not a v0.5.1 blocker: soak r5 measured realistic memory, and the worst case needs crafted dicts.
  - WK-10 now depends on it.
- **X-cmp0-polish** (Opus): CMP-0 (#548) review notes. It lands before CMP-13, which shares `crates/companions/tests/catalogue.rs`. FlareSolverr is matched by substring, `%3b` is refused, templates are tied by id, the release-folder shape is fixed, and doctest error codes are named.
- **X-gs-gate-polish** (Sonnet): GS-gate (#546) review notes. They cover the job timeout, a stale doc comment, and the "within about 0.2 B" wording.

## Owns corrections
- **ENR-9b:** `crates/pipeline/tests/claims_v3_inactive.rs` (it rewrites the inactive-v3 test) and its decisions file.
- **CMP-13:** `crates/companions/data/catalogue.json` and `crates/companions/tests/catalogue.rs`, the files its tool writes and checks.
- **SRC-15b:** its decisions file, and `web/.size-limit.mjs` for the lazy-dialog exemption and its on-demand entry (runner, 21:46Z).
- **ENR-9b:** also `crates/pipeline/src/writer.rs`, only at `promote`'s judge call, so a details section is judged with the verified name at promotion as at ingest (runner, 21:48Z; a content-safety gap its `/security-review` found).
- **X-sources-api-spec:** also `crates/sources/src/consent.rs` and `crates/lodestar/src/ops.rs`, which the credential-spec check and the purge preview need.

## The v0.5.2 manifest's Legal Notice lines
- **v0.5.1 ships notice version 3** (rc.5, #540). **`main` is at version 4** (#547).
- **`docs/plan/2026-11-06-v052-feature-manifest.md` now says so** in the release-branch note, the store-format table, the upgrade steps, Q3 and Q5.
- **The rollback answer is unchanged:** v0.5.1 reads a stored 4 as accepted.
