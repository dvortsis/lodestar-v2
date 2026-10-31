import { describe, expect, it } from 'vitest';
import {
	STORAGE_KEY,
	ariaSort,
	defaultPrefs,
	moveColumn,
	nextSort,
	normalize,
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
		expect(p.hidden).toEqual(['files']);
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
		expect(moved.order.slice(1, 3)).toEqual(['size', 'type']);
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
