// Theme data for the harness, read from the build output of scripts/themes.ts
// (static/themes.json: every theme but Riverstone; critical.css: Riverstone itself).
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const at = (p: string) => fileURLToPath(new URL(`../../${p}`, import.meta.url));

export type Mode = 'light' | 'dark';

export interface ThemeEntry {
	/** `nord-dark`, `riverstone-light`, … (the `data-theme` value). */
	id: string;
	mode: Mode;
	/** `null` for Riverstone: the app ships it in critical.css, so storage carries only the mode. */
	vars: Record<string, string> | null;
}

interface ThemesFile {
	keys: string[];
	themes: { id: string; mode: Mode; values: string[] }[];
}

const file: ThemesFile = JSON.parse(readFileSync(at('static/themes.json'), 'utf8'));

/** Every theme except Riverstone, in file order (light then dark per family). */
export const OTHER_THEMES: ThemeEntry[] = file.themes.map((t) => ({
	id: t.id,
	mode: t.mode,
	vars: Object.fromEntries(file.keys.map((k, i) => [k, t.values[i]]))
}));

export const RIVERSTONE: ThemeEntry[] = (['light', 'dark'] as const).map((mode) => ({
	id: `riverstone-${mode}`,
	mode,
	vars: null
}));

export const ALL_THEMES: ThemeEntry[] = [...RIVERSTONE, ...OTHER_THEMES];

/** The CSS custom property declared for `selector` in critical.css (Riverstone's tokens). */
export function riverstoneToken(mode: Mode, name: string): string {
	const css = readFileSync(at('src/lib/theme/critical.css'), 'utf8');
	const selector =
		mode === 'light' ? ":root,\n[data-theme='riverstone-light']" : `[data-theme='riverstone-dark']`;
	const block = css.slice(css.indexOf(selector));
	const match = new RegExp(`${name}:\\s*([^;]+);`).exec(block);
	if (!match) throw new Error(`critical.css: ${name} not found for riverstone-${mode}`);
	return match[1].trim();
}

/** `#rrggbb` → `rgb(r, g, b)`, the form getComputedStyle reports. */
export function hexToRgb(hex: string): string {
	const m = /^#([0-9a-f]{6})$/i.exec(hex.trim());
	if (!m) throw new Error(`not a #rrggbb colour: ${hex}`);
	const n = parseInt(m[1], 16);
	return `rgb(${n >> 16}, ${(n >> 8) & 255}, ${n & 255})`;
}

/**
 * Deterministic rotating sample of `count` non-Riverstone themes for CI run `seed`.
 * The pool is first permuted with a stride coprime to its size, so neighbours in a window come from
 * different families and modes; run N then takes the window starting at N × count. Consecutive
 * runs get disjoint windows and every theme is covered within ceil(pool / count) = 8 runs.
 * `seed` is `GITHUB_RUN_NUMBER`, or the day number locally.
 */
export function sampleThemes(
	seed: number,
	count = 6,
	pool: ThemeEntry[] = OTHER_THEMES
): ThemeEntry[] {
	const n = pool.length;
	const stride = [7, 5, 3, 1].find((s) => gcd(s, n) === 1) ?? 1;
	const start = (((seed * count) % n) + n) % n;
	return Array.from(
		{ length: Math.min(count, n) },
		(_, k) => pool[(((start + k) % n) * stride) % n]
	);
}

const gcd = (a: number, b: number): number => (b === 0 ? a : gcd(b, a % b));

export function rotationSeed(env: NodeJS.ProcessEnv = process.env): number {
	const run = Number(env.GITHUB_RUN_NUMBER);
	return Number.isInteger(run) && run > 0 ? run : Math.floor(Date.now() / 86_400_000);
}
