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

// A route's own first load: the files it loads beyond what the search route already loads.
const own = (route) => {
	const files = routeFiles(route);
	return {
		js: files.js.filter((f) => !search.js.includes(f)),
		css: files.css.filter((f) => !search.css.includes(f))
	};
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
	// Settings: a screen's route and the Overview, each at the owner's 45 KB route limit (§10.1,
	// 2026-10-23). The form engine is in the screen route; the Overview must not import it.
	entry(
		'Settings screen route (own JS + CSS)',
		[...own('/settings/[section]').js, ...own('/settings/[section]').css],
		'45 KB'
	),
	entry(
		'Settings Overview route (own JS + CSS)',
		[...own('/settings').js, ...own('/settings').css],
		'45 KB'
	)
	// Placeholders for later slices. Fill `path` from the manifest when the chunk exists:
	//   Detail-drawer chunk    <= 15 KB
	//   Insights chunk (uPlot) <= 45 KB
];
