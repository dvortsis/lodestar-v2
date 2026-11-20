# Lodestar UI/UX review, 2026-11-13: part 1

Part 2 (`02-skills-pass-and-future-modules.md`) adds the pass with all 15 design skills loaded, and the plan for unbuilt modules. `03-not-done.md` lists what neither pass checked.

Reviewed `main` at `55e155a9` against `docs/architecture.md` §10, `docs/design-tokens.md` and `docs/ui-glossary.md`, using the `design-review` skill's format.

## How it was checked

- The real server (`lodestar serve --fixture`, UI embedded), not the OpenAPI mock.
- 44 routes at 320 and 390 px (touch), all at 1366 px, 22 key routes at 768 and 1024 px, Riverstone light and dark, plus `zh-CN`. Each cell: full-page screenshot, axe (WCAG 2.2 AA + best-practice), overflow, target-size and console checks.
- Interaction passes at 1366 and 390 px: query typing, facets, drawer, table, cards, select, palette, shortcut sheet, overflow menu, filter sheet, dialogs, settings save bar, leave guard, theme picker.
- First run on a fresh, unclaimed instance: Legal Notice, setup token, password, and all nine wizard steps.

Not checked: the wizard and Settings on a non-fixture install, 2560 px, 200% zoom, real-device touch, and the numbers themselves (see I9: that needs `analytics-validation`). The 768 and 1024 px screens other than Search and Insights had automated checks only. Some gaps below may already be planned slices; they're marked "check plan".

## Blocking

**B1. The first-run wizard shows raw config keys as field labels.**
- What: `/welcome` → Crawl intensity shows the label `intensity` and options `low / balanced / aggressive`. Storage & retention shows `archive_raw` and `mmap_mib` with no help text. Field menus are named "Options for archive_raw". Settings shows the same fields as "Crawl intensity" and "Keep raw metadata" with help.
- Why: this is the first screen a new owner fills in, and owner decision §2.14 sets a non-technical reader as the bar.
- Fix: `wizard/FieldsStep.svelte` passes bare keys to `SettingField`; give it the same label and help message keys the Settings screens use. Add a test that no rendered label equals its setting key.

**B2. There is no way to sign out.**
- What: `features/auth/LogoutButton.svelte` exists but nothing imports it. The overflow menu has only Logs and About; Security offers only "Sign out other sessions".
- Why: on a shared or borrowed device the session can't be ended.
- Fix: add Sign out to the overflow menu and to Settings → Security → Sessions.

**B3. Insights fails a serious axe rule at every width.**
- What: `.ins-cohorts` on `/insights` is a scrollable region with no keyboard access (`scrollable-region-focusable`, WCAG 2.1.1). CI's mock probably has too few cohorts to overflow.
- Fix: `tabindex="0"`, `role="region"` and a label on the scroller, or drop the inner scroll. Add a fixture-backed axe run for `/insights`.

## Important

**I1. At 1366–1463 px the detail drawer covers the toolbar and most of every row.**
- What: with the drawer open at 1366 and 1440, the density, List/Table, Select and Save search controls and "Load more" are cut off, and about 57% of each row is hidden; row facts clip mid-word. Only at ≥ 1464 px does it become a third column.
- Why: §10.15 says xx-large (1366–1919) has a persistent drawer, but the §10.2 tracks can't fit: 264 + 560 + 480 + gaps and padding = 1,384 px. `DetailHostBody.svelte` falls back to overlay.
- Fix (needs a decision): either let the results track shrink to about 420 px while the drawer is open so it goes inline from 1366, or start the overlay below the toolbar and switch covered rows to the narrow layout.

**I2. On a phone, about 300 px of controls sit above the first result.**
- What: at 390 × 800 the first row starts at y ≈ 515: Filters button, count, Sort, density, List/Table/Select/Save search, then the "index is still growing" banner.
- Fix: one row (Filters, Sort, a View menu), and make the banner dismissible or show it once.

**I3. Result rows clip or drop facts instead of adapting.**
- What: at 768 and 1024 px the facts line is cut mid-word ("3 hours ag", "last w") while about 150 px to its right is empty. At 390 px the row shows only title, size and seeders: no type badge, no discovered time, no file count. Discover's "Newest" list on a phone therefore shows no time at all.
- Fix: container queries per §10.15; wrap facts to a third line before clipping; in Discover put discovered time first.

**I4. Toggle labels read as status.**
- What: every alert has a switch labelled "Paused", off for active alerts. Every source has a switch labelled "On", shown off, next to a status reading "Off".
- Fix: name the switch for what on means and keep it true ("Active", "Enabled"), or show the state word beside it.

