# SRC-15b: Settings → Sources: consent dialogs, credentials, generated config forms, purge flow
**Model:** Sonnet (UI, pattern-following), `/security-review` before the PR. A fresh Opus reviewer follows. Runner brief, 2026-11-06T21:25Z.

## Spec
- `docs/plan/2026-11-01-v06-sources.md` §4 "SRC-15a/b" and Q1, Q5, Q13; architecture §18.7, §18.8, §10.10.7 (purge: confirmation, no password), §10.14 (what happened, then what to do).
- **The approved consent texts:** `docs/owner-decisions.md` §2.49 plus `docs/decisions/2026-11-02-SRC-4.md` "Draft consent texts" (External, Account, Restricted, Overlay), **with §2.49's one change to the Account text applied.** Show them verbatim. Fill `{name}`, `{hosts}`, `{egress}` and `{network}` from the API (manifest and endpoints). The headings, bullets, bold and button labels are part of the approved text.
- **The server's model:** `crates/sources/src/consent.rs` (`need`, `needs_ack`, `needs_password`, `Ack`, `version`), `crates/pipeline/src/control.rs` (`consent_text_approved`, `CONSENT_GATE_REASON`), and the API DTOs (`crates/api/src/dto/`, `web/src/lib/api/schema.d.ts`: `SourcePatch.consent`, `consent_required`, credentials endpoints). Build on what SRC-14 and SRC-15a shipped: `web/src/lib/features/sources/**`.

## Do
1. **The consent dialogs:** one per class.
   - **External:** list the hosts and the egress, then "Turn on {name}".
   - **Account:** credential fields generated from the manifest's credential spec, then "Save and turn on".
   - **Restricted and Overlay:** the admin password is required (Q13), then "Turn on {name}".
   - **None** stays a plain toggle.
   - The dialog sends whatever the server's enable path asks for: the ack with the version the server gave, credentials, the password. **The UI never decides that consent is satisfied.** The server's answer (`consent_required` and similar) is the truth, and every refusal is shown in §10.14 form.
2. **The X-consent-gate-052 gate stays in place.** Today the server still refuses External, Restricted and Overlay with `CONSENT_GATE_REASON`.
   - The dialog shows that refusal plainly. Don't change `consent_text_approved` or any Rust file.
   - **Lifting the gate is a separate Opus follow-up** that runs after this PR merges, so it is not in this slice.
   - Your e2e/vitest cover both the refused path (today's server) and the accepted path (a fixture or mock that accepts).
3. **Credentials:** write-only.
   - After saving, show only "set". Never echo a value, put one in a URL, log one, or keep one in a store after submit.
   - Use `autocomplete="off"` or `new-password` as fits, and clear the fields on close.
   - Removing credentials is its own action.
4. **Generated config forms** from the manifest's settings schema (types, ranges, required fields), validated client-side only as a convenience. The server validates.
5. **The purge flow** (§10.10.7):
   - a confirmation dialog that names what's deleted (claims, unverified items, credentials, the consent record), with no password;
   - it shows progress or the result.
6. **Strings test:** a vitest that checks each dialog's rendered text against the approved section. Store the approved texts once, in en.json, and have the test compare them against a fixture copied from §2.49 / SRC-4 and kept next to the test. The PR states that the strings match.
   - **zh:** translate the dialogs. The en text is the approved one. Mark zh as a translation in the decisions file.
7. **`consent::text_version`:** don't change it. It's Rust, and Account consent is given by saving credentials, not by an ack. In the decisions file, record whether §2.49's Account change needs a version bump, and why (Account has no ack record, so `text_version(Account)` = 0 is never compared). The runner settles it from your note.
8. Accessibility per §10: focus trap, Escape cancels, the initial focus is on Cancel for Restricted and Overlay, labelled fields, and status as icon + text. Tokens only, no hex, Tailwind's default scale on the §10.2 ramp, and keep `npx size-limit` green (load the dialogs lazily if they're heavy).

## Own
`web/src/lib/features/sources/**` and `docs/decisions/2026-11-06-SRC-15b.md`. If you need anything else (the openapi example, a shared component, e2e fixtures under `web/e2e/`), ask the runner first. Don't touch Rust.

## Done when
- **In `web/`:** `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, and `npx playwright test` for any spec you add or touch (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`; never `playwright install`).
- **The PR:** titled "SRC-15b: …", against `main` from `slice/SRC-15b`.
- **How you work:** follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the claude-code-remote `send_message` tool when the PR is ready, giving the head SHA. Never merge. Push once per round. No fixed sleeps. Keep the PR at or under 1,500 changed lines where you can.
