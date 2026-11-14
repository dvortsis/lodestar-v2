# Part 4: the UI plan and the current flows, for a moderately technical user

2026-11-13, `main` at `55e155a9`. Parts 1 and 2 judged the screens against the plan. This part judges the plan itself (`docs/architecture.md` §10) and the flows as built, against one reader.

## 1. Who this is for

The owner's instruction for this review: optimise for a moderately technical user, not an expert.

- **Can:** install an app from the Unraid app store, follow a guide, use a torrent client, paste a magnet link.
- **Half-knows:** port forwarding, VPNs, what a tracker is.
- **Doesn't know:** the DHT, query syntax, what "cohort", "yield", "unverified" or "trackers reporting" mean, or why a number has "≈" in front.
- **Wants:** to find a thing, get it into their client, and know Lodestar is working and safe.

`docs/owner-decisions.md` §2.14 already sets this reader for the documentation. **The UI plan names no reader at all** (finding D1), and that is the root of most of what follows.

## 2. How it was checked

- Read §10 for what it assumes the user knows.
- Walked eight journeys on the real server (fixture data) at 1366 px: first run, find and grab, wrong-but-plausible queries, natural phrasing and the builder, save an alert, add a download client, read Insights, change crawl intensity.
- Counted expert terms in the 3,911 English strings.
- Not done: no real person from the audience was watched; the first hour on a live install was not walked; Send to a download client was not exercised (the fixture has none).

## 3. The short version

- **Plain words work.** Typing `lantern keeper 1080p` finds it; Copy magnet confirms with a toast; natural phrasing turns "4k movies from 2020 over 10gb" into four correct filters. The foundations suit this reader.
- **The product then behaves as if the reader were an expert.** The best beginner feature is a small quiet button; near-miss syntax fails silently; the default filters lead with internals; Insights speaks in cohorts and percentiles; Settings is 22 equal screens; nothing in the app links to help.
- **The first evening is the biggest risk.** Twelve steps to the first search, a token to fetch from a container log, a network step that tells the reader to forward a router port, then an almost empty index.

## 4. Journeys

### 4.1 First run

| ID | What happens | Why it's a problem for this reader | Change |
|---|---|---|---|
| U1 | Twelve steps before the first search: Legal Notice, token, password, then nine wizard steps | Each step is a chance to stop; several ask things they can't yet judge | Three required steps (notice, token, password), then Search with a "Finish setting up" checklist that carries the rest |
| U2 | The setup token must be copied from the container log | Finding a Docker log is the hardest thing asked of them; the "Where to find the log" help is collapsed | Open that help by default, with per-platform steps and a screenshot (Unraid: the log icon on the container row) |
| U3 | A wrong token is accepted, then rejected after the password (I13) | They redo two steps without knowing which was wrong | Check on Continue |
| U4 | Network check says "forward the DHT port (UDP) on your router"; a VPN is a second thought; "Other nodes see you as 203.0.113.7:6881" has no explanation | Port forwarding is offered as the fix with no word on what it exposes. §2.14 asks for guidance to a safe setup | Lead with what others can see and why a VPN is the safe choice; make "works either way, just slower" the headline; move port forwarding behind "I know what this means" |
| U5 | Crawl intensity and Storage show raw keys (B1) | Unreadable | Part 1's fix |
| U6 | "After every start … sampling waits 6 hours" appears on How it works and Done | It reads as "nothing for six hours" with no sign of progress | A "What happens next" panel after setup: live count, what to expect in the first hour and first day, and the import option |
| U7 | No step offers an import or a look at example results | The first search on an empty index teaches nothing | Offer the bitmagnet import (plan step 6, missing) and say plainly when the index will be useful |

### 4.2 Find it and grab it

