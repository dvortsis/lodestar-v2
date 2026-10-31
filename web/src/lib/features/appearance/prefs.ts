// The appearance preferences (§10.10.1): read, saved per account and applied at once. Preferences
// have no staged Save; every change goes straight to the server with `if_revision`, and the screen
// offers Undo. Applying also writes the per-browser cache the pre-paint script reads.
import { api } from '$lib/api/client';

export const MODES = ['system', 'light', 'dark'] as const;
export const DENSITIES = ['compact', 'comfortable', 'cards'] as const;
export const MOTIONS = ['system', 'reduced', 'full'] as const;

export interface Appearance {
	theme: string;
	theme_dark: string;
	mode: (typeof MODES)[number];
	density: (typeof DENSITIES)[number];
	motion: (typeof MOTIONS)[number];
	illustrations: boolean;
}
export type Name = keyof Appearance;
export const NAMES: Name[] = ['theme', 'theme_dark', 'mode', 'density', 'motion', 'illustrations'];
export const SECTION = { path: { section: 'preferences' } };
export const CONTENT = { path: { section: 'content' } };

export const DEFAULTS: Appearance = {
	theme: 'riverstone',
	theme_dark: '',
	mode: 'system',
	density: 'comfortable',
	motion: 'system',
	illustrations: true
};

const THEME_ID = /^[a-z0-9]+(-[a-z0-9]+)*$/;
const oneOf = <T extends string>(list: readonly T[], v: unknown): v is T =>
	typeof v === 'string' && (list as readonly string[]).includes(v);

/** Reads the preferences out of untrusted JSON; undefined when any of them is malformed. */
export function narrow(f: Record<string, { value?: unknown } | undefined>): Appearance | undefined {
	const v = (n: Name) => f[n]?.value;
	const ok =
		typeof v('theme') === 'string' &&
		THEME_ID.test(v('theme') as string) &&
		typeof v('theme_dark') === 'string' &&
		(v('theme_dark') === '' || THEME_ID.test(v('theme_dark') as string)) &&
		oneOf(MODES, v('mode')) &&
		oneOf(DENSITIES, v('density')) &&
		oneOf(MOTIONS, v('motion')) &&
		typeof v('illustrations') === 'boolean';
	return ok
		? (Object.fromEntries(NAMES.map((n) => [n, v(n)])) as unknown as Appearance)
		: undefined;
}

export interface Loaded {
	prefs: Appearance;
	revision: number;
	locks: Partial<Record<Name, string>>;
}

export async function load(signal?: AbortSignal): Promise<Loaded | undefined> {
	const res = (await api.get('/settings/{section}', { ...SECTION, signal })) as unknown as {
		fields: Record<string, { value?: unknown; env?: string | null; read_only?: boolean }>;
		revision: number;
	};
	const prefs = narrow(res.fields);
	if (!prefs) return undefined;
	const locks: Loaded['locks'] = {};
	for (const n of NAMES) {
		const f = res.fields[n];
		if (f?.env) locks[n] = f.env;
		else if (f?.read_only) locks[n] = '';
	}
	return { prefs, revision: res.revision, locks };
}

/** Saves some fields; resolves to the new revision. Errors are the api client's. */
export async function save(patch: Partial<Appearance>, revision: number): Promise<number> {
	const res = await api.patch('/settings/{section}', {
		...SECTION,
		body: { ...patch, if_revision: revision } as unknown as Record<string, never>
	});
	return (res as unknown as { revision: number }).revision;
}

// ---- applying -----------------------------------------------------------------------------

const DURATIONS = ['fast', 'popover-out', 'drawer-in', 'drawer-out', 'banner', 'stagger'] as const;
const FULL: Record<(typeof DURATIONS)[number], string> = {
	fast: '150ms',
	'popover-out': '100ms',
	'drawer-in': '300ms',
	'drawer-out': '200ms',
	banner: '200ms',
	stagger: '80ms'
};
export const CACHE = 'lodestar.appearance';

/** Density and motion: attributes other views read, and the duration tokens a forced motion sets. */
export function applyLook(p: Pick<Appearance, 'density' | 'motion' | 'illustrations'>) {
	const root = document.documentElement;
	root.dataset.density = p.density;
	root.dataset.motion = p.motion;
	root.dataset.illustrations = String(p.illustrations);
	for (const d of DURATIONS) {
		if (p.motion === 'system') root.style.removeProperty(`--duration-${d}`);
		else root.style.setProperty(`--duration-${d}`, p.motion === 'reduced' ? '0ms' : FULL[d]);
	}
	try {
		localStorage.setItem(CACHE, JSON.stringify(p));
	} catch {
		/* storage blocked: the server stays the source of truth */
	}
}

/** The family a mode shows: `theme_dark` in dark mode when set. */
export const familyFor = (p: Pick<Appearance, 'theme' | 'theme_dark'>, dark: boolean) =>
	dark && p.theme_dark ? p.theme_dark : p.theme;

export const prefersDark = (mode: Appearance['mode']) =>
	mode === 'dark' || (mode === 'system' && matchMedia('(prefers-color-scheme: dark)').matches);
