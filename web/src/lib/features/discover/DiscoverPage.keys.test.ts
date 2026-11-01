import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { cleanup, render, waitFor } from '@testing-library/svelte';
import { stubLayout } from '$lib/ui/lists/testing/layout';
import { toast, toasts } from '$lib/ui/feedback/toasts.svelte';
import { m } from '$lib/paraglide/messages.js';

const get = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (orig) => ({
	...(await orig<typeof import('$lib/api/client')>()),
	api: { get }
}));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('./commands', () => {
	throw new Error('chunk failed to load');
});

import DiscoverPage from './DiscoverPage.svelte';
import { response } from './testing';

beforeAll(() => stubLayout(720));
afterEach(() => {
	cleanup();
	toast.clear();
});

describe('Discover key registry import', () => {
	it('tells the user when j/k could not load', async () => {
		get.mockImplementation(async (path: string) =>
			path === '/settings/{section}' ? { fields: { show_adult: { value: false } } } : response()
		);
		render(DiscoverPage, { search: new URLSearchParams() });
		await waitFor(
			() =>
				expect(toasts.items.map((t) => [t.kind, t.text])).toContainEqual([
					'error',
					m.discover_error_load()
				]),
			{ timeout: 10_000 }
		);
	});
});
