// §10.1 budgets, gzipped. Size only: the preset-app timing step is disabled (`running: false`).
// Route budgets come from the SvelteKit build manifest (scripts/size-entries.mjs), so later
// route chunks never count against the search route.
import { existsSync, readFileSync } from 'node:fs';
import { checkFirstLoad } from './scripts/first-load.mjs';
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

const entry = (name, path, limit) => ({ name, path, limit, gzip: true, running: false });

// A route's own JS and CSS: everything its first visit fetches beyond the search route's first
// load, including its lazy chunks (dynamic imports, transitively). A route that defers its screen
// with `import()` would otherwise measure as its stub. Chunks the search route already loads are
// never walked into, so the entry's dynamic imports (every other route's node) are not followed.
const manifest = existsSync('.svelte-kit/output/client/.vite/manifest.json')
	? JSON.parse(readFileSync('.svelte-kit/output/client/.vite/manifest.json', 'utf8'))
	: {};
const keyOf = new Map(Object.entries(manifest).map(([key, chunk]) => [`build/${chunk.file}`, key]));
const own = (route) => {
	const mine = routeFiles(route);
	const shared = new Set([...search.js, ...search.css]);
	const seen = new Set();
	const files = new Set();
	const visit = (key) => {
		const chunk = manifest[key];
		if (!chunk || seen.has(key) || shared.has(`build/${chunk.file}`)) return;
		seen.add(key);
		files.add(`build/${chunk.file}`);
		for (const css of chunk.css ?? []) if (!shared.has(`build/${css}`)) files.add(`build/${css}`);
		for (const next of [...(chunk.imports ?? []), ...(chunk.dynamicImports ?? [])]) visit(next);
	};
	for (const f of mine.js) if (!shared.has(f) && keyOf.has(f)) visit(keyOf.get(f));
	for (const f of mine.css) if (!shared.has(f)) files.add(f);
	return [...files];
};

export default [
	entry('Search route JS (initial)', search.js, '45 KB'),
	entry('CSS', search.css, '20 KB'),
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
	// Measured 22.7 KB with its lazy chunks: the screen, both layouts, labels and formatters.
	entry('Compare route (own JS + CSS)', own('/compare'), '25 KB'),
	entry('Discover route (own JS + CSS)', own('/discover'), '15 KB'),
	entry('Alerts route (own JS + CSS)', own('/alerts'), '15 KB'),
	entry('Alert route (own JS + CSS)', own('/alerts/[id]'), '15 KB')
	// Placeholders for later slices. Fill `path` from the manifest when the chunk exists:
	//   Detail-drawer chunk    <= 15 KB
	//   Insights chunk (uPlot) <= 45 KB
];
