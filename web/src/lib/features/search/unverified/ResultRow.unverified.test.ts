import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import Card from '../cards/Card.svelte';
import ResultRow from '../results/ResultRow.svelte';
import { hit } from '../results/fixtures';

describe('ResultRow: Unverified badge', () => {
	for (const density of ['comfortable', 'compact'] as const) {
		it(`shows it for an unverified hit (${density})`, () => {
			render(ResultRow, { hit: hit(1, { verified: false }), density, testid: 'r' });
			const badge = screen.getByTestId('unverified-badge');
			expect(badge.textContent).toContain('Unverified');
			expect(badge.querySelector('svg')).toBeTruthy();
		});
	}
	it('shows nothing for a verified hit or one without the field', () => {
		const { unmount } = render(ResultRow, {
			hit: hit(1, { verified: true }),
			density: 'comfortable',
			testid: 'r'
		});
		expect(screen.queryByTestId('unverified-badge')).toBeNull();
		unmount();
		render(ResultRow, { hit: hit(2), density: 'comfortable', testid: 'r' });
		expect(screen.queryByTestId('unverified-badge')).toBeNull();
	});
});

describe('Card: Unverified badge', () => {
	it('shows it for an unverified hit and not for a verified one', () => {
		const { unmount } = render(Card, {
			hit: hit(1, { verified: false }),
			testid: 'c',
			onopen: () => {}
		});
		expect(screen.getByTestId('unverified-badge').textContent).toContain('Unverified');
		unmount();
		render(Card, { hit: hit(2, { verified: true }), testid: 'c', onopen: () => {} });
		expect(screen.queryByTestId('unverified-badge')).toBeNull();
	});
});

describe('Imported badge', () => {
	const imported = { verified: false, imported: true };
	it('shows Imported in place of Unverified on a row', () => {
		render(ResultRow, { hit: hit(1, imported), density: 'comfortable', testid: 'r' });
		const badge = screen.getByTestId('imported-badge');
		expect(badge.textContent).toContain('Imported');
		expect(badge.querySelector('svg')).toBeTruthy();
		expect(badge.getAttribute('title')).toContain("another indexer's database");
		// Read with the label, for keyboard and screen-reader users too.
		expect(badge.querySelector('.sr-only')?.textContent).toContain("another indexer's database");
		expect(screen.queryByTestId('unverified-badge')).toBeNull();
	});
	it('shows Imported in place of Unverified on a card', () => {
		render(Card, { hit: hit(1, imported), testid: 'c', onopen: () => {} });
		expect(screen.getByTestId('imported-badge').textContent).toContain('Imported');
		expect(screen.queryByTestId('unverified-badge')).toBeNull();
	});
	it('is not shown for an unverified or a verified hit', () => {
		const { unmount } = render(ResultRow, {
			hit: hit(1, { verified: false }),
			density: 'compact',
			testid: 'r'
		});
		expect(screen.queryByTestId('imported-badge')).toBeNull();
		unmount();
		render(ResultRow, { hit: hit(2, { verified: true }), density: 'compact', testid: 'r' });
		expect(screen.queryByTestId('imported-badge')).toBeNull();
	});
});
