import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import type { Ast } from '$lib/state/search.svelte';
import FacetRail from './FacetRail.svelte';
import { response, term } from './fixtures';

const word = (w: string): Ast => ({ op: 'word', field: 'any', word: w });
const props = (over: Record<string, unknown> = {}) => ({
	data: response({
		facets: {
			type: term([['movie', 5]]),
			res: term([['1080p', 3]]),
			lang: term([['en', 9]]),
			ext: term([['mkv', 4]])
		},
		histograms: {
			...response().histograms,
			year: {
				approx: false,
				missing: 0,
				unit: 'year',
				bins: [{ lo: 2020, hi: 2021, count: 4, backfill: false }]
			}
		}
	}),
	ast: word('lantern'),
	error: null,
	more: new Set<string>(),
	onmore: vi.fn(),
	onchange: vi.fn(),
	onretry: vi.fn(),
	...over
});

const position = (el: Element, other: Element) =>
	el.compareDocumentPosition(other) & Node.DOCUMENT_POSITION_FOLLOWING;

describe('FacetRail order (UI review U10, D3)', () => {
	it('is Type, Resolution, Size, Seeders, Language, Year, Discovered, then More filters', () => {
		render(FacetRail, props());
		const ids = [
			'facet-type',
			'facet-res',
			'facet-size',
			'facet-seeders',
			'facet-lang',
			'facet-year',
			'facet-discovered',
			'facet-more'
		];
		const els = ids.map((id) => screen.getByTestId(id));
		for (let i = 1; i < els.length; i++)
			expect(position(els[i - 1]!, els[i]!), `${ids[i - 1]} before ${ids[i]}`).toBeTruthy();
	});

	it('keeps Source, Extension, Trackers reporting and the flags under More filters', () => {
		render(FacetRail, props());
		const more = screen.getByTestId('facet-more');
		for (const id of ['facet-ext', 'facet-trackers'])
			expect(more.contains(screen.getByTestId(id))).toBe(true);
		for (const id of ['facet-seeders', 'facet-lang', 'facet-year', 'facet-discovered'])
			expect(more.contains(screen.getByTestId(id))).toBe(false);
	});
});

describe('Discovered typed dates (UI review U15)', () => {
	it('Custom shows From and To; typing both sets whole UTC days; From = To is one day', async () => {
		const onchange = vi.fn();
		render(FacetRail, props({ onchange }));
		expect(screen.queryByTestId('facet-discovered-dates')).toBeNull();
		await fireEvent.click(screen.getByTestId('facet-discovered-custom'));
		const from = screen.getByTestId('facet-discovered-from') as HTMLInputElement;
		const to = screen.getByTestId('facet-discovered-to') as HTMLInputElement;
		await fireEvent.input(from, { target: { value: '2026-11-28' } });
		await fireEvent.change(from);
		await fireEvent.input(to, { target: { value: '2026-11-28' } });
		await fireEvent.change(to);
		const day = Date.parse('2026-11-28T00:00:00Z') / 1000;
		await waitFor(() =>
			expect(onchange).toHaveBeenLastCalledWith({
				op: 'and',
				of: [
					word('lantern'),
					{ op: 'date', field: 'discovered', from: { at: day }, to: { at: day + 86_399 } }
				]
			})
		);
	});

	it('an empty To leaves the end open', async () => {
		const onchange = vi.fn();
		render(FacetRail, props({ onchange }));
		await fireEvent.click(screen.getByTestId('facet-discovered-custom'));
		const from = screen.getByTestId('facet-discovered-from') as HTMLInputElement;
		await fireEvent.input(from, { target: { value: '2026-11-01' } });
		await fireEvent.change(from);
		const day = Date.parse('2026-11-01T00:00:00Z') / 1000;
		await waitFor(() =>
			expect(onchange).toHaveBeenLastCalledWith({
				op: 'and',
				of: [word('lantern'), { op: 'date', field: 'discovered', from: { at: day }, to: null }]
			})
		);
	});

	it('says what to change and leaves the clause alone when From is after To', async () => {
		const onchange = vi.fn();
		render(FacetRail, props({ onchange }));
		await fireEvent.click(screen.getByTestId('facet-discovered-custom'));
		const from = screen.getByTestId('facet-discovered-from') as HTMLInputElement;
		const to = screen.getByTestId('facet-discovered-to') as HTMLInputElement;
		await fireEvent.input(to, { target: { value: '2026-11-01' } });
		await fireEvent.change(to);
		onchange.mockClear();
		await fireEvent.input(from, { target: { value: '2026-11-05' } });
		await fireEvent.change(from);
		await waitFor(() =>
			expect(screen.getByTestId('facet-discovered-dates-error').textContent).toContain('after')
		);
		expect(onchange).not.toHaveBeenCalled();
		expect(from.getAttribute('aria-invalid')).toBe('true');
	});
});
