// Theme compiler (architecture §10.18, token names from docs/design-tokens.md §1-4).
//
//   node --experimental-strip-types scripts/themes.ts [--dir <themes dir>] [--no-write] [--verbose]
//
// Reads themes/*.json (the Riverstone schema), derives every role token with OKLCH lightness
// shifts only, runs the contrast gates and writes:
//   src/lib/theme/critical.css   Riverstone light + dark, imported by app.css
//   src/lib/theme/prepaint.js    the pre-paint script, also inlined into src/app.html
//   static/themes.json           every other theme, fetched lazily
// Any gate failure prints a report and exits 1 (the build stops).
import { existsSync, readdirSync, readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
	bestContrast,
	contrast,
	deltaE,
	fromOklch,
	minPairwise,
	mix,
	shiftUntil,
	simulate,
	toOklch,
	type Cvd,
	type CvdModel
} from './color.ts';

// ---- schema -----------------------------------------------------------------------------

export type Mode = 'light' | 'dark';

export type Theme = {
	id: string;
	name: string;
	family: string;
	group: string;
	mode: Mode;
	license?: string;
	source?: string;
	colors: {
		background: string;
		surface: string;
		text: string;
		primary: string;
		accent: string;
		categorical: string[]; // Riverstone order: blue, sage, ochre, terracotta, slate, plum
	};
};

const HEX = /^#[0-9a-fA-F]{6}$/;
const COLOUR_KEYS = ['background', 'surface', 'text', 'primary', 'accent'] as const;
const GROUPS = ['official', 'accessibility', 'community', 'original'] as const;

/** One file per family, `themes/<id>.json`, as in docs/themes.md: a light and a dark palette. */
export type Palette = Theme['colors'];
export type Family = {
	id: string;
	name: string;
	group: (typeof GROUPS)[number];
	license?: string;
	source?: string;
	light: Palette;
	dark: Palette;
};

function validatePalette(raw: unknown, where: string): string[] {
	const errors: string[] = [];
	if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) {
		return [`"${where}" must be an object`];
	}
	const c = raw as Record<string, unknown>;
	for (const key of COLOUR_KEYS) {
		if (typeof c[key] !== 'string' || !HEX.test(c[key])) {
			errors.push(`${where}.${key} must be #RRGGBB`);
		}
	}
	const cat = c.categorical;
	if (!Array.isArray(cat) || cat.length !== 6) {
		errors.push(`${where}.categorical must hold exactly 6 colours`);
	} else {
		cat.forEach((v, i) => {
			if (typeof v !== 'string' || !HEX.test(v)) {
				errors.push(`${where}.categorical[${i}] must be #RRGGBB`);
			}
		});
	}
	return errors;
}

/** Schema errors for one parsed family file; empty when it is valid. */
export function validateFamily(raw: unknown): string[] {
	if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) return ['not a JSON object'];
	const t = raw as Record<string, unknown>;
	const errors: string[] = [];
	for (const key of ['id', 'name'] as const) {
		if (typeof t[key] !== 'string' || !t[key]) errors.push(`"${key}" must be a non-empty string`);
	}
	if (typeof t.id === 'string' && !/^[a-z0-9]+(-[a-z0-9]+)*$/.test(t.id)) {
		errors.push('"id" must be lowercase kebab-case');
	}
	if (!GROUPS.includes(t.group as (typeof GROUPS)[number])) {
		errors.push(`"group" must be one of ${GROUPS.join(', ')}`);
	}
	for (const key of ['license', 'source'] as const) {
		if (t[key] !== undefined && typeof t[key] !== 'string')
			errors.push(`"${key}" must be a string`);
	}
	return [...errors, ...validatePalette(t.light, 'light'), ...validatePalette(t.dark, 'dark')];
}

/** The two themes a family file defines: ids `<id>-light` and `<id>-dark`. */
export function expandFamily(f: Family): Theme[] {
	return (['light', 'dark'] as const).map((mode) => ({
		id: `${f.id}-${mode}`,
		name: `${f.name} ${mode === 'light' ? 'Light' : 'Dark'}`,
		family: f.name,
		group: f.group,
		mode,
		...(f.license ? { license: f.license } : {}),
		...(f.source ? { source: f.source } : {}),
		colors: f[mode]
	}));
}

