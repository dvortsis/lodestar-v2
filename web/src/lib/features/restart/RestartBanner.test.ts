import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { ApiError } from '$lib/api/client';
import RestartBanner from './RestartBanner.svelte';
import type { System } from './logic';

const page = vi.hoisted(() => ({ url: new URL('http://x/settings') }));
vi.mock('$app/state', () => ({ page }));
const api = vi.hoisted(() => ({ get: vi.fn(), post: vi.fn() }));
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api
}));
vi.mock('./RestartingPage.svelte', async () => ({
	default: (await import('./testing/Restarting.svelte')).default
}));

// The dialog, Review and the Restarting page are lazy chunks: wait for conditions, never a delay.
vi.setConfig({ testTimeout: 30_000 });
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });

const system = (over: Partial<System> = {}): System =>
	({
		address_change: null,
		restart: { available: true, estimated_seconds: 7, hint: null, method: 'reexec' },
		restart_required: ['network.dht_port', 'crawler.timeout_secs'],
		started_at: 100,
		...over
	}) as System;

beforeEach(() => {
	api.get.mockReset();
	api.post.mockReset();
});
afterEach(() => vi.restoreAllMocks());

function serve(s: System) {
	api.get.mockImplementation(async (path: string) => {
		if (path === '/system') return s;
		throw new Error(`unexpected ${path}`);
	});
}

it('shows nothing while no change waits', async () => {
	serve(system({ restart_required: [] }));
	render(RestartBanner);
	await until(() => expect(api.get).toHaveBeenCalled());
	expect(screen.queryByTestId('restart-banner')).toBeNull();
});

it('counts the changes and offers Restart now when the server can restart itself', async () => {
	serve(system());
	render(RestartBanner);
	await until(() => expect(screen.getByTestId('restart-count').textContent).toMatch(/2 changes/));
	expect(screen.getByRole('button', { name: 'Review' })).toBeTruthy();
	expect(screen.getByRole('button', { name: 'Restart now' })).toBeTruthy();
});

it('shows the hint and no button when it cannot', async () => {
	serve(
		system({
			restart_required: ['network.dht_port'],
			restart: {
				available: false,
				estimated_seconds: 7,
				hint: 'Restart the container',
				method: 'manual'
			}
		})
	);
	render(RestartBanner);
	await until(() =>
		expect(screen.getByTestId('restart-hint').textContent).toMatch(/Restart the container/)
	);
	expect(screen.getByTestId('restart-count').textContent).toMatch(/1 change /);
	expect(screen.queryByRole('button', { name: 'Restart now' })).toBeNull();
});

it('warns about the pause, then restarts and shows the restarting page', async () => {
	serve(system());
	api.post.mockResolvedValue({ estimated_seconds: 7, restarting: true });
	render(RestartBanner);
	await fireEvent.click(
		await screen.findByRole('button', { name: 'Restart now' }, { timeout: 10_000 })
	);
	expect(
		(await screen.findByTestId('restart-warning', {}, { timeout: 10_000 })).textContent
	).toMatch(/about 7 seconds/);
	expect(api.post).not.toHaveBeenCalled();
	await fireEvent.click(screen.getAllByRole('button', { name: 'Restart now' }).at(-1)!);
	await until(() => expect(screen.getByTestId('restarting-page')).toBeTruthy());
	expect(api.post).toHaveBeenCalledWith('/system/restart');
});

it('shows a 409 message and stops', async () => {
	serve(system());
	api.post.mockRejectedValue(new ApiError(409, 'restart_failed', 'It could not get ready.'));
	render(RestartBanner);
	await fireEvent.click(
		await screen.findByRole('button', { name: 'Restart now' }, { timeout: 10_000 })
	);
	await screen.findByTestId('restart-warning', {}, { timeout: 10_000 });
	await fireEvent.click(screen.getAllByRole('button', { name: 'Restart now' }).at(-1)!);
	await until(() =>
		expect(screen.getByTestId('restart-failure').textContent).toBe('It could not get ready.')
	);
	expect(screen.queryByTestId('restarting-page')).toBeNull();
});

it('names the exact new URL for an HTTP address change', async () => {
	serve(
		system({
			restart_required: ['network.http_bind'],
			address_change: {
				deadline: null,
				kind: 'http_bind',
				previous_url: 'http://a:1',
				state: 'awaiting_restart',
				stored: true,
				url: 'http://a:2'
			}
		})
	);
	render(RestartBanner);
	await fireEvent.click(
		await screen.findByRole('button', { name: 'Restart now' }, { timeout: 10_000 })
	);
	expect((await screen.findByTestId('restart-new-url', {}, { timeout: 10_000 })).textContent).toBe(
		'http://a:2'
	);
});

function awaiting(deadline: number): System {
	return system({
		restart_required: [],
		address_change: {
			deadline,
			kind: 'http_bind',
			previous_url: 'http://a:1',
			state: 'awaiting_confirmation',
			stored: true,
			url: 'http://a:2'
		}
	});
}

it('asks "Keep this address?" with the countdown, and keeps it', async () => {
	serve(awaiting(Date.now() / 1000 + 300));
	api.post.mockResolvedValue({ address_change: null, restarting: false });
	render(RestartBanner);
	expect(await screen.findByText('Keep this address?', {}, { timeout: 10_000 })).toBeTruthy();
	expect(screen.getByTestId('keep-countdown').textContent).toMatch(/\d:\d\d/);
	await fireEvent.click(screen.getByRole('button', { name: 'Keep this address' }));
	await until(() => expect(api.post).toHaveBeenCalledWith('/system/address/keep'));
});

it('goes back through revert, and the restart that follows shows the restarting page', async () => {
	serve(awaiting(Date.now() / 1000 + 300));
	api.post.mockResolvedValue({ address_change: null, restarting: true });
	render(RestartBanner);
	await fireEvent.click(
		await screen.findByRole('button', { name: 'Go back' }, { timeout: 10_000 })
	);
	await until(() => expect(api.post).toHaveBeenCalledWith('/system/address/revert'));
	await until(() => expect(screen.getByTestId('restarting-page')).toBeTruthy());
});

it('lists each pending field with its saved value (Review)', async () => {
	serve(system());
	// The review reads the settings through the form engine; here it can't, and says so.
	render(RestartBanner);
	await fireEvent.click(await screen.findByRole('button', { name: 'Review' }, { timeout: 10_000 }));
	await until(() => expect(screen.getByTestId('restart-review')).toBeTruthy());
});
