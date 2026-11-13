# UI/UX review decisions (runner, #809)

The owner's answers to the 2026-11-13 UI/UX review (`docs/owner-decisions.md` §2.66), and where the runner puts each. The review itself is on branch `claude/codebase-review-bugs-x9ff7p` under `docs/review/2026-11-13-ui-ux/`; it comes to `main` in a later runner PR, re-committed rather than merged.

## The owner's decisions (2026-11-13)
- **Who decided:** the owner, in the codebase-review session: "Go with your recommendations to all the decisions." That session relayed the answers to the runner. The B1/B2 priority came the same day, relayed by the "Remaining work plan" session from the codebase-review session.
- **I1, the detail drawer from 1366 px.** It sits beside the results, as §10.15 says.
  - While the drawer is open, the results track may shrink below `--results-min-width` (560 px, §10.2) to a 480 px floor. The drawer opens at `--drawer-min-width` (480 px) until the window fits the stored width. At 1366 px that leaves the results about 542 px.
  - Why: today `web/src/lib/features/detail/DetailHostBody.svelte` overlays the drawer whenever the rail, the 560 px results minimum and the drawer don't fit side by side: below 1,384 px at the drawer's 480 px minimum, and below about 1,464 px at its 560 px default. At the most common laptop width it covers the toolbar and about 57% of the rows.
  - This changes how one §10.2 number is applied, and only while the drawer is open. The owner decided it directly, so it needs no further question.
  - Built together with I3 (result rows adapt to their container, not the viewport), because 542 px rows must not clip.
- **GS-5, "Group versions" default.** On once group data is ready, kept per account and in the URL. While group data isn't ready, search answers ungrouped (GS-3), and the toggle shows a one-line reason (GS-5). If G-v07-scale fails the keyword p95 with grouping on, the default falls back to off and the question goes back to the owner.
- **ENR-13, "Identified as".** Text, IDs and links only: no posters or other third-party images. Each image would be a request from the user's browser to a third party. Artwork later would be a separate setting, off by default, and a new owner question.
- **Settings → Sources.** It's restructured as its own slice, before the next plug-in row lands: collapsible groups with a summary line, a filter box, one-line rows that expand, and a consent-class legend. Consent dialogs and switch behaviour don't change.
- **B1 and B2 first.** The wizard's raw config keys and the missing Sign out go ahead of the larger queue.

## Not decisions
- **B4 (Fetch 0 against the funnel's 413)** is a fixture gap, not a counter bug: `bench/src/fixture.rs` seeds no `FUNNEL_EVENTS` rollups. It's re-ranked Important. The fix seeds them in the fixture, consistent with its cohort rows, and the UI labels the strip's time base and shows an empty state instead of ten "0" rows. A live-crawl check of the counter is still open.
- **Usenet visibility** was already decided (§2.46, §2.47; `owner-v07-q6` holds).

## The runner's placement (§2.19)
- **X-wizard-labels-signout** (new; launched 2026-11-13T03:15Z in the freed build slot): B1 and B2, in one slice. The wizard reuses the Settings screens' label, help and option keys from one source, with a test that no rendered label equals its key. Sign out goes in the overflow menu and in Settings → Security → Sessions, reusing `LogoutButton`.
- **X-ui-guards** (new, after X-wizard-labels-signout and X-sources-restructure): first it moves the 124 unweighted `~icons/ph/*` imports on `main` to a weight and removes the 4 dead breakpoint classes (review P2: the theme removed those breakpoints), then it adds lint bans on both, plus a fixture-backed axe run in CI that covers `/insights` (review B3) and the wizard. Its `web/src/lib/**` edits are those lines only, so it launches when no other web slice is running. It's Sonnet work under the delegation policy: mechanical edits, lint rules and test scaffolding that follow existing patterns.
- **X-sources-restructure** (new): the Sources decision, with `/security-review` because the rows' switches open the consent dialogs. It's added to the deps of IRC-2, NOS-3, LEM-1 and the twelve enrichment plug-in slices that each add a Sources row (ENR-tmdb, -tvdb, -anilist, -mb, -ol, -igdb, -discogs, -steam, -anidb, -srrdb, -predb and -debrid; IRC-3 follows through ENR-predb).
- **GS-5 and ENR-13** carry their decisions in their `notes`. ENR-13 merged as #793 with text only, so nothing changes there. Its follow-up ENR-13b gets the same rule when it's sliced.
- **X-insights-funnel-fixture** (new): B4's fix, as the relay set it out: seed `FUNNEL_EVENTS` in `bench/src/fixture.rs`, label the Fetch strip's time base, and show an empty state instead of ten "0" rows.
- **I1 + I3** go to one slice in the Track Q triage PR, carrying this file's drawer rule.
