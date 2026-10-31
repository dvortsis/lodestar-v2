import { expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';

const state = vi.hoisted(() => ({
	fresh: 0,
	reload: vi.fn(async () => {}),
	reset: vi.fn(),
	follow: vi.fn(),
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
		follow: state.follow,
		reset: state.reset
	})
}));
vi.mock('../results/results.svelte', () => ({
	results: { shown: 'k', status: 'ready', reload: state.reload }
}));

const { default: LiveHost } = await import('./LiveHost.svelte');
const moveTo = vi.fn();
const list = () => ({ moveTo });

it('shows nothing until results are new, and Show reloads from the top', async () => {
	const { unmount } = render(LiveHost, { list });
	expect(screen.queryByTestId('results-live')).toBeNull();
	unmount();

	state.fresh = 1;
	render(LiveHost, { list });
	expect(screen.getByTestId('results-live').textContent).toBe('1 new result');
	await fireEvent.click(screen.getByTestId('results-live-show'));
	expect(state.reset).toHaveBeenCalled();
	// Show registers again, so the count starts from 0.
	await vi.waitFor(() => expect(state.follow).toHaveBeenCalledWith(expect.anything(), true), {
		timeout: 10_000
	});
	await vi.waitFor(() => expect(state.reload).toHaveBeenCalledTimes(1), { timeout: 10_000 });
	// The list owns its active row and focus after Show.
	await vi.waitFor(() => expect(moveTo).toHaveBeenCalledWith(0), { timeout: 10_000 });
});

it('counts with the locale formatter', () => {
	state.fresh = 1240;
	render(LiveHost, { list });
	expect(screen.getByTestId('results-live').textContent).toBe('1,240 new results');
});

it('pauses polling while a selection is held or the list is scrolled deep', () => {
	state.fresh = 0;
	render(LiveHost, { list });
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
