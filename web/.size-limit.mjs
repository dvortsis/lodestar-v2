// §10.1 budgets, gzipped. Size only: the preset-app timing step is disabled (`running: false`).
// Route budgets come from the SvelteKit build manifest (scripts/size-entries.mjs), so later
// route chunks never count against the search route.
import { existsSync, readFileSync } from 'node:fs';
import { checkFirstLoad, preloadedJs } from './scripts/first-load.mjs';
import { routeFiles } from './scripts/size-entries.mjs';

const search = routeFiles('/search');

// Fonts are committed subsets (scripts/fonts.mjs) served from static/fonts -> build/fonts.
const FONT_DIR = 'build/fonts';
const font = (name) => `${FONT_DIR}/${name}.woff2`;
const firstPaintFonts = [font('atkinson-hyperlegible-next-latin')];
const monoFonts = [font('atkinson-hyperlegible-mono-latin')];

// Favicons are exempt: the head links to them (apple-touch-icon.png) are fetched by the browser
// chrome, not rendered on the page.
// §10.17 brand gate: no brand raster (/brand/**/*.{avif,webp,png}) may be in the first-load set.
// Scenes load lazily with the states that use them; a raster referenced from the html, the search
// route's JS or its CSS would be fetched (or preloaded) on first paint. Throwing here fails
// `npx size-limit` before any size is measured.
const BRAND_RASTER = /\/brand\/(?!favicons\/)[^"'`\s)]*\.(?:avif|webp|png)/g;
const firstLoad = ['build/index.html', ...search.js, ...search.css].filter((f) => existsSync(f));
const leaks = firstLoad.flatMap((file) =>
	[...new Set(readFileSync(file, 'utf8').match(BRAND_RASTER) ?? [])].map((ref) => `${file}: ${ref}`)
);
if (leaks.length) {
	throw new Error(`Brand rasters in the first-load set (§10.17):\n  ${leaks.join('\n  ')}`);
}

// §10.1: the search route's first load holds only modules it reaches itself (no other route's code,
// no Svelte runtime only another route uses). scripts/first-load.mjs explains the rules.
const strays = checkFirstLoad('/search', search.js);
if (strays.length)
	throw new Error(
		`The search route's first load carries code it doesn't use:\n  ${strays.join('\n  ')}`
	);

// The route table (kit's app.js), budgeted below.
const appEntry = search.js.find((f) => /\/entry\/app\.[^/]*\.js$/.test(f));
// No file on the first load lists JS to preload (scripts/first-load.mjs `routePreloads`): the route
// table's list grew with every route, and a route node's with every chunk its lazy parts use
// (S-search-2's results panel).
const preloaded = search.js.flatMap((f) =>
	preloadedJs(readFileSync(f, 'utf8')).map((js) => `${f}: ${js}`)
);
if (preloaded.length)
	throw new Error(
		`The search route's first load preloads JS it doesn't run yet:\n  ${preloaded.join('\n  ')}`
	);

const entry = (name, path, limit) => ({ name, path, limit, gzip: true, running: false });

// A route's own JS and CSS is its first view (runner decision, §2.19): static imports, plus dynamic
// imports that run at mount (a route that defers its screen with `import()` would otherwise
// measure as its stub). Chunks loaded only on interaction (opening a dialog or a menu) are left
// out with `onDemand`, a list of regexes on manifest keys: the walk stops at a matching chunk and
// `onDemandOf` measures what it stopped at, so the lazy code is still gated by its own entry.
// Chunks the search route already loads are never walked into, so the entry's dynamic imports
// (every other route's node) are not followed.
const manifest = existsSync('.svelte-kit/output/client/.vite/manifest.json')
	? JSON.parse(readFileSync('.svelte-kit/output/client/.vite/manifest.json', 'utf8'))
	: {};
const keyOf = new Map(Object.entries(manifest).map(([key, chunk]) => [`build/${chunk.file}`, key]));
const walk = (route, onDemand, collectOnDemand, beyond = []) => {
	const mine = routeFiles(route);
	const shared = new Set([...search.js, ...search.css, ...beyond]);
	const seen = new Set();
	const files = new Set();
	const later = new Set();
	const matches = (key) => onDemand.some((re) => re.test(key));
	const visit = (key, demanded) => {
		const chunk = manifest[key];
		if (!chunk || shared.has(`build/${chunk.file}`)) return;
		const demand = demanded || matches(key);
		const id = `${demand}:${key}`;
		if (seen.has(id)) return;
		seen.add(id);
		(demand ? later : files).add(`build/${chunk.file}`);
		for (const css of chunk.css ?? [])
			if (!shared.has(`build/${css}`)) (demand ? later : files).add(`build/${css}`);
		for (const next of [...(chunk.imports ?? []), ...(chunk.dynamicImports ?? [])])
			visit(next, demand);
	};
	for (const f of mine.js) if (!shared.has(f) && keyOf.has(f)) visit(keyOf.get(f), false);
	for (const f of mine.css) if (!shared.has(f)) files.add(f);
	return collectOnDemand ? [...later].filter((f) => !files.has(f)) : [...files];
};
const own = (route, { onDemand = [], beyond = [] } = {}) => walk(route, onDemand, false, beyond);
// A lazily imported module (a manifest key), measured the same way: what the search route doesn't
// already load, through static imports and mount-time dynamic imports. A shared chunk has no source
// key, so `onDemand` also tests `chunk:<its name>` (e.g. /^chunk:magnet$/).
// `beyond` lists files the view has already loaded when this chunk loads (e.g. `viewOf(...)`): they
// are left out like the first load's, so the entry measures only what this chunk adds.
const ownChunk = (sourceKey, onDemand = [], collectOnDemand = false, beyond = []) => {
	const shared = new Set([...search.js, ...search.css, ...beyond]);
	const seen = new Set();
	const now = new Set();
	const later = new Set();
	const matches = (key, chunk) =>
		onDemand.some((re) => re.test(key) || (!!chunk.name && re.test(`chunk:${chunk.name}`)));
	const visit = (key, demanded) => {
		const chunk = manifest[key];
		if (!chunk || shared.has(`build/${chunk.file}`)) return;
		const demand = demanded || matches(key, chunk);
		const id = `${demand}:${key}`;
		if (seen.has(id)) return;
		seen.add(id);
		const into = demand ? later : now;
		into.add(`build/${chunk.file}`);
		for (const css of chunk.css ?? []) if (!shared.has(`build/${css}`)) into.add(`build/${css}`);
		for (const next of [...(chunk.imports ?? []), ...(chunk.dynamicImports ?? [])])
			visit(next, demand);
	};
	visit(sourceKey, false);
	return collectOnDemand ? [...later].filter((f) => !now.has(f)) : [...now];
};
const onDemandOf = (route, onDemand) => walk(route, onDemand, true);

// Detail: the DetailHost module, and the sections that load when scrolled to (or on an error).
const DRAWER = 'src/lib/features/detail/DetailHost.svelte';
// The key commands load after mount (with the key registry and the magnet code), so they are on demand too.
const detailLazy = [
	/features\/detail\/(Contents|ErrorState|parsed\/Parsed)\.svelte$/,
	// Clipboard failure is an interaction, not part of opening the detail view.
	// Its UI and mount runtime remain included in the on-demand budget below.
	/features\/detail\/manual-mount\.ts$/,
	/features\/detail\/magnet\.ts$/,
	/^chunk:magnet$/,
	/features\/detail\/commands\.ts$/
];

// Settings: the form engine loads these on demand (its README), so a screen's first view stops at
// them: every control but the toggle and the text input (per type), instant mode, previews, the
// field menu, the ⌘S registration, copy's toast and the confirmation dialog. S-settings-1's screen
// route entry uses this list; "Settings controls (on demand)" gates what it leaves out.
const settingsOnDemand = [
	/features\/settings\/form\/(Number|Slider|Choice|Secret|Chips|Json)Control\.svelte$/,
	/features\/settings\/form\/(instant|preview|menu|save-key|copy)\.ts$/,
	/features\/settings\/form\/ConfirmDialog\.svelte$/
];
const settingsLazy = onDemandOf('/settings/[section]', settingsOnDemand);

// What another route can still add to /search's first load (runner, 2026-10-24):
// - The route table: one import and one dictionary entry per route, about 27 B gzipped. The budget
//   allows 1.26 KB (measured with only / and /search) plus 40 B a route, so a route that adds
//   more fails here.
// - The vendor chunk holds only modules /search reaches (the stray check above), but it is shared,
//   so another route that uses more of those modules' exports keeps them in it. Fully used, those
//   modules measured 31.07 KB (a build without tree-shaking, against 30.42 KB with every route on
//   main). The budget is that ceiling: another route can't pass it, and only a new dependency of
//   /search's own can (raise it then, on purpose).
const routeCount = existsSync('.svelte-kit/generated/client-optimized/app.js')
	? (readFileSync('.svelte-kit/generated/client-optimized/app.js', 'utf8')
			.match(/dictionary = (\{[\s\S]*?\});/)?.[1]
			.match(/"\/[^"]*":/g)?.length ?? 0)
	: 0;
const vendor = Object.values(manifest).find((chunk) => chunk.name === 'vendor');

// Search results (S-search-2): the panel, its row and its sheets, loaded on a query or the first
// interaction, plus what it loads with the first results (the row actions, the j/k commands and the
// key registry). Opening a result (the drawer, its commands), copying a magnet and the More menu load
// on demand: the Detail entries gate the first two, and the menu (Bits UI) is the shell's too.
const RESULTS = [
	'src/lib/features/search/results/ResultsPanel.svelte',
	'src/lib/features/search/results/results.css',
	// row.css shares a chunk with Alerts' detail page, so it has no key of its own.
	...Object.keys(manifest).filter((k) =>
		(manifest[k].css ?? []).some((c) => /\/row\.[^/]*\.css$/.test(c))
	)
];
const resultsOnDemand = [/features\/detail\//, /^chunk:magnet$/, /^chunk:Menu$/];
// What the view has loaded by the time the panel does (runner, 2026-10-24): the first load and the
// bar's lazy part, which a URL with a query engages at once. Not the shell's Chrome: on a pasted
// link it loads only with the first interaction, so the panel really does fetch what they share.
const viewOf = (...parts) => [
	...new Set(parts.flatMap(([key, onDemand]) => ownChunk(key, onDemand)))
];
const engagedSearch = viewOf([
	'src/lib/features/search/query/Live.svelte',
	[/query\/(FilterStrip|SyntaxPopover)\.svelte$/]
]);
const resultsLazy = [
	...new Set(RESULTS.flatMap((k) => ownChunk(k, resultsOnDemand, false, engagedSearch)))
];
// The facet rail (S-search-2b): its host loads with the results, and the rail with it where the
// rail shows (x-large); below that, on the first Filters press, which also brings the sheet (on
// demand). Measured beyond the engaged view and the results panel it loads with.
const FACETS = [
	'src/lib/features/search/facets/FacetsHost.svelte',
	'src/lib/features/search/facets/FacetRail.svelte'
];
const facetsOnDemand = [/ui\/overlays\/Sheet\.svelte$/];
const facetsLazy = [
	...new Set(
		FACETS.flatMap((k) => ownChunk(k, facetsOnDemand, false, [...engagedSearch, ...resultsLazy]))
	)
];
// The results toolbar (S-search-3): loads with the results and shows once there is a query.
// Save search's dialog (the alerts', with Bits UI's overlay code) loads on the first press.
const TOOLBAR = [
	'src/lib/features/search/toolbar/ResultsToolbar.svelte',
	'src/lib/features/search/toolbar/toolbar.css'
];
const toolbarOnDemand = [/features\/alerts\/SaveSearchDialog\.svelte$/];
const toolbarLazy = [
	...new Set(
		TOOLBAR.flatMap((k) => ownChunk(k, toolbarOnDemand, false, [...engagedSearch, ...resultsLazy]))
	)
];

// Never pass by measuring nothing: a renamed or moved file must fail here, not drop the entry.
if (Object.keys(manifest).length && !resultsLazy.some((f) => f.endsWith('.js')))
	throw new Error('Search results (lazy): no chunk found for ResultsPanel.svelte in the manifest');
if (Object.keys(manifest).length && !facetsLazy.some((f) => f.endsWith('.js')))
	throw new Error('Facet rail (lazy): no chunk found for the facet rail in the manifest');
if (Object.keys(manifest).length && !toolbarLazy.some((f) => f.endsWith('.js')))
	throw new Error('Results toolbar (lazy): no chunk found for the toolbar in the manifest');

// Alerts: the save and delete dialogs load when one is opened, and bring Bits UI's overlay code.
const alertDialogs = [/features\/alerts\/(SaveSearch|DeleteAlert)Dialog\.svelte$/];
// The alert page's j/k (and the key registry) load after mount, as Detail's commands do.
const alertOnDemand = [...alertDialogs, /features\/alerts\/commands\.ts$/];

export default [
	entry('Search route JS (initial)', search.js, '45 KB'),
	entry('CSS', search.css, '20 KB'),
	...(appEntry
		? [entry('Route table (app.js)', [appEntry], `${(1.26 + 0.04 * routeCount).toFixed(2)} KB`)]
		: []),
	...(vendor ? [entry('Search vendor chunk', [`build/${vendor.file}`], '31.1 KB')] : []),
	// Total first load: html + search JS + CSS + the Latin font (fetched at first layout, not preloaded).
	entry(
		'Total first load',
		['build/index.html', ...search.js, ...search.css, ...firstPaintFonts],
		'100 KB'
	),
	// Latin subset of Atkinson Hyperlegible Next, fetched at first layout; a preload costs LCP (§10.1).
	entry('Fonts on first paint', firstPaintFonts, '35 KB'),
	// Mono loads lazily and is never preloaded (§10.1).
	entry('Mono font (lazy)', monoFonts, '25 KB'),
	// Route budgets (runner decision, owner's §10.1 headroom rules): each route's own JS + CSS.
	// Measured 25.96 KB with its lazy chunks (the screen, both layouts, labels, formatters, and the
	// Svelte runtime only this route uses since #132 moved it out of /search's first load); 28.06 KB
	// after #139 and #154 regrouped the shared chunks it pulls in (Button, Badge, transitions,
	// messages). Limit: measured + 10%.
	entry('Compare route (own JS + CSS)', own('/compare'), '31 KB'),
	// Measured 42.8 KB: over the 15 KB target because the screen is a virtualised list (12 KB with
	// the Svelte runtime only lazy routes use), an 11 KB page with both languages' messages, the key
	// registry, the API client and the formatters. Limit: the §10.1 cap for route JS.
	entry('Discover route (own JS + CSS)', own('/discover'), '45 KB'),
	entry('Alerts route (own JS + CSS)', own('/alerts', { onDemand: alertDialogs }), '29 KB'),
	// Alerts: first view 26.11 KB (limit = measured + 10%). Alert: 41.2 KB, so 45 KB (the §10.1 cap) holds:
	// the virtual list for long hit lists (VirtualList, its virtualiser, the hit row and composition bar).
	entry('Alert route (own JS + CSS)', own('/alerts/[id]', { onDemand: alertOnDemand }), '45 KB'),
	// The dialogs' on-demand code (Bits UI overlays, Dialog, both dialogs), still gated.
	// The engine's on-demand code on a Settings screen, still gated: 34.34 KB with S-settings-1's
	// screen (limit = measured + 10%). Until a screen uses the engine there's nothing to measure.
	...(settingsLazy.length ? [entry('Settings controls (on demand)', settingsLazy, '38 KB')] : []),
	entry(
		'Alert dialogs (on demand)',
		[
			...new Set([
				...onDemandOf('/alerts', alertDialogs),
				...onDemandOf('/alerts/[id]', alertDialogs)
			])
		],
		'27 KB'
	),
	// Limits are measured + 10%. Over the 15 KB target (§10.1) because the first view carries the
	// Drawer primitive, ~40 messages, the Svelte runtime and key registry only this view uses so far, and
	// the format and API helpers; the tree and virtual list are in the on-demand entry.
	// Detail (S-detail): the first view is the header and the summary. Contents (composition and the
	// file tree with its virtual list), Parsed and the error state are separate chunks that load as
	// their section scrolls near or an error shows, each gated by the on-demand entry.
	// Beyond Live's mount set (runner, 2026-10-24: 41.5 KB, under the 45 KB per-view cap). VirtualList
	// and its virtualiser are 12 KB of it, the panel 5.2 KB, the row 3.9 KB, the row actions 1.9 KB;
	// the rest is what the shell's Chrome would bring too (the key registry, the event client).
	entry('Search results (lazy)', resultsLazy, '41.5 KB'),
	// Over the 15 KB target (§10.1; runner, 2026-10-24: 16.8 KB). The self-review added Release year,
	// the unavailable and stale-lease states, the staged-count announcement and day-snapped brushes.
	// The FacetRail chunk is 9.9 KB, the host 3.1 KB, facets.css 0.9 KB, Checkbox and input 1.1 KB.
	entry('Facet rail (lazy)', facetsLazy, '16.8 KB'),
	entry('Results toolbar (lazy)', toolbarLazy, '4.4 KB'),
	// 29 KB (runner, 2026-10-30, L3–L5 landing): visibility revalidation (L3) subscribes the drawer to
	// settings events, so the event client (2.1 KB) now loads with it until the shell loads it too.
	entry('Detail drawer (own JS + CSS)', ownChunk(DRAWER, detailLazy), '29 KB'),
	entry('/t/[hash] route (own JS + CSS)', own('/t/[hash]', { onDemand: detailLazy }), '22.4 KB'),
	entry(
		'Detail sections (on demand)',
		[...new Set([...ownChunk(DRAWER, detailLazy, true), ...onDemandOf('/t/[hash]', detailLazy)])],
		'33.9 KB'
	)
	// Placeholders for later slices. Fill `path` from the manifest when the chunk exists:
	//   Insights chunk (uPlot) <= 45 KB
];
