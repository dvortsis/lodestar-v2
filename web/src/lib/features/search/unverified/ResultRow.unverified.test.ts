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
