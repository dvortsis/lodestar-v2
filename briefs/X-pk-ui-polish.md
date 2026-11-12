# X-pk-ui-polish: PK-ui and PK-exc polish (runner brief, 2026-11-12)

**Model:** Sonnet (helper slice). **Branch:** `slice/X-pk-ui-polish`. **PR title:** `X-pk-ui-polish: PK-ui and PK-exc polish`.
Follow `.claude/skills/steward/SKILL.md`, `CLAUDE.md` and `runner/standards.md`. The runner merges; you never do.

## Why
Nits left over from the reviews of PK-ui and PK-exc (rv-701), held out of those PRs so they could merge. Each is small. Together they fix one misquote, one missing test, one screen-reader gap, one shared message key and two wrong guide links.

## Owns (approved for this slice)
- `docs/decisions/2026-11-11-PK-ui.md` and `docs/decisions/2026-11-11-PK-exc.md` (runner exception to "never edit another slice's file": those slices are merged and their sessions closed, and these are corrections to their own text)
- `web/src/lib/features/insights/overview/Exceptions.svelte` and its tests (`components.test.ts`)
- The insights messages files (`en.json`, `zh.json`) next to the insights screens: one new key
- `web/src/lib/features/wizard/StorageStep.svelte` and its test
- `crates/lodestar/src/windows/ports.rs`: **tests only**, no behaviour change
- Your decisions file `docs/decisions/2026-11-12-X-pk-ui-polish.md`

Don't touch `web/src/lib/features/restart/**`, `web/src/lib/shell/**` or `SettingsShell.svelte` (X-restart-banner, #718, is open there).

## Items
1. **N1, the decisions misquote:** in `2026-11-11-PK-ui.md` and/or `-PK-exc.md`, correct the misquoted line, remove the repeated sentence, and add the PR number to the heading (`# PK-ui decisions (#NNN)`). Find the numbers with `git log --oneline --merges -- <file>`.
2. **N3, an IPv4 `netsh` test** in `ports.rs`: the code builds `netsh int ipv4 …` and `netsh int ipv6 …` command lines. Add a unit test that pins the IPv4 command text (as the IPv6 one already is, if it is), for both the set and show forms. It's a test, not a refactor.
3. **N4, the storage-step warning banner announces:** the banner that shows `/system`'s `filesystem_warning` in `StorageStep.svelte` must be announced to screen readers. Render the live region (`role="status"`, polite) **empty first**, then fill it, so assistive tech hears the change. A test checks the region exists before the text arrives.
4. **N5, Insights stops reusing `settings_about_new_tab`:** `Exceptions.svelte:145` uses the About screen's "(opens in a new tab)" key. Give Insights its own key (same English text, plus zh), so the two screens can change independently. Update the test.
5. **Exceptions guide links:** in `Exceptions.svelte`, `data_folder` links to chapter 4 §4.6 and `windows_ports` to the §4.7 anchor of `docs/user/04-install-on-windows-and-linux.md`. Check the anchors exist on `main` (`grep -n '^## 4\.[67]' docs/user/04-install-on-windows-and-linux.md`) and build them the way the file already builds its other guide links. Add a test that asserts both hrefs.

## Budget
The wizard bundle is at 53.58 of 53.6 kB. Item 3 must not push it over. If it would, stop and tell the runner with the numbers (`npx size-limit`); don't raise the limit.

## Checks before pushing
- In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`.
- Rust: `cargo fmt --check`, `cargo +1.99.0 clippy -p lodestar --all-targets -- -D warnings` (plain `cargo clippy` if 1.99 is missing), `cargo test -p lodestar --bin lodestar ports` (or the filter that runs the `ports.rs` tests).
- No fixed sleeps in tests; wait on conditions with a deadline ≥ 10 s.
- Expect about 150–300 changed lines.

When CI is green, message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg): "ready <PR> <head sha>".
