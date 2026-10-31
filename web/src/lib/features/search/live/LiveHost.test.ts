import { expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';

const state = vi.hoisted(() => ({ fresh: 0, retry: vi.fn(async () => {}), reset: vi.fn() }));
vi.mock('./live.svelte', () => ({
	createLive: () => ({
		get fresh() {
			return state.fresh;
		},
		start: () => () => {},
		reset: state.reset
	})
}));
vi.mock('../results/results.svelte', () => ({
	results: { shown: 'k', status: 'ready', retry: state.retry }
}));

const { default: LiveHost } = await import('./LiveHost.svelte');

it('shows nothing until results are new, and Show reloads from the top', async () => {
	const { unmount } = render(LiveHost, {});
	expect(screen.queryByTestId('results-live')).toBeNull();
	unmount();

	state.fresh = 1;
	render(LiveHost, {});
	expect(screen.getByTestId('results-live').textContent).toBe('1 new result');
	await fireEvent.click(screen.getByTestId('results-live-show'));
	expect(state.reset).toHaveBeenCalled();
	await vi.waitFor(() => expect(state.retry).toHaveBeenCalledTimes(1), { timeout: 10_000 });
});

it('counts with the locale formatter', () => {
	state.fresh = 1240;
	render(LiveHost, {});
	expect(screen.getByTestId('results-live').textContent).toBe('1,240 new results');
});
