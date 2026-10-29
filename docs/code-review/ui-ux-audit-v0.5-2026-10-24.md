# Lodestar v0.5 UX/UI design audit

Date: 2026-10-24 (America/Denver)\
Reviewed workspace: `ad0ec5a84555b8b36e7ae94ed515b84ad6d46e02`\
Audit branch: `docs/ui-ux-audit-2026-10-24`

The implemented UI has a consistent visual system and substantial keyboard support. Task completion and responsive interactions need attention: **2 blocking, 7 important and 3 polish findings** are recorded below. This is an audit, not a completed remediation or accessibility certification. Application source was not changed; nothing was committed or pushed.

The two earlier reviews are now logged: [Area A: UI behaviour](codex-review-v0.5-light-area-a-results.md) and [Area B: packaging and install](codex-review-v0.5-light-area-b-results.md). Those reviewed fresh `main` at `2d5e761d30cb1b7947fb09493959668eabcb84ab`; a directory comparison found the workspace's `web/src` identical. Findings carried forward here are explicitly identified.

## Method and coverage

Applied the installed **ui-design-skills** design-review workflow and its application-interface, layout-spacing, layout-grids, responsive-layout, typography, geometry, iconography, motion, minimalist-style, dashboard-storytelling, chart-selection, infographic and analytics-validation guidance. Owner decisions, architecture and metric/token contracts took precedence. The audit preserves Svelte, Atkinson Hyperlegible, Riverstone, the existing radii and all 24 theme pairs; React/Tremor defaults were not treated as requirements.

- **Routes:** Search, Discover, Alerts, alert detail, Insights overview, an Insights stage, Compare, Login, Setup, Welcome, Settings and full torrent detail. Each was rendered at **320, 390, 768, 1024, 1366, 1920 and 2560 CSS px**: 84 route/width observations. Additional interactions covered populated Search, density, drawers, mobile touch, long Latin/CJK names and text spacing.
- **Accessibility:** 24 route scans at 320/1366; Insights scans in all **48 themes**; keyboard navigation, forms, dialogs, sheets, menus, palette, trees, virtual lists and reduced-motion checks through the existing browser suite. The route survey found three scan instances with issues: narrow Compare's duplicate landmark, and full detail's heading order at both widths.
- **States and feedback:** loading, empty, failure/retry, stale, paused, startup, missing measurements, disabled/locked controls, toast focus, form errors, settings conflicts and leave protection were examined through rendered tests and source. Settings form/shortcut showcases validate primitives; they do not make the production Settings route complete.
- **Analytics:** independently checked fixture literals and arithmetic against rendered KPI, funnel and trend-table values; inspected time bases, null/gap treatment, incomplete cohorts, comparisons, drill-throughs and CSV URLs. This validates presentation and request wiring, not backend rollup accuracy.
- **Visual craft:** hierarchy, alignment, spacing, density, type, containment, icon consistency, target sizes and theme contrast were assessed in rendered views and shared source. No new palette, font or decorative system is proposed.

The [validation ledger](evidence/ui-ux-v0.5-2026-10-24/validation.json) retains route measurements, axe nodes, all theme results, focus observations, analytic reconciliation and test outcomes. Screenshots use synthetic fixtures.

### Validation results and limits

- **205 browser cases passed** in the main run: the audit's collection cases plus existing auth, alerts, detail, Discover, Compare, Insights, Search, shell, settings-form, shortcut, chart and UI-primitive checks. Collection cases record accessibility findings; their successful execution does not mean every scan was clean.
- The additional probe run had **51 passes and one initial activation failure**. The focus probe passed separately after changing its immediate scripted focus/Enter setup to a pointer activation; its resulting measurements confirm findings I2/I4. The 48 theme scans reported no axe violations in the sampled Insights state.
- Discover's transfer probe passed and documented the broken destination. A long-name/CJK probe failed on genuine desktop overflow after opening detail. Two subsequent measurements reproduced that overflow with ordinary names, at default and persisted drawer widths. The failure is retained as I1, not dismissed as a long-name fixture issue.
- The theme compiler's `--no-write` run reported **48 themes passing**, with existing documented warnings. All six focused all-family gate tests passed. A broader theme-unit run had **75 passes and 26 failures**: 25 doc comparisons use an LF-only fenced-block parser against CRLF files, and one generated-CSS comparison is byte-sensitive to CRLF. These are validation portability limits, not evidence of 26 runtime colour defects.
- Built the current frontend and used production Vite preview with `VITE_DEV_ROUTES=1`, installed Chrome and Playwright 1.56.1. A temporary resolver disambiguated `shell.svelte.ts` from `Shell.svelte` on Windows. Initial unstyled development captures were discarded. Production assets/source were not patched to improve the screenshots.
- API responses came from the existing OpenAPI mock and hand-built fixtures. The available Rust binary lacked the fixture feature; **no live-backend analytic reconciliation** is claimed. Fixtures intentionally contain distinctive illustrative values; they are not a statistically consistent production dataset.
- Native browser zoom, an actual screen-reader session, Firefox/Safari, physical touch devices, full Chinese-language coverage and Lighthouse were not evaluated. A 200% root-font stress capture is not native browser zoom and does not uniformly scale fixed-pixel type. Text-spacing and 320 px reflow checks cannot establish full zoom compliance. All-theme scans sample one desktop dashboard state, not every route and popup.

