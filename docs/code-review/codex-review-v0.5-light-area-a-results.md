# Lodestar v0.5 light review: Area A results

Date: 2026-10-24

Instructions: [Codex light review](codex-review-v0.5-light.md), **Area A: web UI behaviour**.

Reviewed a fresh clone of `main` at `2d5e761d30cb1b7947fb09493959668eabcb84ab`. Locations refer to that commit. The review was read-only; this report was subsequently saved at the owner's request. No fixes, commits, pushes or GitHub comments were made.

## Validation and limits

- Read the owner decisions, runner standards, architecture §10, glossary, metric and token contracts. Traced Search, detail, Discover, Compare, Alerts, auth and keyboard flows through their shared state and primitives.
- **141 focused tests passed across nine files on rerun.** The initial run had six Compare rendering timeouts; those failures did not reproduce after compilation warmed. Tests used the existing workspace dependencies after a directory comparison confirmed `web/src` was identical to the fresh clone.
- Test files: `search/results/results.test.ts`, `search/results/ResultsPanel.test.ts`, `detail/DetailView.test.ts`, `detail/nav.test.ts`, `discover/DiscoverPage.test.ts`, `compare/Compare.test.ts`, `alerts/controllers.test.ts`, `auth/forms.test.ts`, `shortcuts/boot.test.ts` (relative to `web/src/lib/features/`).
- Isolated Node probes compiled the production Svelte runes and exercised the detail cache and Search results store. A small compiled Svelte lifecycle probe verified conditional-host teardown; the installed SvelteKit shallow-popstate implementation was also inspected. These were not full browser reproductions.
- Excluded swarm sorts because `2026-10-24-S-search-3.md` explicitly defers them. Other explicitly deferred features were not treated as new bugs merely because they are missing.

### 1. Cached adult torrents remain visible after adult content is disabled

Location: `web/src/lib/features/detail/model.svelte.ts:38`\
What happens: Open an adult torrent, disable adult content through the settings API, then revisit the torrent without reloading the app. `load()` adopts `cachedHit` and `cachedDetail` and skips their requests. A probe returned `status: ready`, the cached adult title and body, and zero API requests despite the server now denying access. After the detail body expires, a server `adult_hidden` response still leaves the cached hit title available to the header and raw-name region. `clearDetailCaches()` has no production callers.\
Expected: Adult items disappear when **Show adult content** is off, per architecture §20.9 and the review brief. Revalidate or invalidate cached display data when visibility changes, and clear identifying cached metadata on denial.\
Confidence: high

### 2. First-run setup ends on a page with no way forward

Location: `web/src/routes/welcome/+page.svelte:1`\
What happens: A successful admin-password submission calls `toWelcome()` (`SetupForm.svelte:90`). The destination renders only `<h1>Welcome</h1>`. The shell treats `/welcome` as a bare route, so no normal navigation is available. There is no Continue, Done or Search action.\
Expected: Continue the wizard or provide a usable completion action, per architecture §10.10.10 and the M-auth decision that the remaining steps live on `/welcome`. This finding concerns the exposed dead end, rather than requiring every planned wizard step immediately.\
Confidence: high

### 3. Browser Back closes the detail drawer without restoring focus

Location: `web/src/routes/search/+page.svelte:119`\
What happens: Open a torrent from Search, then use browser Back while focus is inside the drawer. Clearing `page.state.detail` makes the parent unmount `DetailHost`. Its child's effect cannot then execute the closed branch in `DetailHostBody.svelte:89–102`, where focus restoration lives. The lifecycle probe observed only the open effect and cleanup; focus became `BODY`. SvelteKit's shallow-popstate branch updates state and scroll without restoring the invoking element.\
Expected: Return focus to the invoking row, as specified by architecture §10.5 and the comment at `DetailHostBody.svelte:93`. Restore focus during teardown or keep the host mounted long enough to process closure.\
Confidence: high

### 4. Discover's See all in Search opens an empty screen

Location: `web/src/lib/features/discover/tabs.ts:67`\
What happens: On Discover's **All** tab, choose **See all in Search**. The link is `/search?sort=-discovered`, with no `q`. `ResultsPanel` supplies a null AST for an absent query and `results.svelte.ts` treats null/All as the empty screen, sending no search request. The store probe returned `status: empty`, zero hits and no API calls.\
Expected: Show the torrents represented by Newest, matching `searchHref()`'s comment that both views run the same query. Preserve the deliberate no-query landing page while providing an explicit browse destination or supported query for this link.\
Confidence: high

Checked and clean: `auth/token.ts`, `auth/next.ts`, `auth/form.svelte.ts`, `shortcuts/boot.ts`, `alerts/detail.svelte.ts`, `alerts/list.svelte.ts`, `alerts/since.ts`, `detail/files/load.ts`, `detail/files/build.ts`, `detail/summary/Summary.svelte`, `search/results/ResultRow.svelte`, `search/facets/toggle.ts` (relative to `web/src/lib/features/`; no additional defects identified in this light pass).
