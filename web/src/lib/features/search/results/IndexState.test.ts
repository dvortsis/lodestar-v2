// The "still growing" banner never shows a rate that would read "+0/h" (S-search-recover-ui).
import { render, screen } from '@testing-library/svelte';
import { expect, it, vi } from 'vitest';

vi.mock('./results.svelte', () => ({ results: { status: 'ready' } }));
const { default: IndexState } = await import('./IndexState.svelte');

const figures = (growth_per_hour: number | null) => ({
	cold_start: false,
	growing: true,
	growth_hours: 24,
	growth_per_hour,
	rebuild: null,
	searchable_torrents: 5_000,
	unavailable: [],
	updated_at: 0
});
const shown = (rate: number | null) => {
	const { unmount } = render(IndexState, {
		banner: { kind: 'growing' } as never,
		state: figures(rate),
		focusBar: () => {}
	});
	const text = screen.getByTestId('results-cold-start').textContent ?? '';
	unmount();
	return text;
};

it('leaves the rate out for null, 0 and under 1 per hour', () => {
	for (const rate of [null, 0, 0.5]) expect(shown(rate)).not.toContain('/h');
});

it('shows the rate from 1 per hour', () => {
	expect(shown(2104.5)).toContain('/h');
});
