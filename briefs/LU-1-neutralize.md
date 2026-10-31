# LU-1-neutralize: the LU-1 pack rule honours Neutralize terms
**Model:** Opus: content safety and the filter engine. Runner brief, 2026-10-31. This is the follow-up #240 recorded (`docs/decisions/2026-10-30-F-filters-packs.md`, "Not changed: LU-1's `lure-file` reason"). **Small PR:** aim for ≲ 500 changed lines.

## Why
§20.5.5: a CSX-5 *Neutralize term* "stops counting as evidence in every non-floor core rule and pack rule". #240 did this for the pack rules that read keyword lists (MW-3, LU-2, LU-4, SC-1). LU-1 (`crates/filter/src/packs.rs`, about line 349: `junk(LureFile) && junk(ArchiveForMedia)`) still reads the classifier's `lure-file` junk reason. That reason comes from `is_lure_name` in `crates/classify/src/junk.rs`, and Neutralize doesn't reach it. So a neutralized `password` still triggers LU-1. That fails safe, but it isn't what §20.5.5 says.

## Read first
- `docs/decisions/2026-10-30-F-filters-packs.md`: the whole file. **Copy its approach:**
  - token-level blanking with `matcher::neutral_token`, keeping positions;
  - phrases broken only where a blanked word stands;
  - guards read the torrent unchanged;
  - neutralizing only ever removes a match.
- `crates/filter/src/packs.rs` (`Ctx::evidence`, MW-3's lure-name handling) and `crates/classify/src/junk.rs` (`is_lure_name`, `LURES`).
- §20.5.5, §20.6 (packs share the classifier's reasons so the two never disagree) and §20.10–20.11 in `docs/architecture.md`.
- `runner/standards.md`.

## Do
1. **Give the packs the lure evidence, not just the flag.** The junk score itself is not a filter rule and stays exactly as it is. Neutralize doesn't change the junk score, and the decisions file must say so.
   - **The preferred shape:** expose, from `lodestar-classify`, which small files matched which `LURES` entry (for example, a `lure_hits` list, or a pure `lure_matches(name) -> impl Iterator<Item = &'static str>`).
   - LU-1 then requires at least one lure hit that survives the torrent's neutral set, evaluated with the same token-and-position rules MW-3 uses, plus `ArchiveForMedia` unchanged.
   - The size threshold (`< 64 KiB`) and the `LURES` list stay single-sourced in classify, so §20.6's "never disagree" holds when nothing is neutralized.
   - If an alternative is cleaner, use it and justify it in the decisions file.
2. **Matching LURES phrases** (`click here`, `how to open`, `visit www`): a phrase is broken only where a neutralized word stands, exactly as #240's phrase rule does. `password` and `passwd` are whole words, so neutralizing `pass` doesn't touch them.
   - Watch for a pre-existing mismatch: `is_lure_name` uses substring `contains`, so `mypassword.txt` matches `password`. With token rules, `mypassword` is one word that isn't `password`.
   - **Don't narrow the classifier's own matching.** For LU-1's neutralize check, a substring hit inside a joined word can't be neutralized by the bare word, so it stays as evidence. That fails safe, consistent with "joined words can't borrow a neutralized word".
   - Spell this out in the decisions file and cover it with a test.
3. **Tests:**
   - per rule: a neutralized lure word that was LU-1's only lure evidence stops LU-1; another lure file in the same torrent keeps it; `ArchiveForMedia` is still required; a joined word stays evidence; a shadow-mode entry neutralizes nothing;
   - extend the property test `neutralizing_removes_only_sole_evidence` to include LU-1 (lure files plus archive-for-media torrents);
   - an engine-level case in `tests/semantics.rs`;
   - a test that the junk score and reasons don't change when Neutralize entries exist;
   - no golden or intent fixture changes. If one must change, stop and tell the runner.
4. **Budget:** the filter budget (§20.8) holds. Put the numbers in the PR, as #240 did.
5. **Decisions file:** `docs/decisions/<date>-LU-1-neutralize.md`. Also update #240's decisions file's "Not changed" line to point to it.

## Own
`crates/classify/src/junk.rs` (a public accessor only; no change to scoring), `crates/filter/src/packs.rs`, the filter tests, and the two decisions files.

## Done when
- fmt, workspace clippy `-D warnings` and `cargo test --workspace` pass;
- the filter budget job's local equivalent passes, if one exists;
- `/security-review` is clean on your diff.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA. Never merge. Work frugally: the account is under a usage warning. Push once per round of fixes.
