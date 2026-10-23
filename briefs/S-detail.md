# S-detail: Detail drawer and /t/{hash} with file tree worker
**Model:** Sonnet (helper: the §10.5 screen over existing endpoints). The file-tree worker only reshapes the server's decoded file list, and the raw viewer renders JSON as text. Nothing here parses bencode or binary data. **Branch:** `slice/S-detail`. No `/security-review`, on these conditions:
- Torrent names and paths are untrusted text from the DHT. They are rendered only through Svelte's escaping, never `{@html}`.
- The only URL taken from torrent data is `magnet`, used only after checking it matches `^magnet:\?`.
- If you need anything else (another link from torrent data, `{@html}`, decoding bytes), stop and say so.

**Spec:**
- `runner/standards.md`, `docs/owner-decisions.md`, and CLAUDE.md's UI rules.
- Architecture:
  - §10.5: all of it. This slice builds the drawer, the header, sections 1–3 and the section 6 component.
  - §10.1: the drawer chunk ≤ 15 KB; a Web Worker above 2k files; prefetch after 150 ms idle; `content-visibility: auto` for offscreen sections.
  - §10.2: drawer motion, title 20 px, raw name mono 13, Phosphor icons.
  - §10.3: `/t/{hash}` is a drawer from Search and a full page from a direct link.
  - §10.11: one primitive per role.
  - §10.12: absolute dates in the drawer, null versus zero.
  - §10.13: the tree pattern, focus, no focus trap in the drawer.
  - §10.14: "Never scraped", "Seen 3 hours ago", "Reported by 4 of 20 trackers checked", "First seen during backfill".
  - §10.15: the drawer at each range.
  - §10.16: Enter, `o`, `c`, Esc.
  - §10.18: the composition colours and fixed order.
  - §20.9: the `adult_hidden` 404.