// ---- derivation -------------------------------------------------------------------------

/** categorical[i] feeds data slot DATA_SLOT[i] (design-tokens.md §3). */
export const DATA_SLOT = [1, 5, 2, 3, 4, 6] as const;

/**
 * Fitted constants. Every rule below is "shift OKLCH lightness (and, for tints and dividers, mix
 * toward a theme colour)"; these numbers are tuned once so that Riverstone reproduces the §10.18
 * table, and every other theme inherits them. Light and dark differ because the same fraction
 * of lightness reads differently on a dark ground.
 */
const RULES = {
	light: {
		mutedShift: 0.298, // fraction of the text -> background lightness distance
		mutedHue: 0, // share of primary mixed into text before shifting
		dividerWith: 'accent' as const,
		dividerMix: 0.2,
		hoverMix: 0.08,
		selectedMix: 0.135,
		errorChroma: 1.58,
		errorRatio: 4.5
	},
	dark: {
		mutedShift: 0.194,
		mutedHue: 0.2,
		dividerWith: 'primary' as const,
		dividerMix: 0.315,
		hoverMix: 0.09,
		selectedMix: 0.16,
		errorChroma: 1.42,
		errorRatio: 4.75
	}
};

/** Error is the Software colour (terracotta) turned this many degrees redder (OKLCH hue). */
const ERROR_HUE_SHIFT = 12;

/** The star is the logo: it keeps its Riverstone value in every theme (design-tokens.md §4). */
const BRAND_STAR: Record<Mode, string> = { light: '#D49A4A', dark: '#E0A95F' };

export const TOKEN_KEYS = [
	'--color-base-100',
	'--color-base-200',
	'--color-base-300',
	'--color-base-content',
	'--color-primary',
	'--color-primary-content',
	'--color-secondary',
	'--color-secondary-content',
	'--color-accent',
	'--color-accent-content',
	'--color-neutral',
	'--color-neutral-content',
	'--color-info',
	'--color-info-content',
	'--color-success',
	'--color-success-content',
	'--color-warning',
	'--color-warning-content',
	'--color-error',
	'--color-error-content',
	'--color-accent-ink',
	'--color-muted-text',
	'--color-field-border',
	'--color-hover',
	'--color-selected',
	'--data-1',
	'--data-2',
	'--data-3',
	'--data-4',
	'--data-5',
	'--data-6',
	'--data-other',
	'--brand-surface',
	'--brand-primary',
	'--brand-accent',
	'--brand-star'
] as const;

export type Tokens = Record<(typeof TOKEN_KEYS)[number], string>;

const up = (hex: string) => hex.toUpperCase();

/**
 * Riverstone's published values (docs/design-tokens.md §1-4) are a contract, so they are pinned
 * byte for byte. The rules above land within ΔE_ok 0.5 of each of these (tested), one 8-bit
 * step away because the published values carry a little extra margin or hue drift.
 */
export const PINS: Record<string, Record<string, string>> = {
	'riverstone-light': {
		'--color-base-300': '#DCE4E1',
		'--color-accent-ink': '#5B7763',
		'--color-success': '#5B7763',
		'--color-muted-text': '#567079',
		'--data-other': '#567079',
		'--color-field-border': '#8796A2',
		'--color-neutral': '#8796A2',
		'--data-4': '#8796A2',
		'--data-5': '#7E9B86',
		'--color-hover': '#F1F5F6',
		'--color-selected': '#E7EDF0'
	},
	'riverstone-dark': {
		'--color-base-300': '#354851',
		'--color-muted-text': '#B2C3C5',
		'--data-other': '#B2C3C5',
		'--color-error': '#DC8080',
		'--color-hover': '#2A3B45',
		'--color-selected': '#30434D'
	}
};