## Blocking

### B1. Successful first-run setup leaves the user stranded

**What:** Setup submits successfully and routes to `/welcome`, which contains only “Welcome.” Bare-route chrome provides no navigation or completion action. Confirmed in rendered Welcome and source; carried forward from Area A #2.

**Why:** A new administrator cannot continue to Search through the offered flow. This blocks the product's first useful task even though password setup succeeded.

**Fix:** Add a real completion/Continue action to Search, or mount the next implemented onboarding step with a clear exit. Show that setup succeeded. This does not require implementing the entire future wizard at once.

**Locations:** `web/src/routes/welcome/+page.svelte:1`, `web/src/lib/features/auth/SetupForm.svelte:90`. [Screenshot](evidence/ui-ux-v0.5-2026-10-24/welcome-390.png).

### B2. Adult visibility changes do not remove already cached detail

**What:** Cached hit/detail data can remain visible after adult content is disabled elsewhere. A fresh cached reopen skips the API; an expired detail response denied as `adult_hidden` can still leave the cached title/raw name. Carried forward from Area A #1; supported by its compiled-model probe, not newly reproduced against a live backend here.

**Why:** The user's explicit visibility choice is not reflected in what the UI displays. An error message alone does not remove identifying content.

**Fix:** Invalidate/revalidate visibility-sensitive caches when the setting changes, and remove cached identifying metadata and body on an adult-content denial. Verify both reopening and an already visible detail.

**Locations:** `web/src/lib/features/detail/model.svelte.ts:38`, `web/src/lib/features/detail/cache.ts`. See [Area A evidence and reproduction](codex-review-v0.5-light-area-a-results.md).

## Important

### I1. The desktop detail layout overflows at its own 1366 px breakpoint

**What:** Open an ordinary result at 1366 px. Rail/results/drawer tracks measure `264px 560px 560px`; the drawer ends at x=1448. With a persisted 640 px drawer, it ends at x=1528. The primary action and close control extend beyond the viewport. Reproduced with ordinary names after the long/CJK check first exposed it.

**Why:** A supported desktop size loses visible controls immediately after a normal action. A page-level horizontal scrollbar is a poor recovery for closing detail.

**Fix:** Keep detail overlaid until rail, minimum results width, gaps, padding and the chosen drawer width actually fit. Alternatively reflow/collapse a track. Clamp persisted width to the available composition. Verify 1366 with both default and maximum saved width, including the close control.

**Locations:** `web/src/lib/features/search/results/results.css:10`, `web/src/lib/features/detail/DetailHostBody.svelte:25`, `web/src/lib/theme/tokens.css:113`. [Screenshot](evidence/ui-ux-v0.5-2026-10-24/drawer-grid-default-1366.png).

### I2. Fullscreen mobile detail permits focus behind its opaque surface

**What:** At 390 px, open detail and Shift+Tab from its focused title. Focus moves to a Search result underneath the fullscreen drawer. The focused row's rectangle is within the viewport, but hit-testing confirms it is covered by the drawer.

**Why:** Keyboard users lose a visible focus indicator and can operate hidden page content. Desktop non-modal behavior is being applied to a composition that hides the entire background.

**Fix:** Preserve the non-modal desktop drawer. For the fullscreen mode, use the full-page detail route with a reliable return destination, or make covered background content inert for that mode and restore it on exit. Do not introduce a global drawer focus trap contrary to the desktop contract.