- Contract docs:
  - `docs/metrics.md` §1, §3, §4 (every torrent metric's formatter and null rule), §7.1 and §7.2: the field statuses. "Planned on detail" means the field is **not** on `TorrentDetail` yet.
  - `docs/ui-glossary.md`.
  - `docs/design-tokens.md` §3 (`--data-*`) and §7 (`--drawer-*`, `--row-height-compact`, `--compbar-*`).
- What exists on `main`:
  - **The API types** (`web/src/lib/api/schema.d.ts`):
    - `TorrentDetail`: `completed`, `dht_peers`, `dht_seeds`, `discovered_at`, `file_list` (the first **500**, in torrent order), `file_list_total`, `files`, `flags`, `hash`, `hash_v2`, `id`, `last_scrape_at`, `leechers`, `magnet`, `metadata_at`, `name`, `piece_len`, `seeders`, `size`, `swarm_history`, `swarm_trend`, `trackers_checked`, `trackers_reporting`.
    - `FilesPage {files, offset, total}`, from `GET /torrents/{hash}/files?offset&limit` (at most 1000). Its `FileEntry` is `{attrs: ("padding"|"hidden"|"executable"|"symlink")[], path, size}`.
    - `SiblingsResponse.identity: TitleIdentity {years, seasons, episodes, absolute, title_words} | null`, from `GET /torrents/{hash}/siblings`.
    - `SearchHit`, for `title`, `resolved_via`, `content_type`, `adult_kind`, `composition: CompositionShare[] {group, files, bytes, pct_bytes, pct_files}`, `junk_score`, `junk_reasons` and `matched_files`.
  - **The handlers:** `crates/api/src/torrents.rs`, and `crates/api/src/compare.rs` for siblings.
  - **The junk reason keys and their English text:** `crates/classify/src/junk.rs` lines 51–91.
  - **The primitives:**
    - `web/src/lib/ui/overlays/Drawer.svelte`: non-modal, controlled `width`, `onWidthChange`, `returnFocus`, focus to the title, Esc.
    - `web/src/lib/ui/lists/Tree.svelte` with `tree-model.ts`: `FlatNode {id, depth, name, dir, size?}`, `buildIndex`, worker-safe, an `index` prop for a prebuilt index.
    - `web/src/lib/charts/CategoryBar.svelte` and `HBarList.svelte`.
    - `web/src/lib/ui/labels/{Mono,Badge,RelativeTime}.svelte`.
    - `web/src/lib/ui/feedback/{Banner,Skeleton}.svelte` and `toasts.svelte.ts`.
    - `web/src/lib/format/`.
  - **The keyboard registry:** `web/src/lib/keys/README.md`.
  - **The test harness:** `web/e2e/README.md`, and `web/src/dev-routes/guard.ts` for a dev harness route.
  - **Decisions:** `docs/decisions/2026-10-22-W9.md` (budgets, lazy loading) and `docs/decisions/2026-10-22-B13.md` (siblings).
- Skills: the standards §3 "Any screen or primitive" row, plus `analytics-chart-selection` and `data-visualization-design` for the composition list.

## Own
- **`web/src/lib/features/detail/**` (new) and its tests:**
  - `DetailHost.svelte`: the drawer the search page mounts.
  - `DetailView.svelte`: the sections, shared by the drawer and the page.
  - `open.ts`.
  - `commands.ts` and `magnet.ts`.
  - `summary/`, `composition/`, `parsed/`, `raw/`.
  - `files/`: `build.ts`, `tree.worker.ts`, `FileTree.svelte`.
  - `messages/{en,zh}.json`, with the prefix `detail_`.
- `web/src/routes/t/**` (`+page.svelte`, `+page.ts`).
- `web/src/dev-routes/detail/**`: a harness page that opens the drawer.
- e2e:
  - `web/e2e/detail.routes.test.ts`, `web/e2e/detail.keyboard.test.ts`, and `web/e2e/detail.fixture.test.ts` (local only).
  - **the `torrent` object only** in `web/e2e/support/routes.ts`. S-search-1 edits the `search` object.
  - The regenerated `torrent` baselines, through the marker flow in `web/e2e/README.md`.
- `docs/decisions/<date>-S-detail.md`.
- **Hub lines (exactly these; you're their only editor in this batch):**
  - **`web/src/app.d.ts`:** `interface PageState { detail?: { hash: string } }` inside `namespace App`.
  - **`web/scripts/size-entries.mjs`:** an exported `chunkFiles(sourceKey)`. It returns the static-import closure of a dynamically imported module, minus what `routeFiles('/search')` already loads.
  - **`web/.size-limit.mjs`**, two entries:
    - Replace the "Detail-drawer chunk" placeholder comment with `entry('Detail-drawer chunk', [...drawer.js, ...drawer.css], '15 KB')`, where `drawer = chunkFiles('src/lib/features/detail/DetailHost.svelte')`.
    - Add the `/t/[hash]` route's own JS + CSS at 15 KB. The runner set the same budget as the drawer for this round. Use S-compare's `own(route)` helper if it's on `main`; otherwise use `routeFiles('/t/[hash]')` minus the search route's files.
    - S-compare adds other route entries and that helper to the same file in parallel. Touch only your two lines, and on a conflict merge `origin/main` and keep both.
  - **`web/src/lib/util/clipboard.ts` and `web/src/lib/util/magnet.ts`** (new; pure, no Svelte imports, per `lib/util/README.md`) and their tests. This is the shared Copy magnet helper: the runner gave it to S-detail. S-compare keeps a local copy with a TODO until yours merges.
  - **`web/src/lib/charts/HBarList.svelte`:** an optional per-item `color` (a CSS value; default `var(--data-1)`, so nothing changes for current callers), plus a test in `primitives.test.ts`. §10.2 needs the group colours to be the same in the bar and the list.
  - **`web/src/lib/ui/lists/tree-model.ts` and `Tree.svelte`:** an optional `FlatNode.marked?: boolean`, rendered as medium weight plus a "Matched" marker (icon and accessible text, never colour alone), with its label passed as a prop. Add a test in `lists.test.ts`.
- **Don't edit:**
  - `web/src/routes/search/**` and `web/src/lib/features/search/**` (S-search-1);
  - `web/src/lib/keys/**` (S-keys renames the default ids) and `web/src/lib/shell/**`;
  - `web/src/lib/state/**`, `web/src/lib/api/**` and `web/src/lib/i18n/messages/**`;
  - any other file in `lib/ui` or `lib/charts`;
  - `web/package.json`, `playwright.config.ts`, `lighthouserc*.json` and `app.html`.
- **Open PRs nearby:**
  - **#48 (M4)** adds `last_seen_at`, `seeders_approx` and `swarm_suspect` to `SearchHit` and `TorrentDetail`, and `swarm_trackers` to `TorrentDetail`.
    - If it merges while you work, merge `origin/main`, then add the Last seen row (`formatDateTime`), `≈` on seeders when `seeders_approx`, and the "Suspect seeders" label (icon plus text).
    - If it hasn't merged, leave those rows out (see step 4) and list them as a follow-up.
  - **#128 and #129** regenerate `openapi.json`/`schema.d.ts`. Never hand-merge those files. #130 merged as `57296e6`.
- **In-flight neighbours:**
  - **S-search-1** owns the search page, its drawer track and `ensureSearchPath()`.
  - **S-keys** renames `DEFAULT_KEYS` ids to the server's preference ids and owns `lib/keys`.
  - **S-compare and S-insights-1** may also want chart changes. `HBarList.svelte` is yours in this batch; the runner sequences any other edit to it.

## Build
1. **The contract with Search.** S-search-1's brief matches this, and you never edit each other's files.
   - **`openDetail(hash, { hit?, returnFocus?, replace? })`** (in `open.ts`):
     - caches `hit` (a `SearchHit`) by hash;
     - calls `pushState('/t/' + hash + page.url.search, { detail: { hash } })`, or `replaceState` with `replace` (j/k, from S-search-2);
     - remembers `returnFocus`.

     The query string is kept so the search page, which stays mounted under shallow routing, still reads its query.
   - **`closeDetail()`:** `history.back()` when the current entry is the one `openDetail` pushed; otherwise `replaceState('/search' + page.url.search, {})`.
   - **`prefetchDetail(hash)`:** after 150 ms idle, `GET /torrents/{hash}` into an LRU of 50. A newer call cancels a pending one. S-search-2 calls it on hover, focus and j/k.
   - **`DetailHost.svelte`:**
     - The search page imports it through `import.meta.glob` and renders it in its trailing grid track. It shows the drawer while `page.state.detail` is set.
     - **It positions itself:**
       - `xx-large` and up: in flow, in the track;
       - `x-large`: an overlay of `--drawer-overlay-width` over the results' trailing edge, with the list still visible;
       - `large`: `--drawer-overlay-width-narrow` or full width;
       - below `large`: a full-screen view with a Back button that calls `closeDetail()`.
     - **Width:** the width from `onWidthChange` persists in `localStorage` `lodestar.drawer.width` (a per-viewer convenience). Wrap it in try/catch, and fall back to the token width.
     - **Focus and Esc:** focus goes to the title on open, and Esc and close return focus through `returnFocus` (the primitive does this).
     - **Changing torrent:** when `page.state.detail.hash` changes (j/k), it swaps the content without moving focus, and announces the new title in a polite live region.
     - **Back:** the browser's Back (popstate) closes it.
   - **`routes/t/[hash]`:**
     - `+page.ts` lowercases the hash and checks `^[0-9a-f]{40}$` (else `error(404)`, the shell's 404 screen).
     - `+page.svelte` renders `DetailView` as a page: the title as `<h1>`, in a column capped at `--manuscript-width`. Record that choice.
2. **Data and stale responses.**
   - `GET /torrents/{hash}` comes through the prefetch cache, else W5's `latest()` slot, so a slow response for an earlier hash never overwrites a newer one. Every request aborts on close or on a hash change.
   - **The summary fields `TorrentDetail` doesn't carry yet** are `title`, `resolved_via`, `content_type`, `adult_kind`, `composition`, `junk_score`, `junk_reasons` and `matched_files`. They come from the cached `hit` when opened from Search.
     - On a direct load: `GET /search?q=hash:{hash}&limit=1&hidden=true`, its first hit if its `hash` matches. This is the interim until `TorrentDetail` carries them (metrics §7.1 "planned on detail"); record it.
     - With neither, those rows show their null states.
   - **`identity`** comes from `GET /torrents/{hash}/siblings`, fetched when the Parsed section first becomes visible.
   - **Error states:**
     - **404 `torrent_not_found` or 400 `bad_hash`:** "This torrent isn't in the index", with "Back to Search".
     - **404 `adult_hidden`:** "This torrent is Adult, and Show adult content is off", with a link to `/settings/appearance` (provisional until S-appearance; say so).
     - **Network or 5xx:** an error `Banner` with Retry; the header from the hit stays.
     - **401** is the shell's.
3. **The header:**
   - **Title:** `title ?? name` (the primitive's 20 px heading).
   - **Raw name:** shown in `Mono`, `text-small`, when it differs. Add "resolved via files|folder|structure" from `resolved_via`, as glossary `resolved_title` describes.
   - **One primary action: Copy magnet.** "Send to qBittorrent" waits for a download-client endpoint; none exists, so name it as a follow-up.
   - **Secondary:** an "Open full page" link to `/t/{hash}`. Favorite, Hide, Labels, Fix parse, Block this… and Not junk / Not adult are follow-ups; list them. Don't build a one-item menu.
4. **Summary facts** (§10.5.1, `metrics.md` §4). Every number has a `data-testid` and goes through W8:
   - **Size:** `formatBytes`.
   - **Files:** `formatCount`.
   - **Discovered:** `formatDateTime`, plus `m.backfill()` when `flags` has `backfill`.
   - **Seeders and leechers:** `formatCount`. A null is "—" with the accessible name `m.never_scraped()`, and the drawer shows the words "Never scraped". A 0 is "0".
   - **Scrape age:** `m.scrape_age({ relative: formatRelative(...) })` through `RelativeTime`, with the absolute time in its tooltip.
   - **Trackers:** "Reported by {n} of {m} trackers checked", with each number through `formatCount`, or "—" when null.
   - **Piece size:** `formatBytes(piece_len)`.
   - **Flags:** `private`, `v2`, `hybrid` and `files_partial` as `Badge`s, icon plus text.
   - **Junk:** `formatScore(junk_score)` ("—" when null), plus each reason as text from messages (the 9 keys in `junk.rs`). An unknown key is shown as itself.
   - **Rows for fields not on `main`** (Last seen, suspect seeders) are left out, not shown as "—". A "—" would claim the value was measured. List them for #48.
5. **Contents: composition** (§10.5.2):
   - **The bar:** a `CategoryBar` with the groups in the fixed §10.18 order, its text summary, and the dominant share as "{share} {group}" (`formatShare(pct_bytes)`).
   - **The list:** an `HBarList` of the groups, sorted by bytes (the primitive sorts), with:
     - `value` = `bytes` (through `format: formatBytes`);
     - `share` = `pct_bytes / 100`, the unit change `metrics.md` §3 allows;
     - a label that includes the file count;
     - `color` from `GROUP_COLOR` (`lib/charts/shared.ts`).
   - **No arithmetic on counts.**
   - **An empty composition** (padding only, or not classified) shows one line of text, not an empty chart.
   - **Group labels.** If the shared messages (`web/src/lib/i18n/messages/en.json`) have keys for the seven groups and a `dominant_share` message when you start, use them. The runner decides them once all this round's briefs are written. Today they don't exist: there are category keys, but no `images`, `documents` or `archives` groups and no `dominant_share`. Until then use `detail_group_*` and `detail_dominant_share`, and list them in the PR for consolidation. S-compare does the same with `compare_*`. Don't edit the shared files.
6. **Contents: the file tree** (§10.5.2, §10.1):
   - **Loading:**
     - Start from `file_list`. When `file_list_total > file_list.length`, page `GET /files?offset=&limit=1000` with at most 2 requests in flight, aborted on close or a hash change.
     - Load lazily, when Contents first becomes visible.
     - Show `aria-busy` and "Loading all {total} files…" (`formatCount(file_list_total)`) while paging.
     - A page that fails shows a Banner with Retry; whatever has loaded so far stays.
   - **`build.ts`** (pure, no DOM; it runs in the worker and the tests):
     - turns paths into `FlatNode[]`: directories before files, each by `localeCompare(…, { numeric: true })`, with `id` = the path;
     - gives folder sizes as the **exact integer sum of their files' server sizes**, then `formatBytes`. This structural sum is what §10.5 asks for (the primitive's `leaves` works the same way). Record it as a refinement; there are no percentages;
     - hides padding files by default, with a "Show padding files" toggle and no count;
     - shows `executable` and `symlink` as small text badges;
     - marks the nodes in `matched_files` (positions in torrent order) as `marked`, and opens their ancestors by default;
     - filters by a case-insensitive substring of the path, keeping the ancestors of matches.
   - **Above 2,000 files** (`file_list_total`), `build.ts` and `buildIndex` run in `tree.worker.ts`:
     - `new Worker(new URL('./tree.worker.ts', import.meta.url), { type: 'module' })`: a same-origin file, so the server's CSP `script-src 'self'` allows it. **Never a `blob:` URL.**
     - It posts `nodes` plus the `TreeIndex` typed arrays (transferred), and the filter re-runs there, debounced 120 ms.
     - The worker is terminated on close.
     - Set `data-tree-worker` on the tree root, so tests can tell which path ran.
   - **The tree:** `Tree` with `--row-height-compact` rows and no checkboxes. "Send selected files" needs a send endpoint; it's a follow-up.
   - **Missing data:** there are no per-file category icons, because `FileEntry` has no category and the client must not classify extensions. Report it as a backend field.
7. **Parsed metadata** (§10.5.3):
   - **Resolved title:** with provenance: "resolved via …", or "from: name" when `resolved_via` is null.
   - **Type:** `content_type`, plus the adult type when it's adult.
   - **Years, seasons, episodes and absolute numbers:** as the server's spans (`identity`), with no provenance, because the server sends none.
   - **Resolution, codecs, languages, group and edition aren't served** for one torrent (only Compare's `attributes` has them). Leave those rows out and report the backend field needed: a `release` object on `TorrentDetail`, with per-field provenance. "Fix parse" is a follow-up.
   - **Lazy:** mounted on first visibility, with `content-visibility: auto` and a `contain-intrinsic-size` written with tokens in a feature CSS file.
8. **Raw viewer** (§10.5.6):
   - **Build it:** `raw/RawViewer.svelte`, a lazy chunk. It's a collapsible, mono tree for any JSON value (`unknown`, not a local DTO): text only, long strings cut with "Show all", depth-limited, with keyboard disclosure.
   - **Don't mount it in the drawer.** No endpoint serves an info dict: the archive is opt-in, §7 `torrent_raw`, and §10.10.2's rule is never to show an empty placeholder.
   - Show it on the dev harness and test it.
   - Report the backend follow-up: `GET /torrents/{hash}/raw`, decoded on the server, without `pieces` (Opus).
9. **Copy and open magnet; the keys.**
   - **The copy helpers:**
     - `lib/util/magnet.ts` exports `isMagnet(s)`, which checks `^magnet:\?`.
     - `lib/util/clipboard.ts` exports `copyText(text): Promise<'copied' | 'manual'>`. When `window.isSecureContext`, it uses `navigator.clipboard.writeText`; otherwise, or if that throws, a hidden-textarea `execCommand('copy')`.
     - On `'manual'`, the feature's `features/detail/magnet.ts` shows the magnet in a selected read-only `Mono` field with "Press Ctrl+C to copy" (⌘C on a Mac). On `'copied'`, it calls `toast.push({ kind: 'success', text })`.
     - A plain-HTTP LAN address (the usual Unraid setup) isn't a secure context, so the fallback is the normal path there. Record that.
     - Document the two exports in `lib/util/README.md`, for S-compare and the row actions.
   - **Open magnet** is an `<a href={magnet}>`, after the same check.
   - **`commands.ts`** exports `registerDetailCommands()`, idempotent, through `import('$lib/keys')`:
     - It registers `detail.copy_magnet` (`keys: ['c']`) and `detail.open_magnet` (`keys: ['o']`). These are the server's preference ids (`crates/lodestar/src/settings/preferences.rs` `DEFAULT_SHORTCUTS`), which S-keys makes the `DEFAULT_KEYS` ids.
     - Both are global, with `when: () => target() !== null`.
     - Their target is the open drawer's torrent, else whatever `setCommandTarget(hash | null)` was given. S-search-2 sets it from the focused row and **must not register these ids again**; say so in the PR.
     - Don't register `detail.send` until a send endpoint exists.
10. **Budgets:**
    - **CSS:** the global 20 KB sheet is the owner's number. Use only utilities the build already emits, and write anything new in component `<style>` blocks with `@reference` to `app.css` and `@apply`, so it lands in the drawer's chunk. If it doesn't fit, stop and ask.
    - The drawer chunk is ≤ 15 KB gzipped (the new size-limit entry), and so is the `/t/[hash]` route's own JS + CSS. It covers the host, the view and the sections, but not the worker, the raw viewer or anything the search route already loads.
    - The search route's three entries must not change by more than 0.1 kB, because the glob keeps the drawer out of them. Check the manifest.
    - Report the sizes of the worker chunk and the `/t` route.
    - If 15 KB doesn't fit, lazy-load Parsed and Composition behind visibility first. If it still doesn't fit, stop and report the numbers.
11. **Messages:** en in full, using the shared glossary keys (`m.seeders()`, `m.leechers()`, `m.never_scraped()`, `m.scrape_age()`, `m.backfill()`, `m.trackers_reporting()`, `m.composition()`, `m.raw_name()`, `m.resolved_title()`, the category keys). zh proposals for your own keys only.
12. **Decide yourself, and record them:**
    - the page column;
    - the storage key and the width fallback;
    - directory ordering;
    - folder sums;
    - the interim hash lookup;
    - the fallback copy UX;
    - section lazy-loading;
    - message names.

    **Stop and ask:**
    - any budget overrun;
    - a primitive change beyond the two named;
    - a new token or dependency;
    - inventing a field, provenance or category the server doesn't send;
    - any backend change.

## Tests
- **vitest:**
  - **`build.ts`:** nesting and sort order; exact folder sums; padding hidden and shown; `marked` from `matched_files`; the filter with its ancestors; duplicate, empty and leading-`/` paths; CJK names; 50,000 files under 1 s in jsdom.
  - **The worker:** its message handler tested as a function, plus the 2,000-file threshold.
  - **`open.ts`:** push versus replace; close by back versus replace; prefetch debounce, LRU and latest-wins.
  - **`magnet.ts`:** a secure context, the fallback, the manual field, a bad scheme.
  - **The summary:** null, 0 and a number for each swarm field, with the exact strings ("—", "Never scraped", "Reported by 4 of 20 trackers checked", "Seen 3 hours ago", "First seen during backfill"), and every `data-testid` present.
  - **Composition:** order, colours, shares, empty.
  - **The error states:** 404 ×2, 400, network.
  - **The commands:** registered once, `when`, the target.
  - **`RawViewer`:** nesting; a `<script>` string shown as text; truncation.
  - **The two primitive props.**
- **Playwright** (mock harness, on a private port: a copied config with `--strictPort`):
  - **`detail.routes.test.ts`:**
    - `/t/{hash}` with mocked `getTorrent`, `listTorrentFiles`, `searchGet` (the hash lookup) and `getTorrentSiblings`;
    - a 3,000-file torrent through paged `listTorrentFiles` builds in the worker (`data-tree-worker`), and its filter works;
    - the `torrent_not_found`, `adult_hidden` and network states;
    - axe passes (light and dark, 1366 and 390);
    - on `/dev/detail`: open from a button, focus on the title, Esc returns focus, the resize handle by keyboard, the width kept across a reload; the overlay at 1024 and the full-screen view with Back at 390.
  - **`detail.keyboard.test.ts`** (with `context.grantPermissions(['clipboard-read', 'clipboard-write'])`): open, `c` copies (read the clipboard back), `o`'s href, arrow keys in the tree.
  - **The `torrent` route entry's `mock`.**
- **Fixture backend** (`detail.fixture.test.ts`, local only; skipped without `E2E_FIXTURE_URL`):
  - Run it against `cargo run -p lodestar -- serve --fixture <tmp>` on a private port (`bench/README.md`). That's the real binary, so the CSP applies to the worker.
  - Take hashes from the fixture's `fixture.json`.
  - The 1,200-file "Field Recordings Archive" loads every page.
  - A Lantern Keeper version shows its composition (from the hash lookup).
  - A never-scraped torrent shows "Never scraped".
  - The Adult torrent with Adult hidden gives the `adult_hidden` state.
  - Report the results.
- **Baselines:** the `torrent` key screen changes, so use the marker flow.

## Done when
- These pass in `web/` after merging `origin/main`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit` (including the new drawer entry) and `npx playwright test`.
- Axe is clean.
- No hex, arbitrary or off-ramp values, and strings only in `features/detail/messages`.
- **About 1,500 changed lines or fewer, baselines excluded.** If it's bigger, split it into two PRs in sequence:
  1. the host, open, the view, the header, the summary, magnet, the commands and `/t`;
  2. composition, the tree and worker, parsed, and the raw viewer.
- A PR titled `S-detail: Detail drawer and /t/{hash} with file tree worker`.

## Report
The PR description lists:
- **The API:** the exports of `open.ts` and `commands.ts`, for S-search-2, and how `DetailHost` behaves at each breakpoint.
- **Data:** each field it reads and where from (detail, the cached hit, the hash lookup, siblings); how each null shows; the rows left out and why.
- **Measurements:** the drawer chunk, worker and `/t` sizes; the search route's sizes unchanged; axe results; the fixture-backend run.
- **Hub edits:** each one, by file.
- **Backend follow-ups:**
  - the summary and release fields on `TorrentDetail`;
  - per-file `category`;
  - the raw info-dict endpoint;
  - a send endpoint;
  - #48's fields, if it hasn't merged.
- **UI follow-ups:** the Swarm, Related and Notes & labels sections, and the secondary menu.
- **Messages:** the glossary-candidate keys.
- **Owner decisions:** any refinements (§2.19).

## Runner decisions (2026-10-23, apply over anything above)
- **CSS (owner's §10.1 limit, unchanged):** global CSS is 19.82 of 20 KB and nine web slices are in flight, so treat the headroom as zero. Don't add Tailwind utility classes that aren't already in main's built CSS. Put everything route-only in a component `<style>` block with `@reference` to `app.css`, using token variables and ramp spacing only (no px, no hex). It ships in the route chunk. Report `npx size-limit`'s CSS figure before and after. If it moves at all, say why in the PR; if you can't stay under 20 KB after merging main, stop and ask.
- **Route budgets (runner, §2.19):** each route's own JS + CSS is ≤ 15 KB gzipped (compare, discover, alerts, alerts/[id], t/[hash], drawer), and Insights is ≤ 45 KB including uPlot (§10.1). Dialogs and heavy pieces load lazily.
- **Fixture backend:** CI uses the OpenAPI mock. Do one manual check against `serve --fixture` on a private port and report it. Fixture specs stay opt-in through `E2E_FIXTURE_URL`; S-e2e will wire them into CI.
- **Shared files:** edit only the lines this brief assigns you in a shared file. On a conflict, merge `origin/main` and regenerate generated files (messages, OpenAPI, size entries) with the repo's tooling; never hand-merge them.
- **On main since this brief was drafted:** W9 (b8c884d) and B11b part 2 (#130, 57296e6: the Insights Δs, `export.csv` and the `insights` SSE event). M4 (#48, swarm fields) is still in review: use its fields only if it has merged when you start, otherwise use the null state.
- **Shared message keys:** you add the missing shared keys to `web/src/lib/i18n/messages/{en,zh}.json`, with glossary wording: the group labels, the content-type value labels and `dominant_share`. M-settings-form adds `setting_*` keys to the same file. On a conflict, merge main and keep both. S-compare, S-discover and S-alerts use feature-local stand-ins with a TODO until yours merge.
- **One line in `web/src/lib/shell/nav.ts`:** mark Search as current at `/t/{hash}`.
- **Out of scope here, now carry-forwards:** the drawer's Swarm, Related and Notes & labels sections, and the secondary menu (Favorite, Hide, Labels, Fix parse, Block this…). Leave named, empty section slots for them.