| ID | What happens | Why it's a problem | Change |
|---|---|---|---|
| U8 | `seeders>5` and `size > 4gb` return "No torrents match. Try fewer or different words." | They used a filter, not words; the advice is wrong and the cause is hidden | On zero results, run the natural-phrasing parser and offer "Did you mean `seeders:>5`?" as a one-click fix |
| U9 | Natural phrasing is a small quiet button that appears on focus | The feature that best fits this reader is the hardest to find | When the text parses as natural phrasing and plain search finds little, show its chips unasked, with Apply |
| U10 | The rail's second and third groups are "Source: dht" and "Show unverified"; Seeders says "Not available yet" (I6) | Internals first, the filter they want missing | Default order: Type, Resolution, Size, Seeders, Language, Year, Discovered. Source, Unverified, Extension, Trackers and the flags go under More filters |
| U11 | Rows read "↑ 512 ↓ 48  50 / 56 · First seen during backfill · Hybrid · v2" | Four unlabelled numbers and three terms of art per row | One availability fact in words and number ("512 seeders"); trackers, backfill, v2 and hybrid move to the drawer |
| U12 | Sizes are "43.0 GiB"; natural phrasing turns "10gb" into `size:>=10GiB` | Most people say GB; 10 GB and 10 GiB differ by 7% | Keep IEC as the default (§10.12) but accept and echo what was typed; consider a units preference. Owner's call |
| U13 | "Open magnet" is a quiet button under the raw name; pressing `s` with no client does nothing | Handing the torrent to the desktop client is the simplest path for someone without a web client, and it is the least visible | Rename to "Open in torrent client" and place it beside Copy magnet. With no client set up, show "Send to a download client · Set up" once |
| U14 | The detail view lists Piece size, Junk 0, Swarm data, "Found by: dht" | Facts for experts ahead of facts for choosing | Lead with what helps a choice (quality, size, availability, what's inside); put the rest under "Technical details" |
| U15 | Custom date range offers only "drag across the histogram" | No way to type two dates | Add From and To date fields |
| U16 | The Builder's fields are `res`, `name`, `ext` | Raw field codes | Field labels from the glossary, with the code as secondary text |

### 4.3 Save an alert

| ID | What happens | Why it's a problem | Change |
|---|---|---|---|
| U17 | Three names for one thing: "Save search" (button and dialog), "Alerts" (page), "New alert" (button) | They can't tell these are the same | One noun, "alert": "Create alert" everywhere |
| U18 | "New alert" on the Alerts page opens a dialog with an empty Query field | They must write query syntax blind, with no preview | Open Search with a "Create alert from this search" bar, or put a live match count and the natural-phrasing parser in the dialog |
| U19 | After saving from Search nothing visible happens: no toast, no link | They don't know it worked or where it went | Toast: "Alert created · View" |
| U20 | Channels offered are "In the app" and "RSS feed" only, with no link to add others | Phone notifications are the point of an alert | "Get these on your phone: set up notifications" linking to Settings → Notifications |
| U21 | Switch labelled "Paused" (I4) | Reads as the state | Part 1's fix |

### 4.4 Add a download client

| ID | What happens | Why it's a problem | Change |
|---|---|---|---|
| U22 | A new card "Client 1" with a red "Remove client", "Up", "Down", and fields Name, Client, Address, User name, Category, Save path | Only Address has help; Category and Save path are unexplained; the red button dominates an empty form | Help under every field; hide Category and Save path under "Optional"; quiet Remove until the client is saved |
| U23 | Test connection and Save are separate, and Save is staged for the whole screen | They can save a client that doesn't work | Test on Save by default; show "Connected to qBittorrent 5.0.2" before the save completes |
| U24 | Two different "Torznab address" fields on two screens (P10) | One is for others to reach Lodestar, one for Lodestar to reach others | "Lodestar's address for other apps" and "Another indexer's address" |

### 4.5 Is it working?

| ID | What happens | Why it's a problem | Change |
|---|---|---|---|
| U25 | Insights opens on controls, then about 30 panels using cohort (16 strings), harvest (14), yield (8), percentile (7) | Their question is "is it working, and do I need to do anything?"; the answer is one small "Running" tile among ten | Start with one plain sentence: "Lodestar is working. It added 409 torrents this week. Nothing needs your attention." Everything else under "Details" |
| U26 | Clicking Low, Balanced or Aggressive on Insights changes the crawler at once, with no confirmation and no undo | A stray click changes how hard their network is used | Keep it instant (the plan calls it safe) but add an Undo toast, or move the control to Settings → Crawler and show the current value here |
| U27 | The pill reads "Idle · 2.2K torrents" or "Crawling · 1.2k/h"; "Status unknown" on load | "k/h" and "Idle" don't say whether that's good | "Working · 1,200 new an hour" / "Paused" / "Needs attention"; "Connecting…" while loading |
| U28 | Times on Insights are in UTC while Language & region says "Time zone: Automatic" | They read 22:00 as their own time | Use the chosen time zone, as §10.12 says; say UTC only when it is chosen |

### 4.6 Settings and safety

| ID | What happens | Why it's a problem | Change |
|---|---|---|---|
| U29 | Settings is 22 screens in five groups of equal weight | "Trackers", "Logging", "Jobs" and "Outputs" sit level with the three things they need | The Overview becomes a short list of what most people set (download client, notifications, updates, backups, password), each with its state; the full list stays in the navigation |
| U30 | No "Learn more" link exists anywhere in the app (0 of 3,911 strings), though §10.10.4 plans them; no help page; the user guide isn't linked from the screens it explains | They leave the app to search the web | A help link per screen to its guide chapter; a `/help` landing with search |
| U31 | Glossary terms are never explained in place: junk, unverified, backfill, composition, trackers reporting, clone | They're defined only in a developer doc | A "What's this?" popover on each term's first use per screen, from `docs/ui-glossary.md` |
| U32 | Filters & Safety uses rule codes (I5); Network & VPN is read-only | The two screens that keep them safe are the least usable | Part 1's fixes; treat the Network & VPN screen as a v0.6 must, not a later slice |
| U33 | Single-key shortcuts are on by default; `o` opens the torrent client | A stray key press launches another program | Keep `/`, `j`, `k`; ask on first use of `o`, or default single-key actions off. Owner's call |

## 5. The plan itself (§10)

| ID | What the plan says | Concern | Proposed change |
|---|---|---|---|
| D1 | No reader is named. §10.9 says "the self-hoster"; §10.10 "an owner at 2 a.m." | Each section imagined its own user, mostly an expert | Add to §10.0: the §2.14 reader is the UI's reader too. "Plain words first, syntax second. Essentials first, detail on request." |
| D2 | §10.4 makes the query language the centre: tokens, autocomplete, AST, builder; the empty state is "three syntax examples" | The reader's centre is words and clicks | Reorder §10.4: words and filters first, natural phrasing as the bridge, syntax as the expert layer |
| D3 | §10.4 fixes eight default facet groups, with Trackers reporting and Extension among them | Chosen by what the index has, not by what people filter on | The order in U10 |
| D4 | §10.4 fixes the row's facts, including trackers "4 / 20" and the backfill marker | Seven facts, three of them expert | A two-tier row: choice facts always; technical facts in Compact-table or the drawer |
| D5 | §10.9 is an exploratory dashboard of six levels | Right for diagnosis, wrong as the front door | A plain health summary as level 0 (U25); the six levels become "Details" |
| D6 | §10.10 has principles for fields but none for the whole | 22 screens with no "start here" | U29, as a plan section |
| D7 | §10.10.10 lists nine wizard steps, all inline | Front-loads choices | U1: three required steps and a checklist |
| D8 | §10.14 covers labels and errors, not help | No plan for teaching | A help section: per-screen links, glossary popovers, `/help` |
| D9 | §10.16 specifies ten shortcuts; §10.13 a skip link and live regions | More is specified for keyboard experts than for learning | Keep; add U33's guard |
| D10 | §10.12 fixes IEC units and "≈" for estimates | Both are unexplained to this reader | A one-time explanation of "≈"; U12 |
| D11 | §10.3 gives five destinations of equal weight | Discover is one-third built and Insights is an expert tool, yet both sit level with Search | Keep five, but make Search's empty state the home: what's new, your alerts, system health in one line |

## 6. Blind spots

- **No one from the audience has used it.** Every judgement here is mine.
- **The first hour on a live install** (U6) has never been observed.
- **Phone use by this reader.** They will check alerts and send to a client from a phone; that flow was never walked end to end.
- **What they do when it breaks.** Logs and Logging are expert tools; there is no "Something's wrong" path that collects diagnostics and says what to try.
- **Updates and backups.** Both matter most to this reader and both were switched off in the fixture.
- **The words in other places:** notification text, digest, RSS titles, the `/status` page.

## 7. Opportunities, in order

1. **Checklist instead of a nine-step wizard** (U1, U6, U29). One component, used after setup and on the Settings overview.
2. **Zero-result rescue** (U8, U9). The parser already exists.
3. **A plain health sentence** on Insights and Search's empty state (U25, D11).
4. **"What's this?" popovers** from the glossary (U31). One primitive, about twenty terms.
5. **Per-screen help links** (U30).
6. **Simpler row and rail defaults** (U10, U11). Mostly reordering and hiding.
7. **One noun for alerts** (U17) and a confirmation after saving (U19).
8. **"Open in torrent client"** promoted (U13).

## 8. What needs the owner

These change `docs/architecture.md` §10 or the glossary, so they are proposals, not findings to fix:

1. D1: name the UI's reader in §10.0.
2. D2–D4: reorder Search around words and clicks; new default facets and row facts.
3. D5: a plain health summary ahead of the Insights story.
4. D7: three required setup steps and a checklist in place of nine inline steps.
5. U4: lead the network step with safety, not port forwarding.
6. U12: accept GB as typed, or a units preference.
7. U17: "alert" as the single noun (glossary change).
8. U33: single-key actions that launch another program need a first-use prompt, or default off.
