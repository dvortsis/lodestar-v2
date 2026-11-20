# Not done in the 2026-11-13 UI/UX review

Checks neither pass made. Each says what is unknown and how to close it. Nothing here has a finding yet; treat each as untested, not as passed.

## Done later the same day

The owner asked for the cheap checks to be run. Results, with new finding ids L1–L5:

| Check | Result |
|---|---|
| 1440 px, all 44 screens | No new automated findings (no overflow, no console errors; the same three axe findings as elsewhere). The drawer still overlays at this width (I1). |
| 2560 px, all 44 screens | No overflow. **L3 (polish):** on Search the rows use about the left 35% of the screen while the toolbar's controls sit at the far right, about 1,700 px from the content, and the query bar stops at 1,050 px. Insights is centred with a width cap and reads well. |
| 200% zoom (683 px layout), 15 key screens | No overflow or clipped controls. **L4 (evidence for I2):** on Search no result is visible in the first screen; the first row starts about 650 px down a 450 px viewport. |
| Text-spacing overrides (WCAG 1.4.12), 15 key screens | No overflow. **L5 (polish):** histogram axis labels overlap and row facts clip at the right. The server's content security policy blocks a bookmarklet from injecting the test styles; browser extensions and user style sheets are unaffected. |
| Server stops mid-session | Handled well at first: the pill says "Reconnecting", a banner says "Lost contact with the server. Lodestar keeps trying and will update this page when it is back", and a typed query gets "Couldn't check the query … Retry". **L1 (important):** a page opened while the server was down shows "Something went wrong · Error 500" and stays there after the server returns (checked 33 s later), although the pill recovers. It was a network failure, not a 500. Fix: retry the failed page when the connection returns, and say "Couldn't reach Lodestar". |
| Session ends with an unsaved setting | **L2 (important):** pressing Save opens "Leave without saving?" with no word about having been signed out. "Save changes" in that dialog closes it and saves nothing. Only "Discard" leads to sign-in; afterwards the user is returned to the same screen, but the change is gone. §10.10.5 says unsaved work is never lost silently. Fix: say "You've been signed out. Sign in to save your change", and keep the draft across sign-in. |

Scripts: `scripts/check-zoom-and-text-spacing.mjs`, `scripts/check-server-stop-and-session-end.mjs`, `scripts/check-session-end-choices.mjs`; the two widths used `CELLS='1440:light,2560:light' node scripts/sweep.mjs`.

Still open from that list: only Search, Insights and Settings were read by eye at 1440 and 2560 px; the rest had automated checks only.

## Layout and input

| Check | Why it matters | How to run it |
|---|---|---|
| 768 and 1024 px, by eye | Only Search and Insights were looked at; the other 20 screens had automated checks only | Open the `@768-light` and `@1024-light` screenshots the sweep writes |
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
