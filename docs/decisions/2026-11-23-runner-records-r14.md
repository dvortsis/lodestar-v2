# runner-records-r14 decisions (#1084)

The runner's records PR after r13 (#1074). It records:
- the runner's calls from 2026-11-22T16:40Z to 2026-11-23T01:00Z for #1073 and #1075 to #1080;
- the four slices running now;
- the owner's crawler and cloud-testing decisions of 2026-11-23;
- the slices all of these leave.

Each runner call is flagged to the owner, who can veto it (§2.19). Size calls are the runner's under §2.26: performance and UX changes to owner-set numbers. The slices' own decisions files hold the detail.

## The owner's decisions, 2026-11-23 (owner-decisions §2.82)
- **What the owner said.** The runner checked each message against the events of the session it was typed in (session_01MMjhn9MXEK8UMf3V5DqJ7M):
  - 00:26Z: "Let's go with whatever the runner recommends";
  - 00:27Z: "Unfortunately, the unraid box is still down. Can the test be run on the cloud machine first?";
  - 00:31:30Z: "That's OK. I authorize the tests. Run as many tests in parallel if needed. I have $133.72 remaining in credit. It's OK to use as much of that as needed.";
  - 00:35:39Z: "We should be able to run tests on unraid soon.";
  - 00:36:05Z: "Yes, go ahead with the tests."; asked whether that included the Unraid box, the owner chose "Cloud now, Unraid later" (00:36:17Z).
