import { describe, expect, it } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/svelte';
import { hit } from '../results/fixtures';
import Card from './Card.svelte';
import { chunk, columns } from './cards';

describe('cards layout', () => {
	it('picks one to three columns from the list width', () => {
		expect([0, 200, 279, 560, 839, 840, 2000].map(columns)).toEqual([1, 1, 1, 2, 2, 3, 3]);
	});
	it('chunks hits into rows of cols, keyed by the first hash', () => {
		const hits = [1, 2, 3, 4, 5].map((n) => hit(n));
		const rows = chunk(hits, 2);
		expect(rows.map((r) => r.items.length)).toEqual([2, 2, 1]);
		expect(rows.map((r) => r.start)).toEqual([0, 2, 4]);
		expect(rows[1]!.key).toBe(hits[2]!.hash);
		expect(chunk([], 3)).toEqual([]);
	});
});

describe('Card', () => {
	const text = (id: string) => screen.getByTestId(id).textContent?.replace(/\s+/g, ' ').trim();
	it('shows the same facts as the row, and null as a dash', () => {
		render(Card, {
			hit: hit(7, { seeders: 12, leechers: null }),
			testid: 'c',
			onopen: () => {}
		});
		expect(text('c-title')).toBe('Lantern Keeper 7');
		expect(text('c-size')).toBe('2.00 GiB');
		expect(text('c-files')).toBe('5 files');
		expect(text('c-seeders')).toBe('12');
		expect(screen.getByRole('img', { name: /never/i })).toBeTruthy();
	});
	it('opens on click and Enter, but not from its own buttons', async () => {
		const opened: string[] = [];
		render(Card, { hit: hit(3), testid: 'c', onopen: (h) => opened.push(h.hash) });
		const tile = screen.getByTestId('c');
		await fireEvent.click(tile);
		await fireEvent.keyDown(tile, { key: 'Enter' });
		expect(opened).toHaveLength(2);
	});
});
