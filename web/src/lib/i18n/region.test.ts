import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { formatDateTime } from '$lib/format';
import { currentLocale, applyLocale } from './locale.svelte';
import {
	REGION_CACHE,
	applyRegion,
	followSaved,
	displayTimeZone,
	narrowRegion,
	readRegionHint
} from './region.svelte';
import { firstDayOfWeek } from './week';

const get = vi.fn();
vi.stubGlobal('fetch', (...a: unknown[]) => get(...a));
const ok = (body: unknown) => ({ ok: true, json: async () => body });

const auto = { language: '', timeZone: '', firstDay: 'auto' } as const;
// 2026-10-22 23:30 UTC: already the 23rd in Tokyo.
const AT = Date.UTC(2026, 9, 22, 23, 30) / 1000;

beforeEach(() => {
	localStorage.clear();
	get.mockReset();
});
afterEach(() => {
	applyRegion(auto);
	applyLocale('en');
});

describe('time zone', () => {
	it('formatters use the saved zone, and the browser zone when automatic', () => {
		applyRegion({ ...auto, timeZone: 'Asia/Tokyo' });
		expect(formatDateTime(AT, 'en')).toContain('Oct 23, 2026');
		applyRegion({ ...auto, timeZone: 'America/Los_Angeles' });
		expect(formatDateTime(AT, 'en')).toContain('Oct 22, 2026, 16:30');
		applyRegion(auto);
		expect(displayTimeZone()).toBe(Intl.DateTimeFormat().resolvedOptions().timeZone);
	});
	it('an explicit zone beats the setting, and an unknown name falls back', () => {
		applyRegion({ ...auto, timeZone: 'Asia/Tokyo' });
		expect(formatDateTime(AT, 'en', { timeZone: 'UTC' })).toContain('Oct 22, 2026, 23:30');
		applyRegion({ ...auto, timeZone: 'Foo/Bar' });
		expect(displayTimeZone()).toBe(Intl.DateTimeFormat().resolvedOptions().timeZone);
	});
});

describe('first day of week', () => {
	it('follows the setting', () => {
		applyRegion({ ...auto, firstDay: 'sunday' });
		expect(firstDayOfWeek()).toBe(0);
		applyRegion({ ...auto, firstDay: 'monday' });
		expect(firstDayOfWeek()).toBe(1);
		applyRegion({ ...auto, firstDay: 'saturday' });
		expect(firstDayOfWeek()).toBe(6);
	});
	it('automatic follows the language', () => {
		expect(firstDayOfWeek('en-US')).toBe(0);
		expect(firstDayOfWeek('de-DE')).toBe(1);
	});
});

describe('narrowRegion', () => {
	it('treats malformed values as automatic', () => {
		expect(narrowRegion({ language: 3, time_zone: null, first_day_of_week: 'x' })).toEqual(auto);
		expect(narrowRegion(null)).toEqual(auto);
	});
});

describe('boot', () => {
	it('applies the hint, then the saved values', async () => {
		localStorage.setItem(REGION_CACHE, JSON.stringify({ ...auto, language: 'zh' }));
		get.mockResolvedValue(
			ok({
				fields: {
					language: { value: 'en' },
					time_zone: { value: 'Asia/Tokyo' },
					first_day_of_week: { value: 'sunday' }
				}
			})
		);
		const { bootRegion } = await import('./boot');
		const done = bootRegion();
		expect(currentLocale()).toBe('zh');
		await done;
		expect(currentLocale()).toBe('en');
		expect(displayTimeZone()).toBe('Asia/Tokyo');
		expect(readRegionHint().timeZone).toBe('Asia/Tokyo');
	});
	it('a failed fetch keeps the hint and does not throw', async () => {
		localStorage.setItem(REGION_CACHE, JSON.stringify({ ...auto, language: 'zh' }));
		get.mockRejectedValue(new Error('offline'));
		const { bootRegion } = await import('./boot');
		await expect(bootRegion()).resolves.toBeUndefined();
		expect(currentLocale()).toBe('zh');
	});
	it('no hint and no answer falls back to the browser, then en', async () => {
		get.mockResolvedValue(ok({}));
		const { bootRegion } = await import('./boot');
		await bootRegion();
		expect(currentLocale()).toBe('en');
	});
	it('drops an answer older than a value applied meanwhile', async () => {
		let resolve: (v: unknown) => void = () => {};
		get.mockReturnValue(new Promise((r) => (resolve = r)));
		const { bootRegion } = await import('./boot');
		const done = bootRegion();
		followSaved({ ...auto, language: 'zh', timeZone: 'Asia/Tokyo' });
		resolve(ok({ fields: { language: { value: 'en' }, time_zone: { value: '' } } }));
		await done;
		expect(currentLocale()).toBe('zh');
		expect(displayTimeZone()).toBe('Asia/Tokyo');
	});
	it('a 401, a 500 or a malformed body keeps the hint', async () => {
		localStorage.setItem(REGION_CACHE, JSON.stringify({ ...auto, language: 'zh' }));
		const { bootRegion } = await import('./boot');
		for (const answer of [
			{ ok: false, status: 401, json: async () => ({}) },
			{ ok: false, status: 500, json: async () => ({}) },
			{
				ok: true,
				status: 200,
				json: async () => {
					throw new Error('bad json');
				}
			}
		]) {
			get.mockResolvedValue(answer);
			await expect(bootRegion()).resolves.toBeUndefined();
			expect(currentLocale()).toBe('zh');
		}
	});
});
