# RP-UC: the unauthorised-copy pack (UC-1 to UC-3) and MW-6 cracked software in the malware pack
**Model:** Opus (content-safety core). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05T05:02Z. Ships in v0.5.2 (owner §2.40).

## Why
The owner asked for an `unauthorised-copy` pack and for cracked software to go into the malware pack (§2.39, "Go with your recommendations"). RP-DEF (#454) has merged: packs are on by default for fresh stores, and existing installs got explicit off rows. This slice adds the pack itself.

## Read first
- `docs/owner-decisions.md` §2.39 and §2.40.
- `docs/decisions/2026-11-04-X-legal-docs.md`, section "Rule packs on by default, and the unauthorised-copy pack". **It is the spec:** the tiers UC-1, UC-2 and UC-3, the must-not-hide names, MW-6 fully specified, what `RP-UC` owns, and the test on upgraded stores.
- `docs/decisions/2026-11-05-RP-DEF.md`, then `crates/filter/src/packs/{mod.rs,manifest.rs,upgrade.rs}`, `crates/classify/src/junk.rs`, `crates/release-parser/tests/golden/`, and RP-1's precision corpus `crates/filter/data/packs/precision.txt` and its tests.

## Do (branch `slice/RP-UC` from `origin/main`)
1. **The pack, in `crates/filter/src/packs/unauthorised_copy.rs`.**
   - **UC-1, camcorder and screener rips.** Tighten the rule until `Nanny Cam 1080p x264-GRP` and `Little Ts 720p` pass. For example, require a strong source marker, or a weak one in brackets or after the year. Put the precision evidence in the PR.
   - **UC-2, cracked software.** It uses MW-6's test: the parser's `Cracked` flag **and** a crack-specific token. `patch` alone never matches.
   - **UC-3, scene-style film and TV rips,** with the official-source exemption and the allow list.
   - Music and books are out of scope.
   - Register the pack in `packs/mod.rs` and `manifest.rs`. The wizard description says what the pack detects and names Allow rules as the way back.
2. **MW-6 in the malware pack,** exactly as the spec says:
   - software kind plus the `Cracked` flag plus a crack-specific token (crack, cracked, keygen, keymaker, activator, k'ed, pre-activated); action hide;
   - lawful patch and update names, such as `Firefox.120.0.1.Patch`, join the precision set;
   - the parser is unchanged;
   - SC-1's guard text is amended to point at MW-6;
   - turning `unauthorised-copy` off doesn't restore cracked software.
3. **Junk reason:** add `cracked` in `crates/classify/src/junk.rs`.
4. **Upgraded stores (RP-DEF set `packs.defaults` once).** On a store RP-DEF already set up, this build must write the explicit `unauthorised-copy` off row itself, through the upgrade path in `upgrade.rs`. It already reads as off with no row, but write the row so the state is explicit. A fresh store has it on, with UC-1–3 and MW-6 applied and not pending. MW-6 in a malware pack that is already on stays in shadow until RP-3 and RP-6 ship, as RP-0 designed. Tests:
   - an upgraded store has `unauthorised-copy` off until the user turns it on;
   - a fresh store has it on and applied;
   - MW-6 is in shadow on an upgraded store where malware is on.
5. **Precision:** build the precision set under `crates/filter/tests/` from the release-parser golden corpus. Every must-not-hide name passes, and the PR reports precision for each rule.
6. **API and UI:**
   - the pack's rows in `crates/api/tests/golden/`;
   - the `PackSwitch` warning text in `crates/api/src/filters.rs` gains the sixth topic;
   - en and zh description keys in `web/src/lib/features/filters/messages/`;
   - the pack's section in the user guide's filters chapter, per the `user-docs` skill.
7. **#454 review nits (take them here):**
   - `docs/decisions/2026-11-05-RP-DEF.md`: fix the heading to (#454);
   - move the `fresh_marker` helper in `schema.rs` above the `versions_are_contiguous` doc comment;
   - rename or comment `crates/filter/tests/engine.rs` `a_fresh_store_loads_with_no_pack_on`, which cites the superseded §2.8;
   - the header of `web/src/lib/features/wizard/packs.svelte.ts` still says the wizard only turns packs on; correct it;
   - note that S-wizard's "never weakens" line is superseded;
   - check that the `serve --fixture` store rows are pack-judged now that packs are on for fresh stores.

## Own
- `crates/filter/src/packs/unauthorised_copy.rs` (new); its registration in `packs/mod.rs` and `manifest.rs`; MW-6 in the malware pack's rules; the upgrade.rs off-row step;
- `crates/classify/src/junk.rs`;
- the precision set and tests under `crates/filter/tests/`;
- the pack's api goldens; the `PackSwitch` text in `crates/api/src/filters.rs`; the filters message keys;
- the filters chapter section; `docs/decisions/2026-11-05-RP-UC.md`; the #454 nit files above.

**Overlaps:**
- RP-2 (running) adds one `mod` line to `packs/mod.rs` and owns `crates/api/src/filters.rs`, `engine.rs` and `load.rs`. Keep your edit to `api/filters.rs` to the `PackSwitch` text. Whichever PR merges later brings `main` in. Regenerate the goldens rather than hand-merging them.
- **Don't touch** `docs/legal/**`; LEGAL-1 owns it now. When you open the PR, message the runner, so LEGAL-1 or a follow-up drops the notice's "(planned)" marker for this pack.
- RP-1b runs after you (it also edits `packs/mod.rs`).

## Done when
On the branch:
- `cargo fmt --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- `cargo test --workspace`;
- in `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`.

Then `/security-review` on the diff. Open the PR against `main` with the title "RP-UC: The unauthorised-copy pack (UC-1 to UC-3) and MW-6 cracked software in the malware pack" and the label `full-ci`. Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the precision numbers, using the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round of fixes. No fixed sleeps.
