import { beforeEach, describe, expect, it, vi } from 'vitest';
import { DEFAULTS } from './prefs';
import { CUSTOM_KEY, THEME_CACHE, applyTheme, readCustom, usable } from './themes';
import type { ThemeEntry, ThemesFile } from './themes';

/** Fixture colours, built from numbers so no hex literal sits in a test (the bans). */
const hex = (n: number) => `#${n.toString(16).padStart(6, '0')}`;
const KEYS = ['--color-base-100', '--data-1'];
const entry = (
	id: string,
	mode: 'light' | 'dark',
	extra: Partial<ThemeEntry> = {}
): ThemeEntry => ({
	id,
	name: id,
	family: id,
	group: 'community',
	mode,
	contrast: 'AA',
	values: [hex(0x111111), hex(0x222222)],
	...extra
});
const file: ThemesFile = {
	v: 1,
	keys: KEYS,
	themes: [entry('nord-light', 'light'), entry('nord-dark', 'dark')]
};
const cached = () => JSON.parse(localStorage.getItem(THEME_CACHE)!);

beforeEach(() => {
	localStorage.clear();
	document.documentElement.removeAttribute('style');
	document.documentElement.removeAttribute('data-theme');
	vi.stubGlobal('matchMedia', () => ({
		matches: false,
		addEventListener() {},
		removeEventListener() {}
	}));
	vi.stubGlobal(
		'fetch',
		vi.fn(async () => ({ ok: true, json: async () => file }))
	);
});

describe('applyTheme', () => {
	it('writes the pre-paint cache and applies a family', async () => {
		const res = await applyTheme({ ...DEFAULTS, theme: 'nord', mode: 'dark' });
		expect(res.missing).toEqual([]);
		expect(document.documentElement.dataset.theme).toBe('nord-dark');
		expect(document.documentElement.style.getPropertyValue('--data-1')).toBe(hex(0x222222));
		expect(cached()).toEqual({
			mode: 'dark',
			light: {
				id: 'nord-light',
				vars: { '--color-base-100': hex(0x111111), '--data-1': hex(0x222222) }
			},
			dark: {
				id: 'nord-dark',
				vars: { '--color-base-100': hex(0x111111), '--data-1': hex(0x222222) }
			}
		});
	});

	it('clears a theme’s properties when switching back to Riverstone, and fetches nothing', async () => {
		await applyTheme({ ...DEFAULTS, theme: 'nord' });
		vi.mocked(fetch).mockClear();
		await applyTheme({ ...DEFAULTS, theme: 'riverstone' });
		expect(document.documentElement.dataset.theme).toBe('riverstone-light');
		expect(document.documentElement.style.getPropertyValue('--data-1')).toBe('');
		expect(fetch).not.toHaveBeenCalled();
		expect(cached().light).toEqual({ id: 'riverstone-light' });
	});

	it('uses theme_dark in dark mode and keeps duration properties', async () => {
		document.documentElement.style.setProperty('--duration-fast', '0ms');
		await applyTheme({ ...DEFAULTS, theme: 'riverstone', theme_dark: 'nord', mode: 'dark' });
		expect(document.documentElement.dataset.theme).toBe('nord-dark');
		expect(cached().light).toEqual({ id: 'riverstone-light' });
		expect(document.documentElement.style.getPropertyValue('--duration-fast')).toBe('0ms');
	});

	it('falls back to Riverstone and names a custom theme this browser lacks', async () => {
		const res = await applyTheme({ ...DEFAULTS, theme: 'custom-harbour' });
		expect(res.missing).toEqual(['custom-harbour-light', 'custom-harbour-dark']);
		expect(document.documentElement.dataset.theme).toBe('riverstone-light');
	});
});

describe('loadThemes', () => {
	it('retries after a failure', async () => {
		vi.resetModules();
		vi.mocked(fetch).mockRejectedValueOnce(new Error('offline'));
		const fresh = await import('./themes');
		await expect(fresh.loadThemes()).rejects.toThrow();
		await expect(fresh.loadThemes()).resolves.toEqual(file);
	});
	it('is shared once loaded', async () => {
		vi.resetModules();
		const fresh = await import('./themes');
		await fresh.loadThemes();
		await fresh.loadThemes();
		expect(fetch).toHaveBeenCalledTimes(1);
	});
});

describe('readCustom', () => {
	const good = entry('custom-harbour-light', 'light', { custom: true, keys: KEYS });
	const put = (v: unknown) =>
		localStorage.setItem(CUSTOM_KEY, typeof v === 'string' ? v : JSON.stringify(v));

	it('reads valid entries', () => {
		put([good]);
		expect(readCustom()).toHaveLength(1);
	});
	it('survives corrupt storage', () => {
		put('{nope');
		expect(readCustom()).toEqual([]);
		put({ a: 1 });
		expect(readCustom()).toEqual([]);
	});
	it('drops entries that could inject CSS or throw', () => {
		put([
			{ ...good, values: ['red;background:url(//x)', hex(0x222222)] },
			{ ...good, values: 'nope' },
			{ ...good, id: 'Custom Bad' },
			{ ...good, keys: undefined },
			null,
			good
		]);
		expect(readCustom()).toEqual([good]);
	});
	it('ignores a theme saved against other token names', () => {
		expect(usable(good, KEYS)).toBe(true);
		expect(usable(good, [...KEYS, '--new'])).toBe(false);
		expect(usable(entry('nord-light', 'light'), KEYS)).toBe(true);
	});
});