/**
 * A label colour for text drawn on `fill` that reaches `min`:1, whatever the theme.
 *   1. the best of `preferred` (the theme's own grounds or text) when it already passes;
 *   2. otherwise the theme's text colour moved in lightness only (hue and chroma kept), in
 *      whichever direction reaches `min` with the smaller change;
 *   3. otherwise pure black or white, whichever contrasts more.
 * Step 2 is always reachable in practice, because black or white clears 4.58:1 on any fill.
 */
export function labelOn(fill: string, preferred: string[], text: string, min: number): string {
	const best = bestContrast(fill, preferred);
	if (contrast(best, fill) >= min) return best;
	const ok = (c: string) => contrast(c, fill) >= min;
	const shifted = (['darker', 'lighter'] as const)
		.map((direction) => shiftUntil(text, direction, ok))
		.filter(ok)
		.sort(
			(x, y) => Math.abs(toOklch(x).l - toOklch(text).l) - Math.abs(toOklch(y).l - toOklch(text).l)
		);
	return shifted[0] ?? bestContrast(fill, ['#000000', '#FFFFFF']);
}

/** `labelOn` that aims for 7:1 where the family requires AAA text and the fill allows it. */
function label(fill: string, preferred: string[], text: string, strict: boolean): string {
	if (strict) {
		const aaa = labelOn(fill, preferred, text, 7);
		if (contrast(aaa, fill) >= 7) return aaa;
	}
	return labelOn(fill, preferred, text, 4.5);
}

/**
 * Labels on fills. Primary and status fills take whichever ground contrasts more; the neutral
 * fill takes the text or surface colour. Either is replaced by a lightness-shifted text colour
 * when it falls short, so every label reaches 4.5:1 (7:1 for High Contrast where possible).
 * The accent label is the one exception: design-tokens.md allows large/bold text there, so it
 * only warns.
 */
function fillContents(
	t: Record<string, string>,
	text: string,
	bg: string,
	surface: string,
	strict: boolean
): Record<string, string> {
	const on = (fill: string) => label(fill, [surface, bg], text, strict);
	return {
		'--color-primary-content': on(t['--color-primary']!),
		'--color-secondary-content': on(t['--color-primary']!),
		'--color-info-content': on(t['--color-info']!),
		'--color-success-content': on(t['--color-success']!),
		'--color-warning-content': on(t['--color-warning']!),
		'--color-error-content': on(t['--color-error']!),
		'--color-accent-content': bestContrast(t['--color-accent']!, [text, bg]),
		'--color-neutral-content': label(t['--color-neutral']!, [text, surface], text, strict)
	};
}

/**
 * A hover/selected tint: the mix of surface and primary, moved in lightness toward the
 * background until `text` on it reaches 4.5:1. The mix is kept when it already passes.
 */
function tint(mixed: string, bg: string, text: string): string {
	const away = toOklch(bg).l < toOklch(mixed).l ? 'darker' : 'lighter';
	return shiftUntil(mixed, away, (c) => contrast(text, c) >= 4.5);
}

