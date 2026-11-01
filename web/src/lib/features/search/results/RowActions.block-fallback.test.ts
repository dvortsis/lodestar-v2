// If the settle chunk can't load (offline, a stale build), Block still reloads the list.
import { expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { hit, page } from './fixtures';

vi.setConfig({ testTimeout: 20_000 });
const post = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { post }
}));
vi.mock('$lib/features/filters/flows-door', async () => ({
	load: async () => ({
		BlockThisDialog: (await import('./BlockDialog.test-double.svelte')).default
	})
}));
vi.mock('$lib/features/detail/send', () => ({ sendLabel: async () => null, sendHash: vi.fn() }));
vi.mock('./settle', () => {
	throw new Error('chunk failed');
});

const { results } = await import('./results.svelte');
const { default: RowActions } = await import('./RowActions.svelte');

it('reloads the list when the settle chunk fails to load', async () => {
	post.mockReset();
	post.mockResolvedValueOnce(page([1, 2]));
	await results.load({
		ast: { op: 'term', value: 'x' } as never,
		sort: 'relevance',
		hidden: false,
		exact: false
	});
	post.mockResolvedValueOnce(page([1]));
	render(RowActions, { hit: hit(2), density: 'comfortable', testid: 'r' });
	(await screen.findByTestId('r-more')).click();
	(await screen.findByRole('menuitem', { name: /block/i }, { timeout: 10_000 })).click();
	(await screen.findByTestId('stub-apply', {}, { timeout: 10_000 })).click();
	await vi.waitFor(() => expect(results.hits).toHaveLength(1), { timeout: 10_000 });
});
