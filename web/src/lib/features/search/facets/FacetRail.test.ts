import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import type { Ast } from '$lib/state/search.svelte';
import FacetRail from './FacetRail.svelte';
import { response, term } from './fixtures';

const word = (w: string): Ast => ({ op: 'word', field: 'any', word: w });
const props = (over: Record<string, unknown> = {}) => ({
	data: response(),
	ast: word('lantern'),
	error: null,
	more: new Set<string>(),
	onmore: vi.fn(),
	onchange: vi.fn(),
	onretry: vi.fn(),
	...over
});

describe('FacetRail: Adult facets (§20.9)', () => {
	it('names the adult subtypes the server lists, in words', () => {
		const data = response({
			facets: {
				type: term([['adult', 40]]),
				adult: term([
					['scene', 25],
					['vr', 5]
				])
			}
		});
		render(FacetRail, props({ data }));
		expect(screen.getAllByLabelText('Adult').length).toBeGreaterThan(0);
		expect(screen.getByLabelText('Scene')).toBeTruthy();
		expect(screen.getByLabelText('VR')).toBeTruthy();
	});

	it('shows nothing adult when the server lists nothing adult (18+ off)', () => {
		const data = response({ facets: { type: term([['movie', 812]]) } });
		render(FacetRail, props({ data }));
		expect(screen.queryByTestId('facet-adult')).toBeNull();
		expect(screen.queryByLabelText('Adult')).toBeNull();
		expect(screen.queryByLabelText('Scene')).toBeNull();
	});
});