**Locations:** `web/src/lib/features/detail/detail.css:97`, `web/src/lib/ui/overlays/Drawer.svelte:63`, `DetailHostBody.svelte`. [Visible surface while background owns focus](evidence/ui-ux-v0.5-2026-10-24/drawer-mobile-focus.png); focused-element evidence is in the ledger.

### I3. Insights jump links hide the focused destination under sticky controls

**What:** At 320×640, activate the Fetch stage link. The “Where torrents are lost” heading is focused at y=0 while the sticky context bar occupies y=0–276. The beginning of the section is hidden. `jump()` scrolls the heading, but scroll margin is assigned to the enclosing section.

**Why:** Navigation announces arrival without visibly showing where the user arrived. On a small screen the context bar covers nearly half the available height.

**Fix:** Apply the scroll offset to the actual scroll target and account for the context bar's wrapped height. Consider a smaller collapsed context presentation on narrow screens. Verify each stage jump with range chips, custom dates and text spacing.

**Locations:** `web/src/lib/features/insights/overview/Overview.svelte:59`, `web/src/lib/features/insights/overview/insights.css:17` and `:128`. [Screenshot](evidence/ui-ux-v0.5-2026-10-24/insights-jump-mobile.png).

### I4. Browser Back closes detail and drops focus to BODY

**What:** Open a Search result, focus the drawer title and invoke browser Back. The drawer disappears and `document.activeElement` is `BODY`. This audit confirms Area A #3 in a browser; the earlier review used lifecycle/source probes.

**Why:** Returning to results loses the user's place. Escape/button-close coverage does not exercise this route teardown.

**Fix:** Restore the invoking row during teardown, or keep the host mounted long enough to process its closed state. Handle disconnected/virtualized invokers with a stable list fallback. Exercise browser Back separately from Escape.

**Locations:** `web/src/routes/search/+page.svelte:119`, `web/src/lib/features/detail/DetailHostBody.svelte:89`. Active-element evidence is in the ledger.

### I5. Mobile results prioritize secondary details over size and availability

**What:** At 390 px with a coarse pointer, the always-visible Copy magnet/More actions leave the first row's main content about 162 px wide. Its facts line shows badges and part of the composition bar; size and seeders are clipped. The measured size begins at x≈267 while the main content ends at x≈190. Discover also clips later facts in its narrow rows.

**Why:** People comparing downloads cannot scan size or swarm availability without opening each item. These facts matter more to the choice than a partial composition graphic.

**Fix:** Define a narrow-row content priority: preserve title, size and seeders; collapse raw name, composition, extra badges and secondary timestamps first. Consider icon-only copy in narrow rows while retaining its accessible name and touch target. Keep density state consistent with owner decisions rather than silently overriding an explicit choice.

**Locations:** `web/src/lib/features/search/results/ResultRow.svelte:139`, `row.css:17`, `row.css:65`, `RowActions.svelte:60`. [Touch screenshot](evidence/ui-ux-v0.5-2026-10-24/search-touch-390.png).

### I6. Discover's default “See all in Search” loses the content being browsed

**What:** From Discover → All, the action navigates to `/search?sort=-discovered` with an empty editor. The browser probe recorded zero Search requests. The destination shows the blank Search entry state. Carried forward from Area A #4 and now browser-confirmed.

**Why:** A continuation action discards the current browsing task instead of expanding the shown collection.

**Fix:** Encode a supported browse-all query, or let this destination explicitly initialize an all-items AST. Verify the All tab and typed tabs against the request and displayed collection, not just the link string.

**Locations:** `web/src/lib/features/discover/tabs.ts:67`, `web/src/lib/features/search/results/results.svelte.ts`. URL/query/request evidence is in the ledger.

### I7. Navigation and remediation actions promise unfinished destinations

**What:** The primary Settings destination renders only “Settings.” Insights stage links render only “Insights stage”; exception “Open settings”/stage fixes lead into these stubs. Overflow Logs and Syntax target routes that do not exist. Rendered Settings/stage routes and source confirm the destinations.

**Why:** Users encounter a route that looks successful without receiving the controls or diagnosis promised by its link. Settings is especially prominent and is needed to finish exception-remediation tasks.

**Fix:** Prioritize wiring the already-built settings form into a useful production section. Until remaining sections land, gate unavailable actions or provide an explicit unavailable state, an actionable alternative and a return path. Route availability should be shared by navigation, palette and exception links.

