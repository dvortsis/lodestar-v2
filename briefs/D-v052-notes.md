# D-v052-notes: first draft of the v0.5.2 release notes on main
**Model:** Sonnet (docs, material-composer). A fresh Opus reviewer follows. Runner brief, 2026-11-05T07:00Z.

## Why
v0.5.2 is the next release after v0.5.1. `release/v0.5.1` split from main on 2026-10-30, so main is now more than 400 merges ahead. No `docs/releases/v0.5.2.md` exists, and several merged slices left lines for it: the Legal Notice v3 shown once to v0.5.1 users, rule packs on by default for new installs, and the unauthorised-copy pack. A draft written now, while the details are fresh, saves the cut (P-v052-cut) from reconstructing them.

## Read first
- `.claude/skills/user-docs/SKILL.md`, then the owner's `material-composer` skill it names for release notes.
- `docs/releases/v0.5.1.md` on main and on `release/v0.5.1`, for the format, the 450-word cap and the tone.
- `docs/owner-decisions.md` §2.38–§2.41: what v0.5.2 carries and what moved there from v0.5.1.
- `git log --merges --first-parent origin/main --since=2026-10-30 --format='%s'`, and the `docs/decisions/2026-1*` files those PRs added.

## Do (branch `slice/D-v052-notes` from `origin/main`)
1. **Write `docs/releases/v0.5.2.md`**, using material-composer's release-notes format, with the same sections as v0.5.1. Every line must describe what a user of v0.5.1 gets or must do. Group the many merges into a few user-facing bullets.
   - **Legal Notice:** v3 is shown once at sign-in to v0.5.1 users. This lands with LEGAL-1 (#472); write it as "pending LEGAL-1" if #472 hasn't merged.
   - **Rule packs:**
     - on by default for new data folders only (RP-DEF); existing installs keep their choices and are offered the packs;
     - the unauthorised-copy pack (RP-UC, #469 if merged);
     - pack-rule overrides (RP-2, #465).
   - **Sources:**
     - the Settings → Sources screen (SRC-15a);
     - Sonarr/Radarr webhooks (SRC-11b);
     - provenance in the detail drawer (SRC-17, if merged);
     - plugins that are built but not yet available: Internet Archive, Sukebei and torrent caches stay off until their checks pass. Say they come later; don't list them as available.
   - **Search:** faster file-extension search at scale (X-search-files-3 and -4), without quoting gate numbers.
   - **Security:** the log redaction fixes.
   - **Upgrading:**
     - the `CLASSIFY_VERSION` 3→4 re-classification runs in the background after the upgrade, about a minute per million torrents, with nothing filtered differently meanwhile;
     - any store migration since V12 (check `crates/store` for the current version and say what it means for going back).
2. **Mark anything not yet merged as `<!-- pending: <slice> -->`.** The notes are a draft until P-v052-cut; say so in the front matter.
3. **Run material-composer's gate** and the 450-word cap.

## Own
`docs/releases/v0.5.2.md` only.

## Done when
The gate passes. Open the PR against `main` titled "D-v052-notes: first draft of the v0.5.2 release notes". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round.