**I5. Filters & Safety identifies rules only by code.**
- What: rows read `CS-2a`, `MW-4@1`, `LU-1@1` with buttons "Turn off CS-2a"; pack names are lowercase mono (`unauthorised-copy`); a row labelled "Version" lists rule ids. The confirm dialog is titled "Change CS-2a" and never says what the rule checks, and turning off a child-safety check uses the ordinary primary button. The screen is 4,573 px long at 390 px.
- Why: the wizard's Rule packs step already has plain descriptions ("Malware: hides programs disguised as movies or music"); this screen doesn't use them.
- Fix: a name and one-line description per rule and pack, the description in the dialog, the danger style for loosening a child-safety rule, and collapsed packs by default.

**I6. The Seeders and Trackers reporting facets say "Not available yet" while the data is on screen.**
- What: both groups read "Seeder and tracker counts arrive with swarm indexing", but rows show seeders and `seeders:>=5` and `trackers:>=3` work as typed filters (`/facets` returns `unavailable: ["seeders","trackers"]`).
- Fix: offer the §10.4 presets (≥ 1, ≥ 5, ≥ 50, include never-scraped) as plain clauses without counts, or reword.

**I7. Two of Discover's three sections are permanently "Not available".**
- What: Rising and Recently resolved; the second says Lodestar doesn't record the data.
- Fix: hide them until the backend exists, or build them (§10.7). Check plan.

**I8. The Discovered histogram draws a year-wide bin as one day.**
- What: the first bin runs 2025-10-08 → 2026-10-09 and is drawn the same width as the daily bins, with "Oct 8, 2025" as the axis start and "Oct 26, 2026" as the midpoint.
- Why: §10.12 requires explicit, consistent bins.
- Fix: draw it apart as "Earlier", or label it.

**I9. The Insights stage strip disagrees with the funnel and mixes time bases.**
- What: the strip says "Fetch · 0 · Metadata fetched" while the funnel below says "Metadata fetched 413". "Index 2.2K" is all-time and sits beside "Understand 409" for seven days, unlabelled. The strip also repeats the KPI row under it.
- Fix: run `analytics-validation` on the strip; label each tile's time base (§10.9.3).

**I10. Internal values are shown as labels.**
- What: Source `dht`, `bitmagnet`; "Found by: dht"; languages `fr`, `it`, `zh`, `ja`; `h264`, `dts-hd-ma`, `bluray`, `web-dl`, `hdr` in facets, Compare and the detail; release groups lowercased in facets but uppercase in names; "Current theme: riverstone".
- Fix: one label formatter per field, as §10.12 does for metrics.

**I11. Every destination frames its page differently.**
- What: left gutter 16 px (Search, Compare), 24 (Discover, Insights), 48 (Alerts), a centred column (Detail). Page headings are 28 px on Search, Discover, Alerts and Compare, 20 px on Settings and Detail, and not visible on Insights. Settings → Companions and Outputs have no `h1` (axe `page-has-heading-one`). The Keyboard screen is indented 16 px more than its siblings. Settings lead text is 13, 14 or 16 px by screen, and Security and Outputs have sub-headings larger than their section headings.
- Fix: one page frame and one page header primitive; one settings screen header.

**I12. Finding a setting doesn't work as planned.**
- What: "Find a section" filters screen names only ("port" → Crawler, Network & VPN). The command palette has no settings or theme commands ("theme" → no match). Navigation items carry no status badges, and on a phone "Needs attention" sits below 21 links.
- Fix: §10.10.3 field search and palette entries; badges; Needs attention first on narrow screens. Check plan.

**I13. A wrong setup token isn't rejected until after the password.**
- What: `0000-0000-0000-0000` advances to Admin password; the error appears only after submitting a password, and the user is sent back.
- Fix: check the token on Continue (rate-limited), or put token and password on one step.

**I14. A fresh or signed-out visitor sees the Search page until they touch it.**
- What: `/` on an unclaimed instance renders Search; the first click jumps to `/setup`. A signed-out `/search` does the same before going to `/login`.
- Fix: one session check at boot on non-bare routes.

**I15. Table and Cards can't tell versions apart, and the view controls overlap.**
- What: for `lantern type:movie` both show six identical "The Lantern Keeper · Movie" entries: no raw name, resolution or group. Cards have uneven heights with about 90 px dead gaps, a wrapped backfill icon, and no visual content. Density (Comfortable, Compact, Cards) and List/Table are two controls whose combinations don't all mean something.
- Fix: default Raw name and Resolution columns; raw name on cards; one view switcher.

**I16. The detail view is thinner than Compare and than its API.**
- What: Parsed metadata shows only title, type and year, while Compare shows resolution, source, codecs, channels, HDR, container and group. There is no Swarm section, though `/torrents/{hash}` returns `swarm_history`, `swarm_trackers`, `dht_peers` and `completed`. Secondary actions are loose buttons, not the §10.5 menu. At 390 px the title wraps to three lines beside the buttons. `/t/{hash}` and `/alerts/{id}` have no back link.
- Fix: check plan (GS-4, SRC-17, ENR slices cover some); add the back links and the actions menu.

