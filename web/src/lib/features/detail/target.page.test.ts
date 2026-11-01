import { beforeEach, expect, it, vi } from 'vitest';
import { render, waitFor } from '@testing-library/svelte';
import { HASH, torrentDetail } from './fixtures';
import { clearDetailCaches } from './cache';
vi.mock('./visibility', () => ({ watchDetailVisibility: vi.fn() }));
vi.mock('./commands', () => ({ registerDetailCommands: () => Promise.resolve() }));
vi.mock('$app/paths', () => ({ resolve: (route: string) => route }));
const get = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { get }
}));

const { default: Page } = await import('../../../routes/t/[hash]/+page.svelte');
const { commandTarget } = await import('./target');

beforeEach(() => {
	get.mockReset();
	clearDetailCaches();
});

it('/t sets the key target (so `s`, `c`, `o` act) only once the torrent has loaded', async () => {
	let finish: (v: unknown) => void = () => {};
	get.mockImplementation((path: string) =>
		path === '/torrents/{hash}'
			? new Promise((resolve) => (finish = resolve))
			: Promise.resolve({ hits: [] })
	);
	render(Page, { data: { hash: HASH } });
	await waitFor(() => expect(get).toHaveBeenCalled(), { timeout: 10_000 });
	expect(commandTarget()).toBeNull();
	finish(torrentDetail({ hash: HASH }));
	await waitFor(() => expect(commandTarget()).toBe(HASH), { timeout: 10_000 });
});