/** Derive every role token for a theme. Values are upper-case #RRGGBB. */
export function derive(theme: Theme): Tokens {
	const { background: bg, surface, text, primary, accent, categorical: cat } = theme.colors;
	const r = RULES[theme.mode];
	const grounds = [bg, surface];
	// Light themes darken a colour until it passes; dark themes lighten it. A colour that
	// already passes is returned untouched.
	const direction = theme.mode === 'light' ? 'darker' : 'lighter';
	const ink = (hex: string, ratio: number, on: string[] = grounds) =>
		shiftUntil(hex, direction, (c) => on.every((g) => contrast(c, g) >= ratio));

	const primaryInk = ink(primary, 4.5);
	const accentInk = ink(accent, 4.5);
	const warning = ink(cat[2]!, 4.5);

	const tc = toOklch(text);
	const bgL = toOklch(bg).l;
	const mutedBase = mix(text, primary, r.mutedHue);
	const mutedText = ink(
		fromOklch({ ...toOklch(mutedBase), l: tc.l + r.mutedShift * (bgL - tc.l) }),
		4.5
	);

	// Data marks: the categorical colours shifted until each is >= 3:1 on the surface.
	const data = cat.map((c) => ink(c, 3, [surface]));
	const slate = data[4]!;

	const terra = toOklch(cat[3]!);
	const error = ink(
		fromOklch({ l: terra.l, c: terra.c * r.errorChroma, h: terra.h - ERROR_HUE_SHIFT }),
		r.errorRatio
	);

	const withDivider = r.dividerWith === 'accent' ? accent : primary;
	const tokens: Record<string, string> = {
		'--color-base-100': surface,
		'--color-base-200': bg,
		'--color-base-300': mix(bg, withDivider, r.dividerMix),
		'--color-base-content': text,
		'--color-primary': primaryInk,
		'--color-secondary': primaryInk,
		'--color-accent': accent,
		'--color-neutral': slate,
		'--color-info': primaryInk,
		'--color-success': accentInk,
		'--color-warning': warning,
		'--color-error': error,
		'--color-accent-ink': accentInk,
		'--color-muted-text': mutedText,
		'--color-field-border': slate,
		'--color-hover': tint(mix(surface, primary, r.hoverMix), bg, text),
		'--color-selected': tint(mix(surface, primary, r.selectedMix), bg, text),
		'--data-other': mutedText,
		'--brand-surface': surface,
		'--brand-primary': primary,
		'--brand-accent': accent,
		'--brand-star': BRAND_STAR[theme.mode]
	};
	DATA_SLOT.forEach((slot, i) => (tokens[`--data-${slot}`] = data[i]!));

	for (const [key, value] of Object.entries(PINS[theme.id] ?? {})) tokens[key] = value;
	Object.assign(tokens, fillContents(tokens, text, bg, surface, isHighContrast(theme)));

	const ordered = {} as Record<string, string>;
	for (const key of TOKEN_KEYS) ordered[key] = up(tokens[key]!);
	return ordered as Tokens;
}

// ---- gates ------------------------------------------------------------------------------

export type Check = {
	label: string;
	value: number;
	min: number;
	severity: 'fail' | 'warn';
	pass: boolean;
};

export type CvdRow = { kind: Cvd; machado: number; vienot: number };

export type Report = {
	id: string;
	checks: Check[];
	normalDeltaE: number;
	normalPair: [number, number];
	cvd: CvdRow[];
	contrast: 'AA' | 'AAA';
};

const isHighContrast = (theme: Theme) => /^high[ -]contrast$/i.test(theme.family);