**I17. The live log is unreadable on a phone.**
- What: at 390 px the message column shows about four characters ("Pipe…"); the first line sits under the banner; the component filters are plain words with no visible on/off state.
- Fix: wrap by default below 640 px, drop or stack the component column, give the filters a toggle look.

**I18. Settings shows things that do nothing.**
- What: Companions is in the navigation but answers 503 ("switched off in this copy"), against §10.10.2. Network & VPN is wholly read-only. "uTP (not active yet)" is a switch that changes nothing. "Index" (Search & ranking) and "Cadence" (Trackers) are headings with only an Advanced disclosure under them.
- Fix: hide screens and fields that can't act; hide empty groups.

**I19. Type sizes drift inside one toolbar.**
- What: dense buttons are 12 px semibold (including the primary "Copy magnet"), segmented controls 14 px, the "Sort" label 16 px. Line heights are off-token in many places (14/21, 13/19.5, 12/18).
- Fix: dense Button at `text-small` or `text-body` weight 500; set line-height with the size token.

**I20. Insights charts don't use the category colours, and the loss bar can't be read.**
- What: "Searchable by group" draws Video, Audio, Images in one blue, though §10.2 says the category colours are the same in charts as in rows. The loss-reasons stacked bar is one colour, so segments are identifiable only by a `title` tooltip.
- Fix: `--data-N` per group; drop the stacked bar or make it sorted bars.

## Polish

- **P1. Chart axis.** The first x tick is clipped ("ov 6, 2026, 00:00") at 390 and 1366 px; ticks repeat "2026, 00:00"; the legend ends in a dangling colon.
- **P2. Dead classes.** `sm:grid-cols-2` (`InstallDialog.svelte:48`, `Templates.svelte:119`, `ComponentLevels.svelte:31`) and `xl:p-6` (`SettingsShell.svelte:32`) use breakpoints the theme removed; they're absent from the built CSS.
- **P3. Settings navigation.** The current item's background equals the page background, so only weight changes (use `--color-selected`). Seven of 22 icons are the same gear.
- **P4. Boolean fields** render an empty label row: a blank line with only the ⋯ menu, or a lone changed-dot.
- **P5. Top bar.** The pill reads "Status unknown" for a second or two after each load; the palette button is a ⌘ glyph on every platform, touch included; the overflow menu lacks Import, Backup and the Syntax guide, and opening it shifts the page about 8 px.
- **P6. Empty search.** At desktop the block sits 290 px in beside an empty rail; on a phone there's a 100 px blank band; no recent searches or hourly rate (§10.4); "Clear all" shows with nothing to clear in browse mode.
- **P7. Counts.** "2,196 results", "2,208 torrents" and "2.2K" appear together with no "N hidden · Show".
- **P8. Touch targets under 44 px.** Row actions 44 × 32, "Back to Settings" 24 high, log header links 18 high, About links 24 high.
- **P9. Fixed-height empties.** Alert detail and table view leave a large blank card. The wizard's 12-step list is five lines of "· Not yet" with inconsistent numbering.
- **P10. Copy.** "Can't read this query: can't read…"; "11 in other values"; cohort rows "11  1 searchable"; "Off· 0 enabled"; "Interface build 0.0.0"; the tagline twice on About; "Ebook" vs "E-books"; "Removing seed gives 3" vs "Remove Resolution: 1080p"; two different "Torznab address" fields; "100% video" vs "100% Video"; autocomplete rows all prefixed "Value"; an unfinished token is flagged as an error while its suggestions are open.
- **P11. Compare** lists identical attributes at full weight; 4,613 px on a phone. Add "Differences only".
- **P12. Narrow layouts.** The filter rail stays 264 px inside a 390 px sheet; Logging's Verbosity control wraps to three rows; Discover's tabs overflow with no cue.
- **P13. About** links lack the external-link icon and new-tab note (§10.10.13). Absolute times are in a `title` only.
- **P14. Against the plan, to confirm.** Insights shows the funnel before Movement and has no trend 8 + exceptions 4 row (§10.9.2, §10.9.6); the wizard has no bitmagnet import step (§10.10.10).

## Strengths

- No horizontal overflow, no unexpected console errors, and only three axe findings across about 260 screens from 320 to 1920 px.
- States are designed, not defaulted: zero-result recovery with counts, parse errors with underline and text, the staged filter sheet with a live count, the leave guard and save bar, a specific delete confirmation.
- Null is never shown as zero; estimates carry "≈"; times say UTC.
- The Chinese catalogue is complete, dark mode holds contrast, and focus rings are visible everywhere tried.

## Highest-leverage change

Fix B1 and B2 first: both are small and both hit every new owner. After that, I1 to I3 together (how Search uses width at phone, tablet and laptop sizes), since Search is the home screen and all three come from the row and toolbar not adapting to their container.
