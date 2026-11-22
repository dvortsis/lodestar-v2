import { beforeEach, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/svelte';
import FindPanel from './FindPanel.svelte';

const find = vi.hoisted(() => ({ load: vi.fn() }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('$app/paths', () => ({ resolve: (path: string) => path }));
vi.mock('./find', () => ({
	loadFindIndex: find.load,
	search: () => [],
	describe: () => Promise.resolve(undefined),
	forgetValues: () => {},
	highlight: () => () => {}
}));

beforeEach(() => {
	find.load.mockReset();
	find.load.mockRejectedValue(new Error('offline'));
});

const tick = () => new Promise((r) => setTimeout(r, 50));

it('asks for the index once after a failure, and again only when the text changes', async () => {
	const input = document.createElement('input');
	document.body.append(input);
	const { rerender } = render(FindPanel, { input, query: 'p', clear: () => {} });
	await waitFor(() => expect(screen.getByText(/couldn't load/i)).toBeTruthy());
	await tick();
	expect(find.load).toHaveBeenCalledTimes(1);
	// The same text with other spacing is the same query: no new request, a steady message.
	await rerender({ input, query: 'p ', clear: () => {} });
	await tick();
	expect(find.load).toHaveBeenCalledTimes(1);
	expect(screen.getByText(/couldn't load/i)).toBeTruthy();
	// New text asks again, once.
	await rerender({ input, query: 'pa', clear: () => {} });
	await waitFor(() => expect(find.load).toHaveBeenCalledTimes(2));
	await tick();
	expect(find.load).toHaveBeenCalledTimes(2);
	input.remove();
});
