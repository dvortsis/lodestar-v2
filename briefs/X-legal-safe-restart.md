# X-legal-safe-restart: the Legal Notice says the safe-restart check is on by default

**From:** the runner, 2026-11-27T19:47Z. **Model:** Opus. **Branch:** `slice/X-legal-safe-restart`. **PR title:** `X-legal-safe-restart: Legal Notice says the safe-restart check is on by default`.

**Why (owner, 2026-11-27, `docs/owner-decisions.md` §2.87):** CR-0w made the safe-restart check on by default, but section 5 of the Legal Notice still calls it "the optional safe-restart switch". The owner approved new wording and a NOTICE_VERSION bump, so every admin sees the corrected notice once.

**The change**
1. In section 5 of both copies, `docs/legal/legal-notice.md` and `crates/api/src/legal/notice.md` (they must stay byte-identical), replace "and the optional safe-restart switch keeps its restart record in a folder outside the data folder (user guide, section 3.8)" with the owner's words, verbatim: "and the safe-restart check, on by default, keeps its restart record in a folder outside the data folder (user guide, section 3.8)". Change nothing else in the notice. Any other wording change is the owner's.
2. Raise `NOTICE_VERSION` in `crates/api/src/legal.rs` from 5 to 6. Update the version assertion tests in `crates/api/src/auth.rs` only where they hard-code 5. Follow how `docs/decisions/2026-11-11-X-event-log-notice.md` did the last bump: comments, tests, and any notice hash or golden the build checks.
3. `crates/lodestar/tests/legal_facts.rs`: add a fact check tying the sentence to the code. The notice must say "on by default" only while the resolved default of the safe-restart setting is on (find it from CR-0w's config.rs on main). The test fails if either side changes without the other. Also check that the notice no longer contains "optional safe-restart switch".
4. **The section 3.8 pointer is the owner's words, so keep it.** On `main` today, user guide 3.8 is "Check your understanding", and the safe-restart section was taken out for v0.5.1. The runner's call (recorded in r20): D-start, the docs slice that owns `docs/user/03-keep-lodestar-safe.md`, restores the safe-restart section as 3.8 and renumbers "Check your understanding" to 3.9. Don't edit the user guide. In your decisions file, record that the pointer resolves once D-start lands.
5. **Decisions file:** `docs/decisions/2026-11-27-X-legal-safe-restart.md`. Record:
   - the wording, with its source (§2.87);
   - the version bump 5 → 6;
   - the D-start dependency for the 3.8 pointer;
   - a note that X-prowlarr-register's planned notice version moves up one (the runner updates its slices.yaml note).
6. Run `/security-review` on the diff before asking for review, as legal-notice changes do.

**Owns:** `docs/legal/legal-notice.md`, `crates/api/src/legal/notice.md`, `crates/api/src/legal.rs`, `crates/api/src/auth.rs` (version tests only), `crates/lodestar/tests/legal_facts.rs`, `docs/decisions/*-X-legal-safe-restart.md`. Leave `runner/` alone; the runner keeps its entries. Ask the runner before touching any other file. If the bump needs a change elsewhere (a web golden, an e2e fixture), tell the runner which file and why first.

**Checks before `ready`:** cargo fmt, clippy with -D warnings, `cargo test -p api -p lodestar` (legal_facts and auth at least), plus the workspace tests CI runs. If web e2e fixtures pin the notice version, they're in CI's playwright job: tell the runner rather than editing them without a grant.

**Size:** small, about 100–200 lines. Target: ready within a few hours.

**Also granted (runner):** `crates/api/src/dto/auth.rs` keeps the `LegalNotice` example equal to `NOTICE_VERSION`, and the web e2e specs read the version from `web/src/lib/api/openapi.json` (`web/e2e/support/notice.ts`). So update the example in dto/auth.rs. Regenerate `web/src/lib/api/openapi.json`, and `web/src/lib/api/schema.d.ts` if it changes, with the repo's own generator (see how earlier notice bumps did it). Never hand-edit or hand-merge them. Run `npx playwright test e2e/auth*` in web/ with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`.
