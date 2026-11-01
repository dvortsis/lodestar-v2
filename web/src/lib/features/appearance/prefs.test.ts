import { beforeEach, describe, expect, it, vi } from 'vitest';

const patch = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (orig) => ({
	...(await orig<typeof import('$lib/api/client')>()),
	api: { patch }
}));

import { CACHE, Unreadable, applyLook, familyFor, save } from './prefs';

const root = document.documentElement;
beforeEach(() => {
	root.removeAttribute('style');
	localStorage.clear();
});

describe('applyLook', () => {
	it('sets the attributes and caches them', () => {
		applyLook({ density: 'compact', motion: 'system', illustrations: false });
		expect(root.dataset.density).toBe('compact');
		expect(root.dataset.illustrations).toBe('false');
		expect(JSON.parse(localStorage.getItem(CACHE)!).density).toBe('compact');
	});
	it('forces durations to 0ms for reduced and to the full values for full', () => {
		applyLook({ density: 'comfortable', motion: 'reduced', illustrations: true });
		expect(root.style.getPropertyValue('--duration-drawer-in')).toBe('0ms');
		applyLook({ density: 'comfortable', motion: 'full', illustrations: true });
		expect(root.style.getPropertyValue('--duration-drawer-in')).toBe('300ms');
	});
	it('lets the system decide by removing the properties', () => {
		applyLook({ density: 'comfortable', motion: 'reduced', illustrations: true });
		applyLook({ density: 'comfortable', motion: 'system', illustrations: true });
		expect(root.style.getPropertyValue('--duration-fast')).toBe('');
	});
});

describe('familyFor', () => {
	it('uses theme_dark only in dark mode and when set', () => {
		expect(familyFor({ theme: 'a', theme_dark: 'b' }, true)).toBe('b');
		expect(familyFor({ theme: 'a', theme_dark: 'b' }, false)).toBe('a');
		expect(familyFor({ theme: 'a', theme_dark: '' }, true)).toBe('a');
	});
});

describe('save', () => {
	it('resolves to the new revision', async () => {
		patch.mockResolvedValueOnce({ revision: 9 });
		await expect(save({ density: 'compact' }, 8)).resolves.toBe(9);
	});
	it.each([{}, null, { revision: '9' }])('throws Unreadable for a 2xx of %j', async (res) => {
		patch.mockResolvedValueOnce(res);
		await expect(save({ density: 'compact' }, 8)).rejects.toBeInstanceOf(Unreadable);
	});
});
