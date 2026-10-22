// §10.1 budgets, gzipped. Size only: the preset-app timing step is disabled (`running: false`).
// Route budgets come from the SvelteKit build manifest (scripts/size-entries.mjs), so later
// route chunks never count against the search route.
import { existsSync, readdirSync } from 'node:fs';
import { routeFiles } from './scripts/size-entries.mjs';

const search = routeFiles('/search');

const FONT_DIR = 'build/_app/immutable/assets';
const fonts = existsSync(FONT_DIR)
	? readdirSync(FONT_DIR)
			.filter((f) => /^atkinson-hyperlegible-next-latin-wght-normal\..*\.woff2$/.test(f))
			.map((f) => `${FONT_DIR}/${f}`)
	: [];

const entry = (name, path, limit) => ({ name, path, limit, gzip: true, running: false });

export default [
	entry('Search route JS (initial)', search.js, '45 KB'),
	entry('CSS', search.css, '20 KB'),
	// Total first load: html + search JS + CSS + first-paint fonts. Fonts join automatically
	// once they are wired (the Latin subset of Atkinson Hyperlegible Next appears in build/).
	entry('Total first load', ['build/index.html', ...search.js, ...search.css, ...fonts], '100 KB'),
	// Fonts on first paint, Latin subset. Placeholder until the font slice wires them.
	...(fonts.length ? [entry('Fonts on first paint', fonts, '35 KB')] : [])
	// Placeholders for later slices. Fill `path` from the manifest when the chunk exists:
	//   Detail-drawer chunk    <= 15 KB
	//   Insights chunk (uPlot) <= 45 KB
];