export function evaluate(theme: Theme, t: Tokens): Report {
	const bg = t['--color-base-200'];
	const surface = t['--color-base-100'];
	const checks: Check[] = [];
	const add = (
		label: string,
		fg: string,
		on: string,
		min: number,
		severity: Check['severity'] = 'fail'
	) => {
		const value = contrast(fg, on);
		checks.push({ label, value, min, severity, pass: value >= min });
	};
	// §10.18 grandfathers Riverstone on categorical ΔE only, never on text contrast.
	const grandfathered = theme.family === 'Riverstone';

	const textMin = isHighContrast(theme) ? 7 : 4.5;
	add('text on background', t['--color-base-content'], bg, textMin);
	add('text on surface', t['--color-base-content'], surface, textMin);

	for (const [name, key] of [
		['primary-ink', '--color-primary'],
		['accent-ink', '--color-accent-ink'],
		['muted-text', '--color-muted-text'],
		['warning', '--color-warning'],
		['error', '--color-error']
	] as const) {
		add(`${name} on background`, t[key], bg, 4.5);
		add(`${name} on surface`, t[key], surface, 4.5);
	}

	// The focus ring is the primary ink; the field border must read on the surface it outlines.
	add('focus ring on background', t['--color-primary'], bg, 3);
	add('focus ring on surface', t['--color-primary'], surface, 3);
	add('field-border on surface', t['--color-field-border'], surface, 3);

	for (let slot = 1; slot <= 6; slot++) {
		add(`data-${slot} on surface`, t[`--data-${slot}` as keyof Tokens], surface, 3);
	}

	for (const [name, fill, label] of [
		['primary-content', '--color-primary', '--color-primary-content'],
		['info-content', '--color-info', '--color-info-content'],
		['success-content', '--color-success', '--color-success-content'],
		['warning-content', '--color-warning', '--color-warning-content'],
		['error-content', '--color-error', '--color-error-content']
	] as const) {
		add(`${name} on its fill`, t[label], t[fill], 4.5);
	}
	// Accent fills carry short labels; design-tokens.md allows large/bold there.
	add('accent-content on accent', t['--color-accent-content'], t['--color-accent'], 4.5, 'warn');
	add('neutral-content on neutral', t['--color-neutral-content'], t['--color-neutral'], 4.5);

	add('text on hover tint', t['--color-base-content'], t['--color-hover'], 4.5);
	add('text on selected tint', t['--color-base-content'], t['--color-selected'], 4.5);

	const dataMarks = [1, 2, 3, 4, 5, 6].map((n) => t[`--data-${n}` as keyof Tokens]);
	const normal = minPairwise(dataMarks);
	checks.push({
		label: 'data colours, min pairwise ΔE_ok (normal vision)',
		value: normal.min,
		min: 5,
		severity: 'warn',
		pass: normal.min >= 5
	});
	// The error hue must stay apart from the Software colour it is derived from.
	const apart = deltaE(t['--color-error'], t['--data-3']);
	checks.push({
		label: 'error vs data-3 ΔE_ok',
		value: apart,
		min: 5,
		severity: 'warn',
		pass: apart >= 5
	});

	if (grandfathered) {
		for (const c of checks) {
			if (c.label.includes('min pairwise') && !c.pass) {
				c.pass = true;
				c.label += ' (grandfathered)';
			}
		}
	}

	const cvd: CvdRow[] = (['deuteranopia', 'protanopia'] as Cvd[]).map((kind) => {
		const worst = (model: CvdModel) =>
			minPairwise(dataMarks, (hex) => simulate(hex, kind, model)).min;
		return { kind, machado: worst('machado'), vienot: worst('vienot') };
	});

	const aaa = (
		['--color-base-content', '--color-muted-text', '--color-primary', '--color-accent-ink'] as const
	).every((k) => contrast(t[k], bg) >= 7 && contrast(t[k], surface) >= 7);

	return {
		id: theme.id,
		checks,
		normalDeltaE: normal.min,
		normalPair: normal.pair,
		cvd,
		contrast: aaa ? 'AAA' : 'AA'
	};
}

export const failures = (r: Report) => r.checks.filter((c) => !c.pass && c.severity === 'fail');
export const warnings = (r: Report) => r.checks.filter((c) => !c.pass && c.severity === 'warn');

export function formatReport(r: Report, verbose = false): string {
	const f = failures(r);
	const w = warnings(r);
	const head = `${f.length ? '✗' : '✓'} ${r.id}: ${f.length} failure(s), ${w.length} warning(s), contrast ${r.contrast}`;
	const line = (c: Check, mark: string) =>
		`    ${mark} ${c.label}: ${c.value.toFixed(2)} (needs ≥ ${c.min})`;
	const out = [head];
	for (const c of f) out.push(line(c, 'FAIL'));
	for (const c of w) out.push(line(c, 'warn'));
	if (verbose) for (const c of r.checks.filter((c) => c.pass)) out.push(line(c, ' ok '));
	const cvd = r.cvd
		.map((row) => `${row.kind} Machado ${row.machado.toFixed(1)} / Viénot ${row.vienot.toFixed(1)}`)
		.join('; ');
	out.push(`    min pairwise ΔE_ok, normal ${r.normalDeltaE.toFixed(1)}; ${cvd}`);
	return out.join('\n');
}

// ---- output -----------------------------------------------------------------------------

const HEADER = 'generated by scripts/themes.ts from themes/*.json; do not edit.';

