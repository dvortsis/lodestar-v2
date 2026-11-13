import { beforeEach, expect, it, vi } from 'vitest';
import { restart } from './restart.svelte';
import type { System } from './logic';

const api = vi.hoisted(() => ({ get: vi.fn() }));
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api
}));

const awaiting = (): System =>
	({
		address_change: {
			deadline: 1,
			kind: 'http_bind',
			previous_url: 'http://a:1/',
			state: 'awaiting_confirmation',
			stored: true,
			url: 'http://a:2/'
		},
		restart: { available: true, estimated_seconds: 4, hint: null, method: 'reexec' },
		restart_required: [],
		started_at: 1
	}) as unknown as System;

beforeEach(() => {
	api.get.mockReset();
	restart.begin(null);
});

it('a read that started before Keep cannot bring the prompt back when it lands after it', async () => {
	api.get.mockResolvedValueOnce(awaiting());
	await restart.refresh();
	expect(restart.system?.address_change?.state).toBe('awaiting_confirmation');
	let release!: (s: System) => void;
	api.get.mockReturnValueOnce(new Promise<System>((done) => (release = done)));
	const stale = restart.refresh();
	// Keep answers: nothing awaits any more.
	restart.setAddressChange(null);
	release(awaiting());
	expect(await stale).toBe(true);
	expect(restart.system?.address_change).toBeNull();
});

it('a refresh after Keep asks again instead of joining the read from before it', async () => {
	api.get.mockResolvedValueOnce(awaiting());
	await restart.refresh();
	let release!: (s: System) => void;
	api.get.mockReturnValueOnce(new Promise<System>((done) => (release = done)));
	const stale = restart.refresh();
	restart.setAddressChange(null);
	api.get.mockResolvedValueOnce({ ...awaiting(), address_change: null });
	await restart.refresh();
	expect(api.get).toHaveBeenCalledTimes(3);
	release(awaiting());
	await stale;
	expect(restart.system?.address_change).toBeNull();
});

it('reads pause while a restart lasts', async () => {
	api.get.mockResolvedValueOnce(awaiting());
	await restart.refresh();
	restart.begin({ target: null, startedAt: 1, estimateSeconds: 4 });
	await restart.refresh();
	expect(api.get).toHaveBeenCalledTimes(1);
});

it('a read in flight when a restart starts is dropped even after the restart state is cleared', async () => {
	api.get.mockResolvedValueOnce(awaiting());
	await restart.refresh();
	const before = restart.system;
	let release!: (s: System) => void;
	api.get.mockReturnValueOnce(new Promise<System>((done) => (release = done)));
	const stale = restart.refresh();
	// A restart starts, and is later cleared (a page that gave up, a test): `waiting` alone no
	// longer says that this read is older than the restart.
	restart.begin({ target: null, startedAt: 1, estimateSeconds: 4 });
	restart.begin(null);
	release({ ...awaiting(), address_change: null });
	expect(await stale).toBe(true);
	expect(restart.system).toBe(before);
});

it('a save drops a read from before it and asks again, so the old restart_required cannot win', async () => {
	const none = { ...awaiting(), address_change: null, restart_required: [] } as System;
	api.get.mockResolvedValueOnce(none);
	await restart.refresh();
	let release!: (s: System) => void;
	api.get.mockReturnValueOnce(new Promise<System>((done) => (release = done)));
	const stale = restart.refresh();
	// The save answers while that read is still out; the fresh read sees the saved field.
	api.get.mockResolvedValueOnce({ ...none, restart_required: ['network.dht_port'] });
	expect(await restart.saved()).toBe(true);
	expect(api.get).toHaveBeenCalledTimes(3);
	expect(restart.pending).not.toBeNull();
	release(none);
	expect(await stale).toBe(true);
	expect(restart.system?.restart_required).toEqual(['network.dht_port']);
});

it('a save while a restart lasts does not ask', async () => {
	restart.begin({ target: null, startedAt: 1, estimateSeconds: 4 });
	expect(await restart.saved()).toBe(true);
	expect(api.get).not.toHaveBeenCalled();
});
