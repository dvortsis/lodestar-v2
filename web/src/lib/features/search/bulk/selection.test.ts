import { describe, expect, it } from 'vitest';
import { countText, pick, selectAll } from './ops';
import { createSelection, selection } from './selection.svelte';

const rows = ['a', 'b', 'c', 'd', 'e'];
const click = (checked: boolean, shiftKey = false) =>
	({ shiftKey, currentTarget: { checked }, stopPropagation() {} }) as never;

describe('selection', () => {
	it('toggles rows and turns select mode on', () => {
		const s = createSelection();
		expect(s.mode).toBe(false);
		s.toggle('a');
		expect([s.hashes, s.mode, s.has('a')]).toEqual([['a'], true, true]);
		s.toggle('a');
		expect(s.count).toBe(0);
		s.toggle('b', true);
		s.toggle('b', true);
		expect(s.hashes).toEqual(['b']);
	});

	it('keeps the selection across appended pages and clears on a new query', () => {
		const s = createSelection();
		s.follow('q1');
		s.toggle('a');
		s.follow('q1'); // a next page: the same query
		expect(s.hashes).toEqual(['a']);
		s.follow('q2');
		expect(s.count).toBe(0);
	});

	it('leaving select mode clears the selection', () => {
		const s = createSelection();
		s.toggle('a');
		s.mode = false;
		expect([s.count, s.mode]).toEqual([0, false]);
	});
});

describe('selection helpers', () => {
	it('extend a range from the last row toggled, in list order', () => {
		selection.mode = false;
		pick(click(true), 'b', rows);
		pick(click(true, true), 'd', rows);
		expect(selection.hashes).toEqual(['b', 'c', 'd']);
		pick(click(true, true), 'a', rows);
		expect(selection.hashes).toEqual(['b', 'c', 'd', 'a']);
		selection.mode = false;
	});

	it('select all loaded rows, then none of them', () => {
		selection.mode = false;
		selection.toggle('z');
		selectAll(rows);
		expect(selection.count).toBe(6);
		selectAll(rows);
		expect(selection.hashes).toEqual(['z']);
		selection.mode = false;
	});

	it('say the count for the polite region', () => {
		const f = (n: number) => String(n);
		expect([0, 1, 3].map((n) => countText(n, f))).toEqual([
			'Selection cleared',
			'1 selected',
			'3 selected'
		]);
	});
});
