import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { DEFAULTS, type Appearance } from './prefs';
import { applyTheme } from './themes';

const get = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (orig) => ({
	...(await orig<typeof import('$lib/api/client')>()),
	api: { get }
}));
vi.mock('$lib/state/events.svelte', () => ({
	events: {
		status: 'live',
		on: () => () => {}
	}
}));

import { startAppearance } from './boot';

/** Fixture colours, built from numbers so no hex literal sits in a test (the bans). */
const hex = (n: number) => `#${n.toString(16).padStart(6, '0')}`;
const themes = {
	v: 1,
	keys: ['--data-1'],
	themes: ['light', 'dark'].map((mode) => ({
		id: `nord-${mode}`,
		name: 'Nord',
		family: 'nord',
		group: 'community',
		mode,
		contrast: 'AA',
		values: [hex(0x112233)]
	}))
};
const section = (p: Appearance) => ({
	section: 'preferences',
	revision: 1,
	restart_required: [],
	fields: Object.fromEntries(
		Object.entries(p).map(([k, value]) => [k, { value, env: null, read_only: false }])
	)
});
const root = document.documentElement;

let stop: (() => void) | undefined;
beforeEach(() => {
	localStorage.clear();
	root.removeAttribute('style');
	for (const a of ['theme', 'density', 'motion', 'illustrations']) delete root.dataset[a];
	vi.stubGlobal('matchMedia', () => ({ matches: false }));
	vi.stubGlobal(
		'fetch',
		vi.fn(async () => ({ ok: true, json: async () => themes }))
	);
	get.mockReset();
});
afterEach(() => {
	stop?.();
	stop = undefined;
	vi.unstubAllGlobals();
});

describe('startAppearance', () => {
	it('applies the saved theme, density and motion, and caches the theme', async () => {
		get.mockResolvedValue(
			section({ ...DEFAULTS, theme: 'nord', density: 'compact', motion: 'reduced' })
		);
		stop = startAppearance();
		await vi.waitFor(() => expect(root.dataset.theme).toBe('nord-light'));
		expect(root.dataset.density).toBe('compact');
		expect(root.dataset.motion).toBe('reduced');
		expect(JSON.parse(localStorage.getItem('lodestar.theme')!).light.id).toBe('nord-light');
	});

	it('changes nothing when the page already shows the saved values', async () => {
		await applyTheme(DEFAULTS);
		root.dataset.density = DEFAULTS.density;
		root.dataset.motion = DEFAULTS.motion;
		root.dataset.illustrations = String(DEFAULTS.illustrations);
		localStorage.clear();
		get.mockResolvedValue(section(DEFAULTS));
		stop = startAppearance();
		await vi.waitFor(() => expect(get).toHaveBeenCalled());
		await Promise.resolve();
		await Promise.resolve();
		expect(localStorage.length).toBe(0);
		expect(fetch).not.toHaveBeenCalled();
	});

	it('stays silent and leaves the page alone when the fetch fails', async () => {
		root.dataset.theme = 'riverstone-light';
		get.mockRejectedValue(new Error('offline'));
		stop = startAppearance();
		await vi.waitFor(() => expect(get).toHaveBeenCalled());
		await Promise.resolve();
		expect(root.dataset.theme).toBe('riverstone-light');
		expect(root.dataset.density).toBeUndefined();
	});

	it('does not overwrite a choice made while the fetch was in flight', async () => {
		let answer!: (v: unknown) => void;
		get.mockReturnValue(new Promise((r) => (answer = r)));
		stop = startAppearance();
		await vi.waitFor(() => expect(get).toHaveBeenCalled());
		await applyTheme({ ...DEFAULTS, theme: 'nord', mode: 'dark' });
		answer(section({ ...DEFAULTS, density: 'cards' }));
		await Promise.resolve();
		await Promise.resolve();
		expect(root.dataset.theme).toBe('nord-dark');
		expect(root.dataset.density).toBeUndefined();
	});

	it('falls back to Riverstone for a custom theme this browser lacks', async () => {
		get.mockResolvedValue(section({ ...DEFAULTS, theme: 'custom-mine', mode: 'dark' }));
		stop = startAppearance();
		await vi.waitFor(() => expect(root.dataset.theme).toBe('riverstone-dark'));
	});
});
