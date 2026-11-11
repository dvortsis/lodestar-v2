# X-prowlarr-register: register the Prowlarr/Jackett plugin behind the consent gate, under Legal Notice version 5 (Opus, /security-review)

**Run after X-event-log-notice, in the same session.** That slice moves NOTICE_VERSION 4 → 5. This one adds its §2 text under the **same version 5**: no second bump, because no release is cut between the two (§2.55). Start from `main` after X-event-log-notice merges.

**Authority:** owner §2.55. The owner approved changes 1–3 of `briefs/X-prowlarr-register-notice-draft.md` as drafted, verbatim (2026-11-11T14:42Z). Use that wording exactly in both `crates/api/src/legal/notice.md` and `docs/legal/legal-notice.md`.

## Scope
1. **Notice §2:** changes 1–3 from the draft, word for word. Nothing else in the notice changes. Restricted and Overlay keep "None is built yet."
2. **Registration** in `crates/lodestar/src/services.rs`, through `Prowlarr::at_start`, behind the consent gate:
   - A store error at start-up fails the start.
   - The source registers switched off, and it can't be turned on while `consent_text_approved` refuses every class but None, the same as the torrent cache.
3. **`legal_facts.rs`:**
   - four registered plugins;
   - the Prowlarr phrases;
   - "None is built yet." appears exactly twice;
   - Prowlarr's manifest is `Consent::Account`.

   Also update the checked-phrase table in `docs/legal/README.md`.
4. **Bring back commit 9e7285e5's binary test.**
5. **The services.rs registration is a shared edit with PK-win-portable-upd.** Whichever lands second brings `main` in.

## Owns
- `crates/lodestar/src/services.rs`
- `crates/lodestar/tests/legal_facts.rs`
- `docs/legal/legal-notice.md`, `docs/legal/README.md`, `crates/api/src/legal/notice.md`, `crates/api/src/legal.rs` (no version change here)
- the decisions file `docs/decisions/2026-11-11-X-prowlarr-register.md`

## Gates
- Standard Rust gates, plus web gates if any notice golden changes. No fixed sleeps.
- Run `/security-review`. It's an Account source sending requests to a user-given address, and the API key is sealed.
- Reply "ready" with the PR number and head once CI is green, Windows included.
