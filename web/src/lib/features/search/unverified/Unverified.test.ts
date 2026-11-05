import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import { response, term } from '../facets/fixtures';
import SourceFacets from './SourceFacets.svelte';
import UnverifiedBadge from './UnverifiedBadge.svelte';
import UnverifiedToggle from './UnverifiedToggle.svelte';

describe('UnverifiedBadge', () => {
	it('shows text and an icon', () => {
		render(UnverifiedBadge);
		const el = screen.getByTestId('unverified-badge');
		expect(el.textContent).toContain('Unverified');
		expect(el.querySelector('svg')).toBeTruthy();
	});
});

describe('UnverifiedToggle', () => {
	it('is off by default and asks for verified:yes,no when checked', async () => {
		const onchange = vi.fn();
		render(UnverifiedToggle, { ast: { op: 'all' }, onchange });
		const box = screen.getByRole('checkbox', { name: 'Show unverified' }) as HTMLInputElement;
		expect(box.checked).toBe(false);
		await fireEvent.click(box);
		expect(onchange).toHaveBeenCalledWith({ op: 'verified', any: ['yes', 'no'] });
	});
	it('is on when the query already shows them, and drops the clause when unchecked', async () => {
		const onchange = vi.fn();
		render(UnverifiedToggle, { ast: { op: 'verified', any: ['no'] }, onchange });
		const box = screen.getByRole('checkbox') as HTMLInputElement;
		expect(box.checked).toBe(true);
		await fireEvent.click(box);
		expect(onchange).toHaveBeenCalledWith({ op: 'all' });
	});
	it('shows the server count when given', () => {
		render(UnverifiedToggle, {
			ast: null,
			onchange: vi.fn(),
			count: { value: 1234, approx: false }
		});
		expect(screen.getByTestId('unverified-count').textContent).toBe('1,234');
	});
	it('marks an approximate count with ≈', () => {
		render(UnverifiedToggle, {
			ast: null,
			onchange: vi.fn(),
			count: { value: 1234, approx: true }
		});
		expect(screen.getByTestId('unverified-count').textContent).toMatch(/^≈/);
	});
});

describe('SourceFacets', () => {
	const data = (over = {}) =>
		response({
			facets: {
				source: term(
					[
						['dht', 900],
						['nyaa', 40]
					],
					{ other: 7 }
				),
				verified: term([
					['yes', 900, 'selected'],
					['no', 12]
				])
			},
			source_names: { dht: 'Mainline DHT', nyaa: 'Nyaa' },
			...over
		});
	it('lists sources by display name and selects one as a source: clause', async () => {
		const onchange = vi.fn();
		render(SourceFacets, { data: data(), ast: { op: 'all' }, onchange });
		expect(screen.getByText('DHT')).toBeTruthy();
		await fireEvent.click(screen.getByRole('checkbox', { name: /Nyaa/ }));
		expect(onchange).toHaveBeenCalledWith({ op: 'source', any: ['nyaa'] });
	});
	it('shows the unverified count from the verified facet', () => {
		render(SourceFacets, { data: data(), ast: null, onchange: vi.fn() });
		expect(screen.getByTestId('unverified-count').textContent).toBe('12');
	});
	it('names nothing for the other bucket and notes partial counts', () => {
		const d = data();
		d.facets.source = term([['dht', 900]], { other: 7, partial: true });
		render(SourceFacets, { data: d, ast: null, onchange: vi.fn() });
		expect(screen.getByTestId('facet-source-other').textContent).toContain('7');
		expect(screen.getByTestId('facet-source-partial')).toBeTruthy();
	});
	it('renders only the toggle when the server lists no sources', () => {
		const d = data();
		delete (d.facets as Record<string, unknown>).source;
		render(SourceFacets, { data: d, ast: null, onchange: vi.fn() });
		expect(screen.queryByTestId('facet-source-wrap')).toBeNull();
		expect(screen.getByTestId('unverified-toggle')).toBeTruthy();
	});
});
