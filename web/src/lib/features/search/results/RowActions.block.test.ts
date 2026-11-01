// After "Block this…" applies, the list keeps its rows and focus moves to the row that took the
// blocked row's place (or to the heading when none is left).
import { expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { hit, page } from './fixtures';
import BlockStub from './BlockStub.svelte';

vi.setConfig({ testTimeout: 20_000 });
const post = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { post }
}));
vi.mock('$lib/features/filters/flows-door', async () => ({
	load: async () => ({ BlockThisDialog: (await import('./BlockStub.svelte')).default })
}));
vi.mock('$lib/features/detail/send', () => ({ sendLabel: async () => null, sendHash: vi.fn() }));

const { results } = await import('./results.svelte');
const { default: RowActions } = await import('./RowActions.svelte');
void BlockStub;

const query = {
	ast: { op: 'term', value: 'x' } as never,
	sort: 'relevance',
	hidden: false,
	exact: false
};

async function blockRow(shown: number[], after: number[], blocked: number) {
	post.mockReset();
	post.mockResolvedValueOnce(page(shown));
	await results.load(query);
	document.body.innerHTML = '';
	const heading = document.body.appendChild(document.createElement('h2'));
	heading.id = 'rp-label';
	heading.tabIndex = -1;
	// The DOM stands for the list after the reload (the real one re-renders on the new rows).
	for (const [i, h] of after.map((n) => hit(n)).entries()) {
		const row = document.body.appendChild(document.createElement('div'));
		row.dataset.index = String(i);
		row.tabIndex = -1;
		row.innerHTML = `<div data-hash="${h.hash}"></div>`;
	}
	post.mockResolvedValueOnce(page(after));
	render(RowActions, {
		hit: hit(blocked),
		density: 'comfortable',
		testid: 'r'
	});
	(await screen.findByTestId('r-more')).click();
	(await screen.findByRole('menuitem', { name: /block/i }, { timeout: 10_000 })).click();
	(await screen.findByTestId('stub-apply', {}, { timeout: 10_000 })).click();
	return heading;
}

it('focuses the row that took the blocked row’s place and announces it', async () => {
	await blockRow([1, 2, 3], [1, 3], 2);
	await vi.waitFor(() => expect(document.activeElement?.getAttribute('data-index')).toBe('1'), {
		timeout: 10_000
	});
	expect(results.note).toBe('Blocked. 1 result removed.');
});

it('focuses the heading when nothing is left', async () => {
	const heading = await blockRow([1], [], 1);
	await vi.waitFor(() => expect(document.activeElement).toBe(heading), { timeout: 10_000 });
});