const RIVERSTONE = { light: 'riverstone-light', dark: 'riverstone-dark' } as const;

function cssBlock(selector: string, mode: Mode, tokens: Tokens): string {
	const lines = TOKEN_KEYS.map((k) => `\t${k}: ${tokens[k].toLowerCase()};`);
	return `${selector} {\n\tcolor-scheme: ${mode};\n${lines.join('\n')}\n}\n`;
}

export function criticalCss(light: Tokens, dark: Tokens): string {
	return (
		`/* ${HEADER} */\n` +
		cssBlock(`:root,\n[data-theme='${RIVERSTONE.light}']`, 'light', light) +
		'\n' +
		cssBlock(`[data-theme='${RIVERSTONE.dark}']`, 'dark', dark)
	);
}

export type ThemesFile = {
	v: 1;
	keys: readonly string[];
	themes: {
		id: string;
		name: string;
		family: string;
		group: string;
		mode: Mode;
		contrast: 'AA' | 'AAA';
		license?: string;
		source?: string;
		values: string[];
	}[];
};

export function themesJson(
	compiled: { theme: Theme; tokens: Tokens; report: Report }[]
): ThemesFile {
	return {
		v: 1,
		keys: TOKEN_KEYS,
		themes: compiled.map(({ theme, tokens, report }) => ({
			id: theme.id,
			name: theme.name,
			family: theme.family,
			group: theme.group,
			mode: theme.mode,
			contrast: report.contrast,
			...(theme.license ? { license: theme.license } : {}),
			...(theme.source ? { source: theme.source } : {}),
			values: TOKEN_KEYS.map((k) => tokens[k].toLowerCase())
		}))
	};
}

/**
 * Pre-paint script. `lodestar.theme` in localStorage holds
 * `{ mode: 'system'|'light'|'dark', light: { id, vars? }, dark: { id, vars? } }`; `vars` is the
 * cached custom-property map of a non-Riverstone theme. Anything missing or malformed falls back
 * to Riverstone for the resolved mode, including a non-Riverstone id with no cached `vars`
 * (its `data-theme` would match no CSS).
 */
export const PREPAINT =
	"(function(){var d=document.documentElement,s={},v,i,p;try{s=JSON.parse(localStorage.getItem('lodestar.theme'))||{}}catch(e){}" +
	"var m=s.mode,n=m=='dark'||(m!='light'&&matchMedia('(prefers-color-scheme:dark)').matches)," +
	"t=s[n?'dark':'light']||{},r='riverstone-'+(n?'dark':'light'),ok;i=t.id;v=t.vars;" +
	"ok=typeof i=='string'&&/^[a-z0-9-]+$/.test(i)&&i.indexOf('riverstone-')!=0&&v&&typeof v=='object'&&Object.keys(v).length;" +
	"d.dataset.theme=ok?i:r;d.style.colorScheme=n?'dark':'light';" +
	'if(ok)for(p in v)/^--[a-z0-9-]+$/.test(p)&&d.style.setProperty(p,v[p])})()';

export const prepaintFile = () =>
	`// ${HEADER}\n/* eslint-disable */\n// @ts-nocheck\n// prettier-ignore\n${PREPAINT};\n`;

const SCRIPT_TAG = /(<script id="prepaint">)[^]*?(<\/script>)/;

/** Replace the body of `<script id="prepaint">` in app.html; null when the tag is missing. */
export function injectPrepaint(html: string): string | null {
	return SCRIPT_TAG.test(html)
		? html.replace(SCRIPT_TAG, () => `<script id="prepaint">${PREPAINT}</script>`)
		: null;
}

// ---- driver -----------------------------------------------------------------------------

export type Compiled = {
	theme: Theme;
	tokens: Tokens;
	report: Report;
	errors: string[];
};

