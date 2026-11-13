# Not done in the 2026-11-13 UI/UX review

Checks neither pass made. Each says what is unknown and how to close it. Nothing here has a finding yet; treat each as untested, not as passed.

## Layout and input

| Check | Why it matters | How to run it |
|---|---|---|
| 2560 px width | A §10.15 test width; wide screens show stretched rows and unused space first | `CELLS='2560:light' node scripts/sweep.mjs`, then look at Search, Insights and Settings |
| 1440 px across all screens | Only the drawer was checked there; it is the gap between 1366 and 1920 | `CELLS='1440:light' node scripts/sweep.mjs` |
| 768 and 1024 px, by eye | Only Search and Insights were looked at; the other 20 screens had automated checks only | Open the `@768-light` and `@1024-light` screenshots the sweep writes |
| 200% zoom and text-spacing overrides | §10.13 promises both | Chromium at 200% zoom at 1366 px (equals a 683 px layout with large text); apply the WCAG 1.4.12 text-spacing bookmarklet |
| Real touch devices | Histogram brush, drawer resize and table scroll were driven only by emulation | A phone and a tablet against a LAN install |
| A language longer than English | Only `en` and `zh` exist; toolbars and segmented controls have no slack | Pseudo-locale with 30–40% longer strings |

## Accessibility

| Check | Why it matters | How to run it |
|---|---|---|
| Screen reader | axe can't judge reading order, live-region timing or the drawer's focus moves | NVDA or VoiceOver through search → open → copy magnet, save an alert, the wizard |
| Keyboard-only wizard and Settings save | Part 1 walked the wizard by mouse | Tab through `/setup` and `/welcome`; save and discard a setting |
| Dialog and drawer focus return | Seen working in passing; not tested case by case | Open and close each dialog and the drawer from the keyboard; note where focus lands |

## Motion and geometry

| Check | Why it matters | How to run it |
|---|---|---|
| Drawer, sheet, popover and toast timings | Part 2 measured resting transitions only, not overlays while they move | Record the computed `transition-duration` while each is opening and closing |
| Dialog and sheet radius (16 px) | Not sampled: `measure.mjs` ran with no overlay open | Open a dialog and the phone filter sheet, then read `border-radius` |
| Layout shift on load | The first-paint shell is designed for none; not measured here | Lighthouse CI already gates CLS; confirm on fixture data |

## States and failure paths

| Check | Why it matters | How to run it |
|---|---|---|
| Server stops while the UI is open | The connection screen and its recovery were not seen | Stop the server mid-session; restart it |
| Session expires mid-edit | Unsaved Settings drafts should survive sign-in | Revoke the session from another browser with a dirty form open |
| A lazy chunk fails to load | Most screens load on first use | Block one `_app/immutable/chunks/*` request |
| Day one, small index | The "index is still growing" banner and "Idle" pill were seen only on the fixture | A fresh install for its first hour |
| Index rebuild, stale Insights, paused crawler | Designed in §10.4 and §10.9.6; the fixture shows none of them | Trigger each on a test install |
| Print layout for Insights | §10.9.6 | Print preview of `/insights` |

## Data

| Check | Why it matters | How to run it |
|---|---|---|
| The Fetch counter on a live crawl | The fixture gap explains "Fetch 0" (B4), but the live counter itself was never compared with the funnel | On a running install, compare the Fetch stage total with the funnel's fetched count for a completed day |
| The two-torrent gap (407 vs 409, I27) | Probably hidden types; not confirmed | Query the store for the range, grouped by type, with and without hidden types |
| Stage pages other than Fetch and Index | Discover, Understand and Serve were captured but not read | Open `insights-discover`, `insights-understand`, `insights-serve` screenshots; reconcile their KPIs |
| Performance budgets | §10.1 sizes and timings were not measured in either pass | `npx size-limit` and `npm run lhci` in `web/` |

## Install kinds

| Check | Why it matters | How to run it |
|---|---|---|
| Wizard and Settings on a real install | The fixture locks the DHT off and marks Network read-only, so those screens were seen in an unusual state | Docker and Windows installs, first run to first search |
| Updates, Companions, Workers screens with real state | All three were empty or switched off in the fixture | An install with an update available, one companion and one worker |
