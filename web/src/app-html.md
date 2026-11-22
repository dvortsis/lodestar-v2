# Notes on `src/app.html`

These notes were comments in `src/app.html`. HTML comments ship in every first load, so they live here instead (X-search-chunk-retry; the first load's byte budget is §10.1). Each heading names the element the note describes.

## `<head>`, before `<title>`: why there is no font preload

No font preload: it competes with the scripts and CSS that paint the page, and took the mobile LCP from 2.4 s to 2.7 s (§10.1). The font is fetched at the first layout of text and swaps in (font-display: swap); see docs/decisions/2026-10-22-W9.md.

## `<body>`, before `<div id="boot">`: the first-paint shell

First-paint shell. With ssr off, nothing would paint until the Svelte runtime, kit and app chunks have run, so LCP would grow with them (§10.1). This markup paints from HTML and CSS alone: the skip link, the top bar's brand and links (real anchors, which kit's router takes over once it starts) and, on /search, the page heading. When the app mounts, lib/shell/Shell.svelte moves #boot-header into its own header slot and removes #boot in the same task, so nothing moves. It mirrors lib/shell/TopBar.svelte; the guard test e2e/boot-shell.routes.test.ts fails if the two drift. English only: the script below drops it when the app will show another language (same rule as lib/i18n/locale.svelte.ts) and on the sign-in screens.

## `<main id="boot-main">`, before `<div id="boot-h1">`: the /search heading and query bar

/search only (the script below unhides it): the page heading and the query bar, box for box as routes/search and features/search/query/QueryBar.svelte draw them, so the bar is the first paint, not a late LCP. Inert: the app's own bar takes typing.