describe('FacetRail', () => {
	it('shows the server’s counts with a test id each, and labels types', () => {
		render(FacetRail, props());
		expect(screen.getByTestId('facet-type-movie-count').textContent).toContain('812');
		expect(screen.getByLabelText('Movie')).toBeTruthy();
		expect(screen.getByTestId('facet-res-480p-count').textContent).toContain('33');
		expect(screen.getByTestId('facet-ext-other').textContent).toContain('220');
	});

	it('marks estimated counts with ≈, and says so once', () => {
		const data = response({
			sampled: true,
			facets: { type: term([['movie', 812_000]], { approx: true }) }
		});
		render(FacetRail, props({ data }));
		expect(screen.getByTestId('facet-type-movie-count').textContent).toContain('≈');
		expect(screen.getAllByTestId('facet-sampled')).toHaveLength(1);
	});

	it('hands every click to onchange as an AST edit', async () => {
		const onchange = vi.fn();
		render(FacetRail, props({ onchange }));
		await fireEvent.click(screen.getByTestId('facet-res-2160p'));
		expect(onchange).toHaveBeenLastCalledWith({
			op: 'and',
			of: [word('lantern'), { op: 'tag', key: 'res', any: ['2160p'] }]
		});
		await fireEvent.click(screen.getByTestId('facet-res-480p-exclude'));
		expect(onchange).toHaveBeenLastCalledWith({
			op: 'and',
			of: [word('lantern'), { op: 'not', of: { op: 'tag', key: 'res', any: ['480p'] } }]
		});
		await fireEvent.click(screen.getByTestId('facet-discovered-86400'));
		expect(onchange).toHaveBeenLastCalledWith({
			op: 'and',
			of: [word('lantern'), { op: 'date', field: 'discovered', from: { ago: 86_400 }, to: null }]
		});
	});

	it('disables the values only the query text can change, and says why', () => {
		// -(res:480p & res:1080p): un-excluding one of them by a click would change the logic.
		const not = (of: Ast): Ast => ({ op: 'not', of });
		const res = (v: string): Ast => ({ op: 'tag', key: 'res', any: [v] });
		const ast: Ast = {
			op: 'and',
			of: [word('lantern'), not({ op: 'and', of: [res('480p'), res('1080p')] })]
		};
		const data = response({
			facets: {
				res: term([
					['1080p', 640],
					['480p', 33],
					['2160p', 12]
				])
			}
		});
		const onchange = vi.fn();
		render(FacetRail, props({ data, ast, onchange }));
		const note = screen.getByTestId('facet-res-fixed');
		expect(note.textContent).toContain('Set by your query: edit it in the search box.');
		for (const v of ['480p', '1080p']) {
			const box = screen.getByTestId(`facet-res-${v}`) as HTMLInputElement;
			const minus = screen.getByTestId(`facet-res-${v}-exclude`) as HTMLButtonElement;
			expect(box.disabled).toBe(true);
			expect(minus.disabled).toBe(true);
			expect(box.getAttribute('aria-describedby')).toBe(note.id);
			expect(minus.getAttribute('aria-describedby')).toBe(note.id);
		}
		const free = screen.getByTestId('facet-res-2160p') as HTMLInputElement;
		expect(free.disabled).toBe(false);
		expect(free.getAttribute('aria-describedby')).toBeNull();
		expect(screen.queryByTestId('facet-type-fixed')).toBeNull();
	});

	it('shows a selected value checked, in medium weight, with a Clear', () => {
		const data = response({ facets: { res: term([['1080p', 640, 'selected']]) } });
		render(FacetRail, props({ data }));
		const box = screen.getByTestId('facet-res-1080p') as HTMLInputElement;
		expect(box.checked).toBe(true);
		expect(box.closest('li')?.classList.contains('fr-selected')).toBe(true);
		expect(screen.getByTestId('facet-res-clear')).toBeTruthy();
	});

	it('offers no exclusion on a yes/no flag', () => {
		render(FacetRail, props());
		expect(screen.queryByTestId('facet-private-yes-exclude')).toBeNull();
		expect(screen.getByLabelText('Yes')).toBeTruthy();
	});

	describe('the swarm groups', () => {
		it('say "not available yet" before swarm indexing, never zeros', () => {
			render(FacetRail, props());
			for (const name of ['seeders', 'trackers']) {
				expect(screen.getByTestId(`facet-${name}-unavailable`).textContent).toContain(
					'Not available yet'
				);
				expect(screen.queryByTestId(`facet-${name}-1-count`)).toBeNull();
			}
		});

		it('offer the presets with the server’s counts once it sends them', async () => {
			const onchange = vi.fn();
			const seeders = term([
				['≥1', 900],
				['≥5', 500],
				['≥50', 40],
				['never_scraped', 384]
			]);
			const data = response({ buckets: { seeders, trackers: null } });
			render(FacetRail, props({ data, onchange }));
			expect(screen.getByTestId('facet-seeders-5-count').textContent).toBe('500');
			expect(screen.queryByTestId('facet-seeders-unavailable')).toBeNull();
			await fireEvent.click(screen.getByTestId('facet-seeders-5'));
			expect(onchange).toHaveBeenLastCalledWith({
				op: 'and',
				of: [word('lantern'), { op: 'range', field: 'seeders', lo: 5, hi: null }]
			});
		});
	});

	it('says a group the index can’t serve is unavailable', () => {
		const data = response({ unavailable: ['seeders', 'trackers', 'codec'] });
		render(FacetRail, props({ data }));
		expect(screen.getByTestId('facet-codec-unavailable')).toBeTruthy();
	});

	it('says nothing for a facet with no values for this query', () => {
		const data = response({ facets: { ...response().facets, hdr: term([]) } });
		render(FacetRail, props({ data }));
		expect(screen.queryByTestId('facet-hdr')).toBeNull();
		expect(screen.queryByText('No extension matches')).toBeNull();
	});

	it('keeps the exclude button’s name and says “excluded” in the value’s own name', () => {
		const data = response({ facets: { res: term([['480p', 33, 'excluded']]) } });
		render(FacetRail, props({ data }));
		const button = screen.getByTestId('facet-res-480p-exclude');
		expect(button.getAttribute('aria-label')).toBe('Exclude 480p');
		expect(button.getAttribute('aria-pressed')).toBe('true');
		expect(screen.getByLabelText('480p (excluded)')).toBeTruthy();
	});

	it('lists the release year under More filters, as a range', async () => {
		const onchange = vi.fn();
		const data = response({
			histograms: {
				...response().histograms,
				year: {
					approx: false,
					missing: 12,
					unit: 'year',
					bins: [
						{ lo: 2019, hi: 2020, count: 4, backfill: false },
						{ lo: 2020, hi: 2021, count: 6, backfill: false }
					]
				}
			}
		});
		render(FacetRail, props({ data, onchange }));
		expect(
			screen.getByTestId('facet-more').querySelector('[data-testid="facet-year"]')
		).toBeTruthy();
		expect(screen.getByTestId('facet-year-missing').textContent).toContain('12');
	});

	it('says a histogram the index can’t serve is unavailable', () => {
		const histograms = { ...response().histograms };
		delete histograms.size;
		const data = response({ histograms, unavailable: ['seeders', 'trackers', 'size'] });
		render(FacetRail, props({ data }));
		expect(screen.getByTestId('facet-size-unavailable')).toBeTruthy();
	});

	it('offers Retry when the counts fail', async () => {
		const onretry = vi.fn();
		const { ApiError } = await import('$lib/api/client');
		render(FacetRail, props({ data: null, error: new ApiError(503, 'unavailable', 'x'), onretry }));
		await fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
		expect(onretry).toHaveBeenCalled();
	});
});