- **The runner's calls on the crawler:**
  1. **Owner queue item 32 is answered, and the item is closed.** CR-1c goes into CR-AB-LC with CR-LOOKUP-END, in the B arm only (r5.1's stacking). Its screen conditions become three:
     - stored per minute rises;
     - stored per DHT query doesn't fall;
     - the memory bound holds.
     Stored per slot-second is no longer a condition for a change whose purpose is more slots, because each extra lookup reaches lower-value hashes by design. Per query (politeness) and memory are the bounds that protect the network and the host.
     - **This loosens build-plan §0.5's screen for CR-1c only** (per query "doesn't fall" instead of "rises", per slot-second dropped). The owner accepted the recommendation that said so (00:26Z); this PR amends the plan's screen row to match, and it's flagged (§2.19). Every other switch keeps the standing rule: per slot-second and per query both rise.
  2. **New slice CR-SOFT-TIMEOUT** (core, Opus, `/security-review`). It is build-plan §0.5's "bounded soft timeout", built now rather than waiting for CR-5:
     - a lookup stops waiting on an unanswered DHT query sooner;
     - behind its own switch, off by default;
     - screened in the calibrated CR-SIM scenario first, under §0.5's standing rule (stored per slot-second and per query both rise), not item 32's CR-1c rule.
     It only shortens waits. It never lengthens one, never raises the rate or outstanding caps, and never loosens a bound against slow peers (those stay the owner's). It takes the next free build slot.
  3. **Experiment-host runs wait until the owner says the box is back.** The box is down, and "soon" isn't "up". The `experiment-host` condition stays met: it's the standing permission (§2.68), and only the box is missing.
  4. **Cloud first, provisional (below).**
- **Cloud spend (runner's reading).**
  - The owner named the remaining credit, not a cloud. The conversation was about AWS, so the runner reads it as AWS: new AWS spend from 2026-11-23T00:31Z may use up to the stated $133.72.
  - It stops at 90% of that (about $120), so nothing comes out of pocket.
  - The runner may not read billing. So X-test-vm confirms the balance from billing before stage 1, and the lower figure wins if billing shows less.
  - X-test-vm sends a cost estimate before each stage. A stage that would cross the stop doesn't start.
  - This replaces §2.74's $100 AWS cap for these tests only. Linode's $100 and the fleet's $40 are unchanged.
  - **If billing shows no AWS credit** (the owner may have meant another credit), §2.74's $100 AWS cap stands, no stage starts that would cross it, and the runner asks the owner.
  - **What counts:** all AWS spend from 00:31Z counts against the new limit, including the remaining hours of the three lanes that started about 2026-11-22T21:20Z.

## Cloud-first provisional A/B (runner's design)
- **Stage 1 runs in parallel, as independent treatments.**
  - Each treatment gets its own fixed instance and public IP. All use the same region, instance type, build, preset and run length, and CR-SPEC's ABBA/BAAB order within each instance.
  - **Instance 1, CR-AB-ADM:** B is `dht.coordinated_admission`; A is the shipped defaults.
  - **Instance 2, CR-AB-4's uTP arm:** B is `dht.utp`; A is the shipped defaults. The route backoff arm runs only if instance 2's A legs show IPv6 no-route (decisions/2026-11-02-CR-4.md).
  - **Instance 3, if the estimate fits: an A/A run** of the shipped defaults over the same window. It measures the noise floor between legs; a stage 1 effect smaller than it is reported as "no effect shown".
  - **Why parallel holds up:** every comparison stays within one instance and IP, so instances are never compared with each other. The cost is stacking, so stage 1 doesn't stack.
- **Stage 2, CR-AB-LC, comes after.**
  - It runs on stage 1's winners as its control, with CR-LOOKUP-END and CR-1c both on in B.
  - It starts once stage 1 reports, both slices are on main, and checkpoint-1 is met.
  - It uses one instance, plus an A/A instance only if stage 1's A/A showed a wide noise floor.
- **Start:** stage 1 starts as soon as X-test-vm posts the balance and the estimate. It doesn't wait for the three lanes running now (ending about 2026-11-23T11:00Z), unless X-test-vm's concurrency limit or vCPU quota requires it.
- **What the results count for:**
  - They're provisional relative effects (B against A), with their intervals. They choose the stacked control for the cloud's own CR-AB-LC. Like the cloud screen, they can stop a treatment only for a clear failure (a crash, a memory or disk bound, nothing stored, or a clear harm); every other stage 1 treatment still gets its host run.
  - They never count as the G-v06-crawler gain, which still comes from the experiment host.
  - Absolute rates aren't comparable: AWS ran at 8.16/min against CR-0's 12.3.
- **When the host is back:** it takes CR-AB-ADM and CR-AB-4 as the runs of record. The cloud moves on to CR-AB-LC as soon as its own stage 1 reports.
- **New slice X-host-ab-runsheet** (docs, Sonnet, reviewed on Opus; the earlier host runbooks were Opus, so a weak result is redone on Opus). It prepares the experiment host's stage 1 run sheet now, so the first block can start within hours of the owner's word:
  - CR-AB-ADM and CR-AB-4 under CR-SPEC's ABBA/BAAB design;
  - backup set to excluded;
  - §2.68's parity and backup pause, with a restore step that always runs;
  - the pinned image and the collector hash.
  It is docs only, and nothing touches the box.
- **X-test-vm's lanes on 85125f73** (base, utp2, esc2, started about 2026-11-22T21:20Z, about $7.70):
  - base and utp2 go ahead as planned;
  - esc2 counts only with the cooldown split on; otherwise it stops at a safe point.
  - The earlier lanes' calls (19:20Z):
    - route and lookup-cost pass as working as designed;
    - the route drill is still outstanding and rides the next lane;
    - #1054's uTP screen was re-confirmed, plus one control lane, all switches off, for L1's anonymous-memory drift.

## Calls in merged slices
- **X-insights-story (#1073):**
  - The review fixes were checked by the runner, and the regenerated `state-insights-*` PNGs were kept.
  - **Owns gain** `web/e2e/states.test.ts` (the paused fixture).
- **X-search-toolbar (#1075):**
  - **The URL is never written automatically.** The view comes from one place (URL, then the Appearance preference, then List). The URL is written only when the user picks a view. This fixed typed text being lost on an empty `/search`, and stops a written `density=` from outranking Appearance.
  - **A late preference applies at the next query,** so it never shifts rows already shown.
  - **The Filters host:**
    - it is absolutely positioned in every state below x-large;
    - `ResultsToolbar` sets `--tb-inset` and `--tb-rail-h` on `#results`, in the same `Promise.all` as the panel and `toolbar.css`, so it's there in the first frame;
    - the layout-shift bound went back to 1e-9 below 1024 px, with delayed-chunk and 4× CPU variants. The cold baselines are main's.
  - **The banner** fits one line at 390 px, with its 16 px icon and shorter phone wording, which refines §10.4's wording.
  - **No size raises:**
    - Search cards 8.00 / 8.01 kB;
    - Results toolbar 4.29 / 4.6 kB;
    - the Search route 44.95 / 45 kB.
    - The raw name on cards (97 B) is deferred to X-cards-raw-name.
  - **Owns gain** `results/ResultsPanel.svelte`, `results/IndexState.svelte` (a class hook) and the search messages' banner copy.
- **X-units-typed (#1076):**
  - `to_query()` is byte-identical: a 200,000-case property test checks it against the frozen printer.
  - Typed output appears only at the request AST, `/parse` and natural phrasing.
  - **Owns gain:**
    - doc comments in `crates/api/src/dto/{events,export,facets}.rs`;
    - `web/src/lib/features/search/query/clauses.test.ts`;
    - `crates/search/tests/suggest.rs`, `crates/api/tests/{suggest,user_data}.rs` and the two re-blessed parse goldens;
    - `crates/search/tests/doc_examples.rs`, `crates/store/src/saved.rs` (one doc line), `dto/saved.rs` and `dto/search.rs` (docs only);
    - the regenerated `openapi.json` and `schema.d.ts`.
  - **For the owner:** bookmarked and recent searches written with `GB` now match a set about 7% smaller (U12, documented in the slice's file).
  - **Nits for the next edit of these files:**
    - `saved.rs:131-141` (the `q` field when an AST is sent);
    - `query_props.rs:148` (`PROPTEST_CASES`);
    - `clauses.ts` `typedSize` (n±1 by side);
    - sub-byte fractions truncate, as the stored path does.
- **X-settings-find (#1077):**
  - **A failed index load isn't retried in a loop.** It's said once, in the single live region.
  - Entries for screens that can't resolve `#field` were dropped.
  - Secret fields stay masked; the security pass was clean.
  - **Sizes:**
    - the Settings section and screen first views were set at measured;
    - the new "Settings find (on demand)" entry was set at measured;
    - no raise for the global CSS, Alert or Welcome.
    - P4's field layout moves to X-field-layout, at no size cost.
  - **Results (lazy) went from 40.85 to 40.86 kB, set in #1077 by the file's owner.** Only the table chunk's hashed filename changed in Results' imports, and it compresses 3 B worse.
  - **Owns gain:**
    - `settings/ChangePassword`, `Keys` and `Proxy` (P17);
    - `screens2/GroupedForm.svelte` (I18);
    - `features/settings/find/messages/**`.
  - The uTP/TCP transport choice is hidden: it's stored and shown only. Badges were dropped, because no loaded data backs them.
- **X-insights-story-stages (#1078):**
  - **Sizes:**
    - the stage pages went from 40.66 to 41.12 kB (measured 41.11);
    - a new "Insights overview (own JS + CSS)" entry at 65.03 kB.
  - **The KPI tile track** is `repeat(auto-fit, minmax(min(12rem, max(9.5rem, 19%)), 1fr))` from 40 rem, so five tiles share 976 px rather than wrapping 4 + 1.
  - The Harvested chart counts from first harvest, matching its tile. It has a new summary message.
  - **The flake** in `insights.routes.test.ts`: the test pressed Undo mid-request. It now waits for the condition.
  - Chinese separators go through message placeholders.
- **X-hit-resolution (#1079):**
  - `SearchHit.resolution` is required and nullable. It's set once, in the shared `enrich()`, so every hit list carries it; export is unchanged.
  - The table column `res` has no sort. Saved column preferences that predate it get it appended, hidden.
  - **Owns gain:**
    - `crates/api/src/torrents.rs` (one line);
    - the re-blessed `crates/api/tests/golden/*.txt`;
    - one search message key;
    - the hit fixtures in `search/results/fixtures.ts`, `discover/testing.ts` and `alerts/fixtures.ts`;
    - `web/e2e/alerts.routes.test.ts`, `alerts.keyboard.test.ts` and `detail.fixtures.ts` (one line each).
  - The table entry went from 6.12 to 6.18 kB, under its 6.5 kB limit.
- **X-narrow-states (#1080):**
  - Discover hides every `needs_*` section; `search_disabled` keeps its full-page empty state.
  - Logs uses a two-line row and wraps by default below 640 px, set by CSS before first paint. There's no `matchMedia` at mount, and layout shift is 0 at 390 px.
  - **The Discover tab strip** fades at its edges with tokens. Focus removes the fade (`:has(:focus-visible)`), and a focused tab scrolls into view. The sr-only cue is gone.
  - **Owns gain** `settings/logging/**` (the Verbosity wrap only).
  - **A commit ran prettier over `docs/decisions/`** and rewrote 520 records. They were restored from the previous head in the same PR. X-narrow-states-2 says what ran it (in its step 0), and the runner decides whether a guard is needed.
  - **The contrast failure was mid-fade.** The e2e now waits until no animation is running (15 s deadline) before axe.

## Calls in running slices
- **X-detail-depth (#1081), split in two:**
  - PR 1 is the drawer and `/t`: I16, U13, U14, I28, I26, 02 §5.5 and Swarm.
  - PR 2, X-detail-depth-compare, is Compare Differences only, on its own branch.
  - **Calls:**
    - tracker URLs show the host only, because of passkeys;
    - ID links come from strict id patterns into fixed `https` templates, never from API URLs;
    - the actions menu has text-only items, like the row menu;
    - the no-client hint is a menu item ("Set up a download client…"), not an inline line, which keeps layout shift at 0;
    - the `/t` back link guards a null `from.url`;
    - Favorite, Hide, Labels and Not this one wait for a door to exist.
  - **Sizes:**
    - Detail sections 38.82 to 47.49 kB (Swarm, Technical details, RelativeTime and the CSS they moved). The menu's library chunks are excluded by name, as the row menu's are;
    - a new "Detail actions menu (on demand)" entry at 0.79 kB;
    - `/t` set back to 22.04 kB (measured 22.03);
    - the drawer stays at 28.31 kB;
    - Identified-as at 2.84 kB;
    - Compare 31.14 kB, the same 24 modules, with the difference coming from size-limit's shared-chunk partition.
  - **Owns gain** `routes/t/[hash]/+page.svelte` (the back link) and the e2e specs `detail-depth.routes`, `detail-history.routes`, `detail.routes`, `detail.fixture` and `filters-block.keyboard`.
- **X-shell-home (step 0, go 2026-11-22T22:40Z):**
  - **The pill's words:** the glossary's terms where it has them, otherwise Working, Ready, Needs attention, Connecting…, with no rate.
  - The home screen drops "Nothing leaves this computer". Privacy claims are legal wording, which is the owner's.
  - The syntax guide stays out until `/help/syntax` exists.
  - `clearRecent()` runs from `auth/sign-out.ts`, on every sign-out path.
  - **Size:**
    - the home screen gets its own lazy chunk, "Search home (lazy)", at measured (about 1.89 kB), in the same `Promise.all`;
    - `Home.svelte` joins the Results partition, so banners stay at or under 4.1 kB;
    - no raise for Results (lazy), the Search route, the global CSS, Alert or Welcome.
  - **The page shift is Bits UI's scroll lock** on `body`, with classic scrollbars. `ui/overlays/Menu.svelte` gets `preventScroll={false}` for every menu, since they're non-modal. That comes after #1081 merges, when `Menu.svelte` and `web/.size-limit.mjs` are handed over together, and an e2e covers it. `app.css` is unchanged.
  - **Owns refined:**
    - `search/home/**` replaces `search/empty/**`, which doesn't exist;
    - plus `search/assist/recent*` and `auth/sign-out.ts`;
    - plus the one `results/results.css` rule (`:has(.rp-empty[data-home])`);
    - plus `Menu.svelte` and `.size-limit.mjs` after the handover;
    - plus the shell e2e specs and its decisions file.
- **X-skip-results-flake (option A, a product fix):**
  - **The cause:** the shell's keydown listener attaches only after `whenEngaged`'s seven-chunk `Promise.all` (`Chrome.svelte`). Only the waking key was replayed, so users lost keys typed on a slow load.
  - **The fix:**
    - keys are buffered until the listener attaches, then replayed in order through `keys.handle`, with guards;
    - nothing typed into an editable target is replayed, nor Tab, nor Enter or Space on native controls;
    - the buffer is capped at 32 events and 5 s.
  - **Owns:** `shell/Chrome.svelte`, `shell/Shell.svelte` and `web/e2e/search-toolbar.keyboard.test.ts`.
- **X-retry-typed-flake stays its own slice.** Slot 2 ran `chunk-load.routes.test.ts:291` 30 times:
  - without the key-buffer fix, 10 of 30 failed; with it, 5 of 30.
  - Every failure has the same signature: the URL ends `q=lanternharbor`. Select-all lost its selection, and the typing was appended to the old text.
  - The key buffer never holds typing keys, so it isn't the cause. The slice looks first at the search box's content or selection being reset when the chrome arrives (unverified).
- **X-narrow-states-2 (step 0, go 2026-11-23T00:10Z):**
  - the alert list is sized to its rows;
  - the table keeps a minimum height only while loading, if that's needed;
  - the wizard shows compact progress on a phone, numbered per the spec and owner-decisions, else 1–12, with no Welcome raise;
  - the facet rail uses the full width of the 390 px sheet;
  - Discover hides `unavailable` when its reason is null.
  - **Deferred:** Newest's width jump becomes a requirement on whichever slice ships Rising, which decides the layout before paint.

## Size measurement
- size-limit's `own()` credits a route by chunk partition. So a shared chunk moving between routes, or a renamed lazy chunk's hashed filename, moves a route's figure without its code changing (Compare +0.25 kB, Results +3 B).
- **X-size-own-modules** measures a route by its module set and ignores hashed import specifiers. It runs once `web/.size-limit.mjs` is handed to it.

## Follow-ups tracked in runner-state, not new slices
- **The `?browse=1` layout shift** of 0.012–0.013 at 320–360 px, which predates X-search-toolbar (its decisions file names it a runner follow-up). Tracked in runner-state `followups`; it rides the next slice that owns the search layout.
- **A back link on `/alerts`**, from X-detail-depth's step 0. Tracked in runner-state `followups`; it rides the next alerts slice.

## New slices
- **Crawler (core, Opus):**
  - CR-SOFT-TIMEOUT (above);
  - X-sampler-streak-metrics: export the sampler's failure, streak and known-node counters;
  - X-capacity-rejection-source: label capacity rejections by source. On four lanes they were 3.8 to 6.1 M an hour, and they aren't fetch admission.
- **Experiment host:** X-host-ab-runsheet (above).
- **API (core, Opus):**
  - X-hit-resolution-nits: `example_hit()` carries a resolution, and the schema gives `resolution` an enum plus null;
  - X-detail-attributes: `GET /torrents/{hash}` carries the parsed attributes that only `POST /compare` has today;
  - X-hidden-count: P7's other counts ("2,208 torrents", "2.2K");
  - X-insights-reconcile-test: a Rust test that seeds one store and compares the strip's numbers with the stage pages'.
- **Web (Sonnet, reviewed on Opus, unless marked):**
  - X-detail-depth-compare;
  - X-field-layout: P4 at no size cost;
  - X-cards-raw-name: the raw name on cards, paid for by savings;
  - X-rowactions-arrowdown: ArrowDown opens the row menu's trigger before its chunk loads, as #1081 does for the detail menu;
  - X-index-banner-late (Opus): under load, the "still growing" banner can arrive after the rows (`FIGURES_WAIT_MS` is 1 s) and push them down 38–60 px;
  - X-retry-typed-flake (Opus);
  - X-size-own-modules.
- **The API lane** (owner credit, 2026-11-22) holds items 25 to 30: X-insights-reconcile-test, X-sampler-streak-metrics, X-capacity-rejection-source, X-hidden-count, X-detail-attributes and X-size-own-modules. Their "wait for r14" holds are cleared when this PR merges. The lane's reviews are pre-reviews; the runner's fresh Opus review decides each merge.
- **Not a new slice:** the Newest width jump (above). X-discover-latent's other item, `unavailable` with a null reason, went to X-narrow-states-2.
