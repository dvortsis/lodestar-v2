import { expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';

const state = vi.hoisted(() => ({
	fresh: 0,
	reload: vi.fn(async () => {}),
	reset: vi.fn(),
	deps: undefined as undefined | { paused: () => boolean },
	selection: { mode: false, count: 0 }
}));
vi.mock('../bulk/selection.svelte', () => ({ selection: state.selection }));
vi.mock('./live.svelte', () => ({
	createLive: (deps: { paused: () => boolean }) => ({
		_: (state.deps = deps),
		get fresh() {
			return state.fresh;
		},
		start: () => () => {},
		reset: state.reset
	})
}));
vi.mock('../results/results.svelte', () => ({
	results: { shown: 'k', status: 'ready', reload: state.reload }
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
	await vi.waitFor(() => expect(state.reload).toHaveBeenCalledTimes(1), { timeout: 10_000 });
});

it('counts with the locale formatter', () => {
	state.fresh = 1240;
	render(LiveHost, {});
	expect(screen.getByTestId('results-live').textContent).toBe('1,240 new results');
});

it('pauses polling while a selection is held or the list is scrolled deep', () => {
	state.fresh = 0;
	render(LiveHost, {});
	const paused = () => state.deps!.paused();
	expect(paused()).toBe(false);
	state.selection.mode = true;
	expect(paused()).toBe(true);
	state.selection.mode = false;
	state.selection.count = 2;
	expect(paused()).toBe(true);
	state.selection.count = 0;

	const grid = document.createElement('div');
	grid.id = 'results';
	grid.innerHTML = '<div role="grid"></div>';
	document.body.append(grid);
	const g = grid.querySelector<HTMLElement>('[role="grid"]')!;
	Object.defineProperty(g, 'clientHeight', { value: 500 });
	g.scrollTop = 900;
	expect(paused()).toBe(false);
	g.scrollTop = 1200;
	expect(paused()).toBe(true);
	grid.remove();
});