/** Load, validate, derive and gate every family file themes/<id>.json in `dir`. */
export function compileDir(dir: string): { compiled: Compiled[]; problems: string[] } {
	const problems: string[] = [];
	const compiled: Compiled[] = [];
	const files = existsSync(dir)
		? readdirSync(dir)
				.filter((f) => f.endsWith('.json'))
				.sort()
		: [];
	const families: { file: string; family: Family }[] = [];
	for (const file of files) {
		let raw: unknown;
		try {
			raw = JSON.parse(readFileSync(join(dir, file), 'utf8'));
		} catch (e) {
			problems.push(`${file}: invalid JSON (${(e as Error).message})`);
			continue;
		}
		const errors = validateFamily(raw);
		if (errors.length) {
			problems.push(...errors.map((e) => `${file}: ${e}`));
			continue;
		}
		families.push({ file, family: raw as Family });
	}
	const result = compileFamilies(families);
	problems.push(...result.problems);
	compiled.push(...result.compiled);
	return { compiled, problems };
}

/** Derive and gate already-validated families; `file` is only used in messages. */
export function compileFamilies(families: { file: string; family: Family }[]): {
	compiled: Compiled[];
	problems: string[];
} {
	const problems: string[] = [];
	const compiled: Compiled[] = [];
	const seen = new Set<string>();
	for (const { file, family } of families) {
		if (seen.has(family.id)) problems.push(`${file}: duplicate id "${family.id}"`);
		seen.add(family.id);
		if (file !== `${family.id}.json`) {
			problems.push(`${file}: file name must be "${family.id}.json"`);
		}
		for (const theme of expandFamily(family)) {
			const tokens = derive(theme);
			compiled.push({ theme, tokens, report: evaluate(theme, tokens), errors: [] });
		}
	}
	if (!seen.has('riverstone')) problems.push('missing required family "riverstone"');
	return { compiled, problems };
}

async function prettify(text: string, filepath: string): Promise<string> {
	const prettier = await import('prettier');
	const config = (await prettier.resolveConfig(filepath)) ?? {};
	return prettier.format(text, { ...config, filepath });
}

/** The generated files, keyed by path relative to web/. */
export async function renderOutputs(compiled: Compiled[]): Promise<Record<string, string>> {
	const byId = new Map(compiled.map((c) => [c.theme.id, c]));
	const light = byId.get(RIVERSTONE.light)!.tokens;
	const dark = byId.get(RIVERSTONE.dark)!.tokens;
	const rest = compiled.filter((c) => !Object.values(RIVERSTONE).includes(c.theme.id as never));
	return {
		'src/lib/theme/critical.css': await prettify(criticalCss(light, dark), 'critical.css'),
		'src/lib/theme/prepaint.js': prepaintFile(),
		'static/themes.json': await prettify(JSON.stringify(themesJson(rest)), 'themes.json')
	};
}

async function main() {
	const args = process.argv.slice(2);
	const root = fileURLToPath(new URL('..', import.meta.url));
	const dirArg = args.indexOf('--dir');
	const dir = dirArg >= 0 ? args[dirArg + 1]! : join(root, 'themes');
	const write = !args.includes('--no-write');
	const verbose = args.includes('--verbose');

	const { compiled, problems } = compileDir(dir);
	let bad = problems.length;
	for (const p of problems) console.error(`✗ ${p}`);
	for (const { report } of compiled) {
		const text = formatReport(report, verbose);
		(failures(report).length ? console.error : console.log)(text);
		bad += failures(report).length;
	}
	if (bad) {
		console.error(`\nthemes: ${bad} problem(s); nothing written.`);
		process.exit(1);
	}
	console.log(`themes: ${compiled.length} theme(s) pass.`);
	if (!write) return;

	const htmlPath = join(root, 'src/app.html');
	const html = readFileSync(htmlPath, 'utf8');
	const next = injectPrepaint(html);
	if (next === null) {
		console.error('themes: src/app.html has no <script id="prepaint"> tag.');
		process.exit(1);
	}
	for (const [rel, text] of Object.entries(await renderOutputs(compiled))) {
		const path = join(root, rel);
		mkdirSync(dirname(path), { recursive: true });
		writeFileSync(path, text);
	}
	if (next !== html) writeFileSync(htmlPath, next);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) await main();
