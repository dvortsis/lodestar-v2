import { beforeEach, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/svelte';
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
const target = await import('./target');
const sendTarget = () => (target.loadedHash === target.drawerHash ? target.drawerHash : null);
const { ApiError } = await import('$lib/api/client');

beforeEach(() => {
	get.mockReset();
	clearDetailCaches();
});

const pending = () => {
	let finish: (v: unknown) => void = () => {};
	let fail: (e: unknown) => void = () => {};
	get.mockImplementation((path: string) =>
		path === '/torrents/{hash}'
			? new Promise((resolve, reject) => ((finish = resolve), (fail = reject)))
			: Promise.resolve({ hits: [] })
	);
	return { finish: (v: unknown) => finish(v), fail: (e: unknown) => fail(e) };
};

it('/t: `s` waits for the torrent to load, and stops when the page goes', async () => {
	const load = pending();
	const { unmount } = render(Page, { data: { hash: HASH } });
	await waitFor(() => expect(get).toHaveBeenCalled(), { timeout: 10_000 });
	expect(sendTarget()).toBeNull();
	load.finish(torrentDetail({ hash: HASH }));
	await waitFor(() => expect(sendTarget()).toBe(HASH), { timeout: 10_000 });
	// Let the lazy Contents chunk (composition, file tree) finish importing before the environment
	// is torn down; a stray import after teardown fails the whole run.
	await waitFor(
		() => {
			expect(screen.getByTestId('detail-composition')).toBeTruthy();
			expect(screen.getByTestId('detail-file-tree')).toBeTruthy();
		},
		{ timeout: 10_000 }
	);
	unmount();
	expect(sendTarget()).toBeNull();
});

it('/t: a torrent that fails to load never becomes the target', async () => {
	const load = pending();
	render(Page, { data: { hash: HASH } });
	await waitFor(() => expect(get).toHaveBeenCalled(), { timeout: 10_000 });
	load.fail(new ApiError(404, 'torrent_not_found', 'x'));
	await waitFor(() => expect(get).toHaveBeenCalledTimes(1), { timeout: 10_000 });
	expect(sendTarget()).toBeNull();
});