**Scope:** The build plan schedules several of these screens later. This is an **exposed delivery gap**, not a claim that every deferred feature is a regression or must be implemented now. Swarm sorts, export, grouping, bulk selection, enrichment, worker mode and other explicitly deferred capabilities are not separate audit defects.

**Locations:** `web/src/routes/settings/[section]/+page.svelte:1`, `web/src/routes/insights/[stage]/+page.svelte:1`, `web/src/lib/shell/nav.ts:45`, `web/src/lib/features/insights/overview/links.ts:87`. [Settings screenshot](evidence/ui-ux-v0.5-2026-10-24/settings-1366.png).

## Polish

### P1. Narrow Compare has two landmarks named “Files”

**What/why:** The attribute-first Files section and the file-diff section have the same accessible name. Axe reports `landmark-unique` at 320 px; a screen-reader landmark list cannot distinguish them.

**Fix:** Name the first “File count” and the second “File comparison,” or otherwise provide distinct descriptive names while preserving visible/accessible label alignment. Verify both narrow and wide layouts.

**Locations:** `web/src/lib/features/compare/CompareList.svelte:43`, `web/src/lib/features/compare/FileDiff.svelte:28`, `web/src/lib/features/compare/messages/en.json`. Exact axe node is retained in the ledger.

### P2. Full detail skips a heading level in Contents

**What/why:** Full-page Contents is level 2, but its Composition child is hardcoded `<h4>`. Axe reports `heading-order` at both 320 and 1366. The same component's h4 is appropriate beneath the drawer's level-3 Contents heading, so this is a shared-component context mismatch.

**Fix:** Pass the parent heading level into the Contents children and render the next level, preserving their visual type token. Check the drawer and standalone page independently.

**Locations:** `web/src/lib/features/detail/DetailView.svelte:98`, `web/src/lib/features/detail/Contents.svelte:15`, `web/src/lib/features/detail/composition/Composition.svelte:38`.

### P3. Short Discover lists reserve most of a mobile screen as empty space

**What/why:** With three items, the Newest panel retains a viewport-derived list height. In the 390×900 capture, rows finish around y=420 but the panel continues to roughly y=906 before the next section. It makes the short list look unfinished and pushes the other sections down.

**Fix:** Cap the list viewport by the loaded content height when there is no further page. Keep the fixed virtual viewport for genuinely long/paginated collections. Preserve stable loading geometry without keeping permanent empty space after loading.

**Location:** `web/src/lib/features/discover/DiscoverPage.svelte:508`. [Screenshot](evidence/ui-ux-v0.5-2026-10-24/discover-390.png).

## Analytical and visual findings that passed

- **Dashboard story and chart choice:** status/attention lead into loss drivers, movement, segments, Serve and detail. Ranked horizontal bars, a count-based funnel and separate trend charts fit their questions. The stage strip explains a process without implying its mixed snapshot/event counts form one conversion denominator. [Desktop example](evidence/ui-ux-v0.5-2026-10-24/insights-desktop-top.png).
- **Data presentation:** a literal 1,234,567 becomes `1.2M`; stored 40,012 remains `40,012`; the 48-row hourly UTC trend begins at 100 and ends at `100 + 47×7 = 429`. Null buckets 7/8 remain em dashes and visible line gaps. Incomplete cohorts are labeled/hatched; missing Serve measurements have explanatory text. Rates are supplied by the server, not recomputed from the illustrative fixture totals. Existing tests exercised comparisons, custom ranges, retry and URL state.
- **Drill-through meaning:** segment links explain that event-time chart counts can differ from Search's discovery-time results. This is an explicit semantic distinction, not an unexplained reconciliation failure. Actual destination availability remains I7.
- **Consistency and interaction:** token-based surfaces, borders, control geometry, type and Phosphor icons form a restrained coherent system. Keyboard forms, virtualized lists, trees, dialogs, menus and toasts performed well in the exercised cases. Coarse-pointer primitive targets and reduced-motion paths passed their existing checks. These strengths do not negate the specific hidden-focus and route-level issues above.

## Suggested repair order

1. Give successful setup a usable completion path; address visibility-sensitive cache invalidation.
2. Correct the drawer's desktop fit, fullscreen background focus and browser-Back return focus together.
3. Repair Insights jump offsets, mobile result priorities and Discover's browse continuation.
4. Make destination availability explicit, then finish heading/landmark names and short-list spacing.

**Highest-leverage change:** restore a complete first-run path from successful setup into usable Search. It removes the earliest hard stop before users can benefit from the UI's existing visual and keyboard strengths.
