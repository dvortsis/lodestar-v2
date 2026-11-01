import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('./actions', async (orig) => ({
	...(await orig<typeof import('./actions')>()),
	copyMagnets: vi.fn(async () => ({ ok: true, copied: false, text: 'magnet:?xt=old', count: 2 }))
}));

import BulkBar from './BulkBar.svelte';
import { selection } from './selection.svelte';

afterEach(() => {
	cleanup();
	selection.mode = false;
});

describe('BulkBar manual copy', () => {
	it('closes the stale textarea when the selection changes and returns focus to Copy', async () => {
		selection.mode = true;
		selection.toggle('a'.repeat(40), true);
		selection.toggle('b'.repeat(40), true);
		render(BulkBar);
		await fireEvent.click(screen.getByTestId('bulk-copy'));
		const box = await screen.findByTestId('bulk-manual-text');
		await waitFor(() => expect(document.activeElement).toBe(box));
		selection.toggle('c'.repeat(40), true);
		await waitFor(() => expect(screen.queryByTestId('bulk-manual')).toBeNull());
		expect(document.activeElement).toBe(screen.getByTestId('bulk-copy'));
	});
});
