import { expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { torrentDetail } from './fixtures';
import DetailActions from './DetailActions.svelte';

vi.mock('$lib/features/filters/flows-door', () => ({
	load: () => Promise.reject(new Error('chunk failed'))
}));
vi.mock('./send', () => ({ sendLabel: async () => null, sendHash: vi.fn() }));

it('says so with the error toast when the flows door fails to load', async () => {
	const { toasts } = await import('$lib/ui/feedback/toasts.svelte');
	toasts.items = [];
	render(DetailActions, { detail: torrentDetail() });
	await fireEvent.click(screen.getByTestId('detail-block'));
	await waitFor(() => expect(toasts.items.map((t) => t.kind)).toEqual(['error']), {
		timeout: 10_000
	});
	expect(screen.queryByRole('dialog')).toBeNull();
});
