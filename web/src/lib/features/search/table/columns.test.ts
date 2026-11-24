import { describe, expect, it } from 'vitest';
import {
	STORAGE_KEY,
	ariaSort,
	defaultPrefs,
	moveColumn,
	nextSort,
	normalize,
	rawName,
	readPrefs,
	toggleColumn,
	visible,
	writePrefs
} from './columns';

const store = (value: string | null) => ({ getItem: () => value });

describe('column prefs', () => {
	it('start from the defaults', () => {
		expect(readPrefs(store(null))).toEqual(defaultPrefs());
		expect(visible(defaultPrefs())).not.toContain('leechers');
	});

	it('round-trip through storage', () => {
		const saved: Record<string, string> = {};
		const prefs = moveColumn(toggleColumn(defaultPrefs(), 'files'), 'size', -1);
		expect(writePrefs(prefs, { setItem: (k, v) => (saved[k] = v) })).toBe(true);
		expect(readPrefs(store(saved[STORAGE_KEY]!))).toEqual(prefs);
	});

	it('survive corrupt storage', () => {
		for (const bad of ['{', 'null', '42', '"x"', '[]', '{"order":7,"hidden":"no"}'])
			expect(readPrefs(store(bad))).toEqual(defaultPrefs());
	});

	it('drop unknown columns, append missing ones, and never hide the name', () => {
		const p = normalize({
			order: ['size', 'bogus', 'size', 'name'],
			hidden: ['name', 'files', 'x']
		});
		expect(p.order.slice(0, 2)).toEqual(['size', 'name']);
		expect(new Set(p.order).size).toBe(p.order.length);
		expect(p.order).toContain('discovered');
		expect(p.hidden).toEqual(['files', 'raw', 'res', 'versions']);
	});

	it('show the raw name by default, and keep a saved choice that predates it', () => {
		expect(visible(defaultPrefs())).toContain('raw');
		const old = normalize({
			order: ['name', 'type', 'size', 'files', 'seeders', 'leechers', 'trackers', 'discovered'],
			hidden: ['leechers']
		});
		expect(visible(old)).not.toContain('raw');
		expect(visible(normalize({ order: ['name', 'raw', 'size'], hidden: [] }))).toContain('raw');
	});

	it('show the versions by default (GS-5), and keep a saved choice that predates them', () => {
		expect(visible(defaultPrefs())).toContain('versions');
		const before = normalize({ order: ['name', 'raw', 'res', 'size'], hidden: [] });
		expect(before.order.at(-1)).toBe('versions');
		expect(visible(before)).not.toContain('versions');
		expect(visible(normalize({ order: ['name', 'versions'], hidden: [] }))).toContain('versions');
	});

	it('show the resolution by default, and keep a saved choice that predates it', () => {
		expect(defaultPrefs().order).toEqual([
			'name',
			'raw',
			'type',
			'res',
			'size',
			'files',
			'seeders',
			'leechers',
			'trackers',
			'discovered',
			'versions'
		]);
		expect(visible(defaultPrefs())).toContain('res');
		// Saved after Raw name, before Resolution: Raw name stays as chosen, Resolution arrives hidden.
		const withRaw = normalize({
			order: ['name', 'type', 'raw', 'size', 'files'],
			hidden: ['raw']
		});
		expect(withRaw.order.slice(0, 5)).toEqual(['name', 'type', 'raw', 'size', 'files']);
		expect(withRaw.order).toContain('res');
		expect(withRaw.hidden).toEqual(['raw', 'res', 'versions']);
		const shownRaw = normalize({ order: ['name', 'raw', 'size'], hidden: [] });
		expect(visible(shownRaw)).toContain('raw');
		expect(visible(shownRaw)).not.toContain('res');
		// Saved before both: both arrive hidden.
		expect(normalize({ order: ['name', 'size'], hidden: [] }).hidden).toEqual([
			'raw',
			'res',
			'versions'
		]);
		// A choice that knows the column keeps it, shown or hidden, and toggling it back works.
		expect(visible(normalize({ order: ['name', 'res', 'raw'], hidden: [] }))).toContain('res');
		const off = normalize({ order: ['name', 'res', 'raw'], hidden: ['res'] });
		expect(visible(off)).not.toContain('res');
		expect(visible(normalize(toggleColumn(off, 'res')))).toContain('res');
		// No order stored at all reads as the defaults: shown.
		expect(visible(normalize({ hidden: [] }))).toContain('res');
	});

	it('work when storage throws or is missing', () => {
		const throwing = {
			getItem: () => {
				throw new Error('denied');
			},
			setItem: () => {
				throw new Error('full');
			}
		};
		expect(readPrefs(throwing)).toEqual(defaultPrefs());
		expect(writePrefs(defaultPrefs(), throwing)).toBe(false);
	});

	it('toggle and move', () => {
		expect(toggleColumn(defaultPrefs(), 'name')).toEqual(defaultPrefs());
		const moved = moveColumn(defaultPrefs(), 'type', 1);
		expect(moved.order.slice(2, 4)).toEqual(['res', 'type']);
		expect(moveColumn(defaultPrefs(), 'name', -1)).toEqual(defaultPrefs());
	});
});

describe('header sort', () => {
	it('goes largest first, then reverses', () => {
		expect(nextSort('relevance', 'size')).toBe('-size');
		expect(nextSort('-size', 'size')).toBe('+size');
		expect(nextSort('+size', 'size')).toBe('-size');
		expect(nextSort('-files', 'size')).toBe('-size');
		expect(nextSort('discovered', 'discovered')).toBe('+discovered');
	});

	it('reports aria-sort for the active column only', () => {
		expect(ariaSort('+size', 'size')).toBe('ascending');
		expect(ariaSort('-size', 'size')).toBe('descending');
		expect(ariaSort('size', 'size')).toBe('descending');
		expect(ariaSort('-files', 'size')).toBe('none');
		expect(ariaSort('relevance', 'size')).toBe('none');
	});
});

describe('the raw name', () => {
	it('shows only when it differs from the title', () => {
		expect(rawName({ title: 'The Lantern Keeper', name: 'The.Lantern.Keeper.1080p' })).toBe(
			'The.Lantern.Keeper.1080p'
		);
		expect(rawName({ title: 'same', name: 'same' })).toBeNull();
		expect(rawName({ title: null, name: 'no title' })).toBeNull();
	});
});
