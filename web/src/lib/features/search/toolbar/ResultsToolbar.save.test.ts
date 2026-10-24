// Save search when the dialog's chunk can't load (offline, or a redeploy replaced it): an error
// toast, and the button is usable again. A file of its own: the import has to fail from the start.
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { expect, it, vi } from 'vitest';
import { hit } from '../results/fixtures';

vi.setConfig({ testTimeout: 20_000 });
const WAIT = { timeout: 10_000 };

vi.mock('$app/state', async () => ({
	page: (await import('../query/testing.svelte')).testPage
}));
vi.mock('$app/navigation', () => ({ goto: vi.fn(), replaceState: vi.fn(), pushState: vi.fn() }));
vi.mock('$lib/state/search.svelte', async (original) => ({
	...(await original<typeof import('$lib/state/search.svelte')>()),
	search: { q: 'lantern', sort: 'relevance', setSort: vi.fn() }
}));
vi.mock('../results/results.svelte', () => ({
	results: { status: 'ready', hits: [hit(1)], total: 1, totalApprox: false }
}));
const push = vi.hoisted(() => vi.fn());
vi.mock('$lib/ui/feedback/toasts.svelte', () => ({ toast: { push } }));
vi.mock('$lib/features/alerts/SaveSearchDialog.svelte', () => {
	throw new Error('Failed to fetch dynamically imported module');
});

const { default: ResultsToolbar } = await import('./ResultsToolbar.svelte');

it('says so in a toast when the dialog can’t load, and the button works again', async () => {
	render(ResultsToolbar);
	const button = screen.getByRole('button', { name: 'Save search' });
	await fireEvent.click(button);
	await waitFor(
		() =>
			expect(push).toHaveBeenCalledWith({
				kind: 'error',
				text: "Couldn't open Save search. Reload the page and try again."
			}),
		WAIT
	);
	await waitFor(() => expect(button.getAttribute('aria-busy')).not.toBe('true'), WAIT);
	expect(screen.queryByRole('dialog')).toBeNull();
});
