// Builds web/static/fonts/*.woff2 from the Fontsource masters (both SIL OFL).
// Run with `npm run fonts`; the output is committed so the build needs no font tooling.
// Weight axis is clamped to 400-600, the only weights the UI uses (design-tokens.md §8).
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import subsetFont from 'subset-font';

const SRC = 'node_modules/@fontsource-variable';
const OUT = 'static/fonts';

/** Unicode ranges as Fontsource ships them (also used in the @font-face unicode-range). */
export const RANGES = {
	latin:
		'U+0000-00FF,U+0131,U+0152-0153,U+02BB-02BC,U+02C6,U+02DA,U+02DC,U+0304,U+0308,U+0329,U+2000-206F,U+20AC,U+2122,U+2191,U+2193,U+2212,U+2215,U+FEFF,U+FFFD',
	'latin-ext':
		'U+0100-02BA,U+02BD-02C5,U+02C7-02CC,U+02CE-02D7,U+02DD-02FF,U+0304,U+0308,U+0329,U+1D00-1DBF,U+1E00-1E9F,U+1EF2-1EFF,U+2020,U+20A0-20AB,U+20AD-20C0,U+2113,U+2C60-2C7F,U+A720-A7FF'
};

/** Features kept: kerning, ligatures, tabular/lining numerals and zero. */
export const FEATURES = [
	'kern',
	'liga',
	'calt',
	'ccmp',
	'locl',
	'mark',
	'mkmk',
	'tnum',
	'lnum',
	'zero'
];

export const FILES = [
	{ family: 'atkinson-hyperlegible-next', subset: 'latin' },
	{ family: 'atkinson-hyperlegible-next', subset: 'latin-ext' },
	{ family: 'atkinson-hyperlegible-mono', subset: 'latin' }
];

function codepoints(ranges) {
	const out = [];
	for (const part of ranges.split(',')) {
		const [a, b = a] = part
			.replace(/U\+/g, '')
			.split('-')
			.map((h) => parseInt(h, 16));
		for (let c = a; c <= b; c++) out.push(c);
	}
	return out;
}

export async function build() {
	mkdirSync(OUT, { recursive: true });
	for (const { family, subset } of FILES) {
		const master = readFileSync(`${SRC}/${family}/files/${family}-${subset}-wght-normal.woff2`);
		const text = String.fromCodePoint(...codepoints(RANGES[subset]));
		const woff2 = await subsetFont(master, text, {
			targetFormat: 'woff2',
			variationAxes: { wght: { min: 400, max: 600 } },
			features: FEATURES
		});
		writeFileSync(`${OUT}/${family}-${subset}.woff2`, woff2);
		console.log(`${family}-${subset}.woff2: ${woff2.length} bytes`);
	}
}

if (import.meta.url === `file://${process.argv[1]}`) await build();
