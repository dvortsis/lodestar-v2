import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, waitFor } from '@testing-library/svelte';
import { toasts } from '$lib/ui/feedback/toasts.svelte';
import { stubLayout } from '$lib/ui/lists/testing/layout';

const get = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (orig) => ({
	...(await orig<typeof import('$lib/api/client')>()),
	api: { get }
}));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
// The after-mount import of the command chunk fails, as a dropped chunk request does.
vi.mock('./commands', () => {
	throw new Error('chunk failed to load');
});

import DiscoverPage from './DiscoverPage.svelte';

afterEach(() => {
	cleanup();
	toasts.items = [];
});

describe('Discover when the key registry fails to load', () => {
	it('says shortcuts are unavailable instead of failing silently', async () => {
		stubLayout(720);
		get.mockResolvedValue({ data: undefined, error: undefined });
		render(DiscoverPage, { search: new URLSearchParams() });
		await waitFor(
			() => expect(toasts.items.map((t) => t.text).join()).toMatch(/Keyboard shortcuts/),
			{
				timeout: 10_000
			}
		);
		expect(toasts.items).toHaveLength(1);
		expect(toasts.items[0]?.kind).toBe('info');
	});
});
