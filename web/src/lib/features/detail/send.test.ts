import { beforeEach, describe, expect, it, vi } from 'vitest';

const register = vi.hoisted(() => vi.fn());
vi.mock('$lib/keys', () => ({ keys: { register } }));
const get = vi.fn();
const post = vi.fn();
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { get, post }
}));

const { ApiError } = await import('$lib/api/client');
let mod: typeof import('./send');
let toasts: typeof import('$lib/ui/feedback/toasts.svelte').toasts;
const HASH = 'a'.repeat(40);
const section = (clients: unknown, def = '') => ({
	fields: { clients: { value: clients }, default_client: { value: def } }
});
const texts = () => toasts.items.map((t) => [t.kind, t.text]);

beforeEach(async () => {
	vi.resetModules();
	get.mockReset();
	post.mockReset();
	register.mockReset();
	({ toasts } = await import('$lib/ui/feedback/toasts.svelte'));
	toasts.items = [];
	mod = await import('./send');
});

describe('clientName', () => {
	it('is the default client’s name, else the first’s, and is read once', async () => {
		get.mockResolvedValue(
			section(
				[
					{ id: 'a', name: 'Home' },
					{ id: 'b', name: 'Seedbox' }
				],
				'b'
			)
		);
		expect(await mod.clientName()).toBe('Seedbox');
		expect(await mod.clientName()).toBe('Seedbox');
		expect(get).toHaveBeenCalledTimes(1);
		await vi.waitFor(() => expect(register).toHaveBeenCalledTimes(1), { timeout: 10_000 });
	});

	it('is null with no clients, and a failed read is asked again', async () => {
		get.mockRejectedValueOnce(new Error('offline'));
		expect(await mod.clientName()).toBeNull();
		get.mockResolvedValue(section([]));
		expect(await mod.clientName()).toBeNull();
		expect(get).toHaveBeenCalledTimes(2);
		await vi.dynamicImportSettled();
		expect(register).not.toHaveBeenCalled();
	});
});

describe('sendHash', () => {
	beforeEach(() => get.mockResolvedValue(section([{ id: 'q', name: 'qBittorrent' }])));

	it('posts just the hash (the default client) and toasts the success', async () => {
		post.mockResolvedValue({ client: 'q', sent: 1, results: [{ hash: HASH, outcome: 'sent' }] });
		await mod.sendHash(HASH);
		expect(post).toHaveBeenCalledExactlyOnceWith('/send', { body: { hashes: [HASH] } });
		expect(texts()).toEqual([['success', 'Sent to qBittorrent']]);
	});

	it('says a duplicate is already there', async () => {
		post.mockResolvedValue({
			client: 'q',
			sent: 1,
			results: [{ hash: HASH, outcome: 'duplicate' }]
		});
		await mod.sendHash(HASH);
		expect(texts()).toEqual([['success', 'Already in qBittorrent']]);
	});

	it('shows a refusal’s sentence, or a plain failure without one', async () => {
		post.mockResolvedValueOnce({
			client: 'q',
			sent: 0,
			results: [{ hash: HASH, outcome: 'refused', code: 'private', message: 'It is private.' }]
		});
		await mod.sendHash(HASH);
		post.mockResolvedValueOnce({
			client: 'q',
			sent: 0,
			results: [{ hash: HASH, outcome: 'failed', code: null, message: null }]
		});
		await mod.sendHash(HASH);
		expect(texts()).toEqual([
			['error', 'qBittorrent didn’t take it. It is private.'],
			['error', 'Couldn’t send to qBittorrent. Try again.']
		]);
	});

	it('names no_client, and a network error, in plain words, with no retry', async () => {
		post.mockRejectedValueOnce(new ApiError(404, 'no_client', 'none'));
		await mod.sendHash(HASH);
		post.mockRejectedValueOnce(new TypeError('network'));
		await mod.sendHash(HASH);
		expect(texts()).toEqual([
			['error', 'No download client is set up. Add one in Settings, under Integrations.'],
			['error', 'Couldn’t send to qBittorrent. Try again.']
		]);
		expect(post).toHaveBeenCalledTimes(2);
	});
});

describe('the s key', () => {
	it('is registered once a client is known, active with a target, and sends it', async () => {
		get.mockResolvedValue(section([{ id: 'q', name: 'qBittorrent' }]));
		post.mockResolvedValue({ client: 'q', sent: 1, results: [{ hash: HASH, outcome: 'sent' }] });
		const { setCommandTarget } = await import('./target');
		await mod.clientName();
		await vi.waitFor(() => expect(register).toHaveBeenCalledTimes(1), { timeout: 10_000 });
		const command = register.mock.calls[0]![0];
		expect([command.id, command.keys]).toEqual(['detail.send', ['s']]);
		expect(command.when()).toBe(false);
		setCommandTarget(HASH);
		expect(command.when()).toBe(true);
		command.run();
		await vi.waitFor(() => expect(post).toHaveBeenCalledTimes(1), { timeout: 10_000 });
		setCommandTarget(null);
	});

	it('waits for the open drawer’s torrent to load, but not for a focused row', async () => {
		const { setCommandTarget, setDrawerTarget, setLoadedHash } = await import('./target');
		get.mockResolvedValue(section([{ id: 'q', name: 'qBittorrent' }]));
		await mod.clientName();
		await vi.waitFor(() => expect(register).toHaveBeenCalledTimes(1), { timeout: 10_000 });
		const when = register.mock.calls[0]![0].when as () => boolean;
		setCommandTarget(HASH);
		expect(when()).toBe(true); // a row, no drawer
		setDrawerTarget(HASH);
		expect(when()).toBe(false); // the drawer’s torrent is still loading
		setLoadedHash(HASH);
		expect(when()).toBe(true);
		setDrawerTarget(null);
		setLoadedHash(null);
		setCommandTarget(null);
	});
});

describe('freshness', () => {
	it('asks again when the drawer asks fresh, so a renamed client shows its new name', async () => {
		get.mockResolvedValueOnce(section([{ id: 'a', name: 'Home' }]));
		expect(await mod.clientName()).toBe('Home');
		get.mockResolvedValueOnce(section([{ id: 'a', name: 'Seedbox' }]));
		expect(await mod.clientName(true)).toBe('Seedbox');
		expect(get).toHaveBeenCalledTimes(2);
	});
});
