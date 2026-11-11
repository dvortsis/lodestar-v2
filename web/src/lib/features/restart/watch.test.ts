import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { waitFor } from '@testing-library/svelte';
import { restart } from './restart.svelte';
import { watchRestart } from './watch.svelte';

const page = vi.hoisted(() => ({ url: new URL('http://x/settings') }));
vi.mock('$app/state', () => ({ page }));
const api = vi.hoisted(() => ({ get: vi.fn(), post: vi.fn() }));
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api
}));

const until = (check: () => void) => waitFor(check, { timeout: 15_000 });
let slot: HTMLElement;

beforeEach(() => {
	restart.begin(null);
	slot = document.body.appendChild(document.createElement('div'));
	slot.id = 'restart-slot';
});
afterEach(() => slot.remove());

const system = (restart_required: string[]) => ({
	address_change: null,
	restart: { available: true, estimated_seconds: 4, hint: null, method: 'reexec' },
	restart_required,
	started_at: 1
});

it('tells the pill, and mounts the banner into the Settings shell once a change waits', async () => {
	api.get.mockResolvedValue(system(['network.dht_port']));
	const set = vi.fn();
	const stop = watchRestart(set);
	await until(() => expect(set).toHaveBeenCalledWith(true));
	await until(() => expect(slot.querySelector('[data-testid="restart-banner"]')).not.toBeNull());
	stop();
	expect(slot.querySelector('[data-testid="restart-banner"]')).toBeNull();
});

it('shows nothing while no change waits', async () => {
	api.get.mockResolvedValue(system([]));
	const set = vi.fn();
	const stop = watchRestart(set);
	await until(() => expect(api.get).toHaveBeenCalled());
	await until(() => expect(set).toHaveBeenCalledWith(false));
	expect(slot.querySelector('[data-testid="restart-banner"]')).toBeNull();
	stop();
});
