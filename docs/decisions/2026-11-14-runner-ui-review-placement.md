# UI/UX review placement (runner, #875)

This file puts every finding of the 2026-11-13 UI/UX review in the plan. It builds on `2026-11-13-runner-ui-review-decisions.md` (#809), `2026-11-13-runner-tracks-q-u.md` and `2026-11-13-runner-owner-queue.md` (§2.69), and changes none of them.

The review now lives on `main` at `docs/review/2026-11-13-ui-ux/`. It is copied over from branch `claude/codebase-review-bugs-x9ff7p` (5c3e447a) as it stands, not merged, as #809 said it would be. The screenshots stay out (about 500 files); its `scripts/` folder regenerates them.

## Summary
- **About 125 distinct findings** come from 150 rows (01: 37, 02: 39, 03: 24, 04: 50) once duplicates are folded. The counts below are approximate: several rows are split across two homes, and the tables are the record.
- **Status:**
  - 11 are done;
  - 7 were already placed in a slice not yet started;
  - every other finding now has a home: a new slice, or a note on an existing one;
  - 4 need the owner (devices, a screen reader, real users);
  - 1 conflicts with owner decisions and stays declined (U1, restated as D7).
- **New slices:** 23, all in Track U (build-plan §0, steps 5 to 8). The Sonnet helpers follow the delegation policy. Opus builds the query-language parser change (X-units-typed), the setup-token check (X-setup-token-boot) and the child-safety rule screen (X-safety-toggles). The last two also get `/security-review`. So does X-alert-noun-text, which changes wording in `crates/integrations/src/notify/text.rs`: that file escapes untrusted names, so it stays on Opus.
- **The I1 + I3 slice** that #809 promised already exists as X-results-drawer-adaptive (added in #813). It hasn't started. It gains a note below.
- **Launch order:**
  - X-ui-guards owns `web/src/lib/**`, so every new web slice waits for it.
  - The slices that share a folder are chained through `deps`, so no two run at once.
  - X-ui-plan-r5 (the §10 text) lands before the screens that build on it.

## Withdrawn, corrected or duplicate findings
- **B4** was filed Blocking in 02 and re-ranked Important (#809): it was a fixture gap. The "Fetch 0" part of I9 is B4.
- **L4** (200% zoom) is extra evidence for I2, not a new finding.
- **Duplicates:**
  - I27 bullet 2 = P7. I26 repeats I8, I20 and P1. P15 repeats I11's page edges. I24's line heights repeat I19.
  - U3 = I13, U5 = B1, U21 = I4, U24 is part of P10, U32 = I5 + I18.
  - D3 = U10, D4 = U11, D5 = U25, D6 = U29, D7 = U1, D9 = U33, D10 is part of U12.
  - 02 §5.0 and 04 §7–8 restate earlier findings.
- **01's unchecked widths:** 01 says 2560 px and 200% zoom weren't checked. 03 ran them later: no overflow, and they produced L3 to L5.
- **Possibly fixture artefacts:** I18 and U32 (Network & VPN read-only, Companions 503). The fixture locks the DHT off, marks Network read-only and switches Companions off. X-settings-find confirms them on a real install before changing anything.
- **04 §8's eight proposals** were decided by the runner under §2.69 item 17: seven accepted, D7 declined.

## The runner's UX calls (§2.26, flagged per §2.19)
These are the "Decide" items in 02 §5.
- **Watch-folder retry (UP-2).** A failed file gets a Retry button in the upload results. It runs the same checks again. Moving the file back out of the folder still works. Why: the §2.14 reader may not know where the watch folder lives on the host.
- **Five badges on one row (ENR-11 and X-row-rail).** Safety and verification badges never give way. The rest collapse into a "+N" chip that opens the detail drawer, sources and enrichment first.
- **Worker names (WK-16).** The display name is editable. The worker's identity and keys don't change.
- **Update on a platform that can't update itself (WK-16).** No button that does nothing: show the version and the steps for that platform (pull the image on Docker or Unraid).
- **Undo on the intensity click (X-insights-story).** The change applies at once and a toast offers Undo for 10 s, so a misclick costs nothing.

## Owner items
- **U1 and D7 stay declined:** three required steps then a checklist, replacing the nine-step wizard. They conflict with §2.9, §2.13 and §2.39. The runner declined them under §2.69 item 17.4, and the owner's veto is open (owner queue). Their non-conflicting part goes ahead: the Settings Overview lists the steps the user skipped (X-settings-find).
- **Questions for the owner (owner queue):**
  - a first-run walk-through by the owner or someone like the intended user (04 §6a);
  - a screen-reader run and a real phone or tablet pass, or emulation accepted for 1.0 (03-c, 03-e, 02 §3f).

## Where each finding goes
"New" means a slice this PR adds. "Note" means text added to an existing slice's `notes`.

### 01-review.md
| id | finding | status | home |
|---|---|---|---|
| B1, B2 | Raw keys in the wizard; no Sign out | done | #811 |
| B3 | /insights scroller fails axe | done | #833; the fixture axe run is in X-ui-guards |
| I1 | Drawer covers the toolbar and rows at 1366–1463 px | placed | X-results-drawer-adaptive |
| I2 | ~300 px of controls above the first row on a phone | new | X-search-toolbar |
| I3 | Rows clip facts; Discover Newest has no time | placed and note | X-results-drawer-adaptive |
| I4 | Toggle labels read as status | new | X-safety-toggles |
| I5 | Filters & Safety shows rule codes only | new | X-safety-toggles |
| I6 | Seeders and Trackers facets "Not available yet" | new | X-row-rail |
| I7 | Discover Rising and Recently resolved always unavailable | new | X-narrow-states (hidden until a backend exists) |
| I8 | Histogram draws a year bin as one day | new | X-charts |
| I9 | Stage strip against funnel; mixed time bases | partial (#823) and new | X-insights-story |
| I10 | Internal values shown as labels | new | X-labels |
| I11 | Every page framed differently; missing h1 | new | X-page-frame |
| I12 | Settings field search, palette, badges | new | X-settings-find |
| I13 | Wrong setup token caught only after the password | new | X-setup-token-boot |
| I14 | A signed-out or unclaimed visitor sees Search first | new | X-setup-token-boot |
| I15 | Table and Cards can't tell versions apart; two view controls | new | X-search-toolbar |
| I16 | Detail thinner than Compare and the API; no back links | partial (SRC-17 #474; Related is GS-4) and new | X-detail-depth |
| I17 | Live log unreadable on a phone | new | X-narrow-states |
| I18 | Settings shows controls that do nothing | new (check on a real install first) | X-settings-find |
| I19 | Type sizes drift in one toolbar | new | X-ctl-geometry |
| I20 | Charts ignore category colours; loss bar unreadable | new | X-charts |
| P1 | Chart axes clip; ticks | new | X-charts |
| P2 | Dead breakpoint classes | placed | X-ui-guards |
| P3 | Settings nav selected state; repeated gear icons | new | X-settings-find |
| P4 | Boolean fields render an empty label row | new | X-settings-find |
| P5 | Top bar: pill, ⌘ glyph, overflow items, page shift | new | X-shell-home |
| P6 | Empty search state | new | X-shell-home |
| P7 | Count mismatch; no "N hidden · Show" | new | X-search-toolbar |
| P8 | Touch targets under 44 px | new | X-ctl-geometry |
| P9 | Fixed-height empty states; wizard step list | new | X-narrow-states; the step list in X-first-run |
| P10 | Copy (12 items) | new, split | query bar: X-zero-rescue; cohort copy: X-insights-story; About: X-narrow-states; Ebook and Video wording: X-labels; Torznab labels: X-client-handoff |
| P11 | Compare "Differences only" | new | X-detail-depth |
| P12 | Narrow layouts: rail sheet, Logging control, Discover tabs | new | X-narrow-states |
| P13 | About: external-link icon, absolute times | new | X-narrow-states |
| P14 | Insights order; no import step in the wizard | new | X-insights-story; X-first-run |

### 02-skills-pass-and-future-modules.md
| id | finding | status | home |
|---|---|---|---|
| B4 | Fetch 0 against funnel 413 | done (#823) | the live-crawl check: note on X-test-vm |
| I21 | Mixed icon weights; 14 px icons | partial (weights in X-ui-guards) and new | X-ctl-geometry (16 px minimum) |
| I22 | Form controls ignore reduced motion | new | X-ctl-geometry |
| I23 | Six control heights | new | X-ctl-geometry |
| I24 | Chart font, prose width, tabular numbers | new | X-charts (font); X-ctl-geometry |
| I25 | Insights reads as equal-weight cards | new | X-insights-story |
| I26 | Chart-by-chart changes | new | X-charts; detail composition in X-detail-depth |
| I27 | 407 against 409 group sums | new | X-charts (Opus checks the store query) |
| I28 | Placeholders as labels | new | X-detail-depth (file filter); X-narrow-states (logs) |
| P15 | Near-miss page edges | new (dup I11) | X-page-frame |
| P16 | Card overuse on Insights | new | X-insights-story |
| P17 | Three primary buttons on Security | new | X-settings-find |
| §3a | CI blind to overflow; baselines keep clipped rows | partial and note | X-results-drawer-adaptive refreshes the baselines |
| §3b | No test width between 1366 and 1920 | note | X-results-drawer-adaptive adds 1440 |
| §3c | Two languages tried | new | X-ui-checks |
| §3d | Day one | new and note | X-first-run; X-test-vm |
| §3e | Failure paths | partial (#842; X-session-draft-keep) and new | X-ui-checks (chunk-load failure) |
| §3f | Touch on a real screen | owner | owner queue |
| §3g | Sources will triple | done | #816 |
| §4a | Three lint rules | partial (2 in X-ui-guards) and new | X-ctl-geometry (control heights) |
| §4b | Fixture axe and screenshots, 5 screens | partial and new | X-ui-checks (screenshots) |
| §4c | Click a row badge to filter | new | X-row-rail |
| §4d | Recent searches on the empty state | new | X-shell-home |
| §4e | "Differences only" | new (dup P11) | X-detail-depth |
| §5.1 | Version grouping UI | partial and notes | GS-3, GS-4, GS-5 |
| §5.2 | Regex in the query bar | note and new | RX-1; RX-2 |
| §5.3 | Upload and watch folder | note | UP-2 |
| §5.4 | Usenet rows and Send | note | UN-4 |
| §5.5 | Enrichment UI | notes and new | ENR-11, ENR-debrid, the ENR plug-in slices; "Not this one" and ID links in X-detail-depth |
| §5.6 | Stremio | note | OUT-3 |
| §5.7 | Workers and placement | notes | WK-16, WK-17, PL-7a, PL-7b |
| §5.8 | More sources | notes | IRC-2, NOS-3, LEM-1, CMP-12, ADH-0 |
| §5.9 | The 1.0 design gate | notes | M8-DR, G-v1-design, G-v1-ui |
| §6.1–6.6 | The owner's decisions | done or placed | #809 |

### 03-not-done.md
The review doesn't number 03's open checks; 03-a to 03-t letter them in reading order, starting with the "Still open" sentence.

| id | check | status | home |
|---|---|---|---|
| L1 | Error 500 sticks after the server returns | done | #842, #844 |
| L2 | Session end loses an unsaved setting | placed and note | X-session-draft-keep |
| L3 | 2560 px: toolbar far from content | new | X-search-toolbar |
| L4 | 200% zoom | counted under I2 | X-search-toolbar |
| L5 | Text spacing: labels overlap, facts clip | new and note | X-charts; X-results-drawer-adaptive |
| 03-a, 03-b | Other screens at 768, 1024, 1440 and 2560 | note | M8-DR |
| 03-c | Real touch devices | owner | owner queue |
| 03-d | A language longer than English | new | X-ui-checks (pseudo-locale) |
| 03-e | Screen reader | owner | owner queue |
| 03-f | Keyboard-only wizard and Settings save | partial (wizard spec exists) and new | X-ui-checks |
| 03-g, 03-h, 03-i | Focus return, overlay timings, dialog radius | new | X-ui-checks |
| 03-j | Layout shift | placed (lhci) and new | X-ui-checks confirms it on the fixture |
| 03-k | A lazy chunk fails to load | new | X-ui-checks |
| 03-l, 03-o, 03-s | Day one, the live Fetch counter, a real install | note | X-test-vm |
| 03-m | Rebuild, stale and paused states | new | X-ui-checks |
| 03-n, 03-q | Print; the other stage pages reconciled | new | X-insights-story |
| 03-p | 407 against 409 | new | X-charts |
| 03-r | Performance budgets | placed | size-limit and lhci in CI; G-v1-ui |
| 03-t | Updates, Companions and Workers with real state | notes | WK-16, M8-DR |

### 04-plan-and-flow-review-moderate-user.md
| id | finding | status | home |
|---|---|---|---|
| U1, D7 | Three required steps plus a checklist | declined (§2.69 17.4) | non-conflicting part in X-settings-find |
| U2 | Token-from-log help open by default | new | X-first-run |
| U3 | Token checked on Continue | dup I13 | X-setup-token-boot |
| U4 | Network step leads with safety | new | X-first-run |
| U5 | Raw keys | dup B1, done | #811 |
| U6 | "What happens next" | new | X-first-run |
| U7 | Offer import; say when the index is useful | new | X-first-run (a guide link; the in-app import waits on B-import-server-2) |
| U8, U9 | "Did you mean"; natural-phrasing chips | new | X-zero-rescue |
| U10, U11, D3, D4 | Facet order; two-tier row | new | X-row-rail |
| U12 | Accept GB as typed | new | X-units-typed |
| U13, U14 | Open in torrent client; Technical details | new | X-detail-depth |
| U15 | Typed From and To dates | new | X-row-rail |
| U16 | Builder shows raw field codes | new | X-labels |
| U17 to U20 | "Alert" everywhere; create from a search; toast; notifications link | new | X-ui-plan-r5 (glossary); X-alert-noun |
| U21 | "Paused" switch | dup I4 | X-safety-toggles |
| U22 to U24 | Download client form; test on Save; two Torznab fields | new | X-client-handoff |
| U25, D5 | Plain health sentence | new | X-insights-story |
| U26 | Undo for the intensity click | new (runner call above) | X-insights-story |
| U27 | Pill wording | new | X-shell-home |
| U28 | Insights in UTC, not the chosen zone | new | X-labels |
| U29, D6 | Settings Overview "start here" | new | X-settings-find; X-ui-plan-r5 |
| U30, U31, D8, D10 | Help links, /help, "What's this?", "≈" | new | X-help; X-ui-plan-r5 |
| U32 | Safety screens; Network & VPN | dup I5, I18 | X-safety-toggles; X-settings-find |
| U33, D9 | First-use prompt for `o` | new | X-client-handoff |
| D1, D2 | Name the reader in §10.0; reorder §10.4 | new | X-ui-plan-r5 |
| D11 | Search empty state as home | new | X-shell-home; X-ui-plan-r5 |
| §6a | Nobody from the audience has used it | owner | owner queue |
| §6b | First hour on a live install | note | X-test-vm |
| §6c | Phone flow: alerts and Send | new | X-ui-checks |
| §6d | "Something's wrong" path | new | X-help |
| §6e | Updates and backups off in the fixture | new | X-ui-checks (a fixture variant) |
| §6f | Words in notifications, digest, RSS, /status | new | X-alert-noun-text (Opus) |
