// Themes at run time (§10.18): `themes.json` is fetched the first time Appearance needs it, and a
// theme is applied as custom properties on <html>. The choice is cached in `lodestar.theme`, which
// the pre-paint script (src/lib/theme/prepaint.js) reads before first paint.
import { prefersDark, familyFor, type Appearance } from './prefs';

export interface ThemeEntry {
	id: string;
	name: string;
	family: string;
	group: string;
	mode: 'light' | 'dark';
	contrast: 'AA' | 'AAA';
	license?: string;
	source?: string;
	values: string[];
	/** A custom theme (per browser). */
	custom?: boolean;
	/** A custom theme saved with failing contrast ("Use anyway"). */
	warn?: boolean;
	/** A custom theme's base palette, so it can be edited and exported. */
	palette?: unknown;
	/** The token names `values` line up with when it was saved. */
	keys?: string[];
}
export interface ThemesFile {
	v: 1;
	keys: string[];
	themes: ThemeEntry[];
}

export const THEME_CACHE = 'lodestar.theme';
export const CUSTOM_KEY = 'lodestar.customThemes';
const MODES = ['light', 'dark'] as const;

let file: Promise<ThemesFile> | undefined;
/** Fetches `themes.json` once; a failure is not remembered, so the next call retries. */
export function loadThemes(): Promise<ThemesFile> {
	file ??= fetch('/themes.json')
		.then((r) => {
			if (!r.ok) throw new Error(String(r.status));
			return r.json() as Promise<ThemesFile>;
		})
		.catch((e) => {
			file = undefined;
			throw e;
		});
	return file;
}

const CUSTOM_ID = /^custom-[a-z0-9]+(-[a-z0-9]+)*-(light|dark)$/;
const HEX = /^#[0-9a-f]{6}$/i;

/** Custom themes from storage; anything malformed is dropped (storage is untrusted input). */
export function readCustom(): ThemeEntry[] {
	try {
		const raw: unknown = JSON.parse(localStorage.getItem(CUSTOM_KEY) ?? '[]');
		if (!Array.isArray(raw)) return [];
		return raw.filter(
			(t): t is ThemeEntry =>
				!!t &&
				t.custom === true &&
				typeof t.id === 'string' &&
				CUSTOM_ID.test(t.id) &&
				typeof t.name === 'string' &&
				typeof t.family === 'string' &&
				(t.mode === 'light' || t.mode === 'dark') &&
				Array.isArray(t.values) &&
				t.values.every((v: unknown) => typeof v === 'string' && HEX.test(v)) &&
				Array.isArray(t.keys) &&
				t.keys.every((k: unknown) => typeof k === 'string')
		);
	} catch {
		return [];
	}
}

/** Whether a theme's values line up with this build's token names (a custom theme saved by an older build may not). */
export const usable = (t: ThemeEntry, keys: string[]) =>
	!t.custom || (t.keys?.length === keys.length && t.keys.every((k, i) => k === keys[i]));

export function writeCustom(list: ThemeEntry[]) {
	try {
		localStorage.setItem(CUSTOM_KEY, JSON.stringify(list));
	} catch {
		/* storage blocked */
	}
}

const vars = (keys: string[], t: ThemeEntry) =>
	Object.fromEntries(keys.map((k, i) => [k, t.values[i]!]));

/**
 * Applies the family the preferences choose for the current mode, and caches both modes for the
 * pre-paint script. Riverstone needs no file (it is in the critical CSS).
 */
/** Bumped by every call: a call that finds a newer one after its fetch leaves the page alone. */
let generation = 0;

export async function applyTheme(p: Appearance): Promise<{ missing: string[] }> {
	const mine = ++generation;
	const root = document.documentElement;
	const dark = prefersDark(p.mode);
	const cache: Record<string, unknown> = { mode: p.mode };
	const missing: string[] = [];
	let all: ThemesFile | undefined;
	const needs = [familyFor(p, false), familyFor(p, true)].some((f) => f !== 'riverstone');
	if (needs) all = await loadThemes();
	if (mine !== generation) return { missing: [] };
	const pool = [...(all?.themes ?? []), ...readCustom()].filter((x) => usable(x, all?.keys ?? []));
	const keys = all?.keys ?? [];
	for (const mode of MODES) {
		const id = `${familyFor(p, mode === 'dark')}-${mode}`;
		const t = pool.find((x) => x.id === id);
		if (!t && !id.startsWith('riverstone-')) missing.push(id);
		cache[mode] = t && !id.startsWith('riverstone-') ? { id, vars: vars(keys, t) } : { id };
	}
	const mine = cache[dark ? 'dark' : 'light'] as { id: string; vars?: Record<string, string> };
	const prev = root.getAttribute('style') ?? '';
	for (const k of prev.match(/--(?!duration-)[a-z0-9-]+(?=:)/g) ?? []) root.style.removeProperty(k);
	const ok = mine.vars !== undefined;
	root.dataset.theme = ok ? mine.id : `riverstone-${dark ? 'dark' : 'light'}`;
	root.style.colorScheme = dark ? 'dark' : 'light';
	if (ok) for (const [k, v] of Object.entries(mine.vars!)) root.style.setProperty(k, v);
	try {
		localStorage.setItem(THEME_CACHE, JSON.stringify(cache));
	} catch {
		/* storage blocked */
	}
	return { missing };
}
