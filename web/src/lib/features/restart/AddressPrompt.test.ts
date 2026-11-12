import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/svelte';
import AddressPrompt from './AddressPrompt.svelte';
import { restart } from './restart.svelte';
import type { System } from './logic';

const api = vi.hoisted(() => ({ get: vi.fn(), post: vi.fn() }));
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api
}));

// Waits for conditions with a deadline of 15 s; the test's own timeout must not cut them short.
vi.setConfig({ testTimeout: 30_000 });
const until = (check: () => void) => waitFor(check, { timeout: 15_000 });

const system = (deadlineIn: number, state = 'awaiting_confirmation', available = true): System =>
	({
		address_change: {
			deadline: Math.floor(Date.now() / 1000) + deadlineIn,
			kind: 'http_bind',
			previous_url: 'http://a:1/',
			state,
			stored: true,
			url: 'http://a:2/'
		},
		restart: { available, estimated_seconds: 4, hint: null, method: 'reexec' },
		restart_required: [],
		started_at: 1
	}) as unknown as System;

beforeEach(() => {
	vi.useFakeTimers({ shouldAdvanceTime: true });
	api.get.mockReset();
	api.post.mockReset();
	restart.begin(null);
});
afterEach(() => vi.useRealTimers());

it('is not a live region, and nothing live changes on a one-second tick', async () => {
	const { container } = render(AddressPrompt, { props: { system: system(300) } });
	const live = '[role="status"], [role="alert"], [aria-live]';
	expect(container.querySelectorAll(live)).toHaveLength(0);
	const before = screen.getByTestId('keep-countdown').textContent;
	await vi.advanceTimersByTimeAsync(3_000);
	expect(screen.getByTestId('keep-countdown').textContent).not.toBe(before);
	// The visible digits are hidden from assistive tech; what it reads does not change.
	const spoken = screen.getByTestId('keep-countdown').querySelector('.sr-only')!.textContent;
	await vi.advanceTimersByTimeAsync(3_000);
	expect(screen.getByTestId('keep-countdown').querySelector('.sr-only')!.textContent).toBe(spoken);
	expect(container.querySelectorAll(live)).toHaveLength(0);
	expect(screen.getByTestId('keep-countdown').querySelector('[aria-hidden="true"]')).not.toBeNull();
});

it('at 0:00 asks the server, and stays while the server still waits (a browser clock ahead)', async () => {
	api.get.mockResolvedValue(system(300));
	render(AddressPrompt, { props: { system: system(-60) } });
	await until(() => expect(api.get).toHaveBeenCalledWith('/system', expect.anything()));
	await vi.advanceTimersByTimeAsync(1_000);
	expect(restart.waiting).toBeNull();
	expect(screen.getByRole('button', { name: 'Keep this address' })).toBeTruthy();
});

it('at 0:00 goes back once the server says it was reverted', async () => {
	api.get.mockResolvedValue(system(-60, 'reverted'));
	render(AddressPrompt, { props: { system: system(-60) } });
	await until(() => expect(restart.waiting).not.toBeNull());
	expect(restart.waiting!.target).toBe('http://a:1/');
});

it('at 0:00 goes back when the server stops answering', async () => {
	api.get.mockRejectedValue(new Error('down'));
	render(AddressPrompt, { props: { system: system(-60) } });
	await until(() => expect(restart.waiting).not.toBeNull());
});

it('at 0:00 does not start a restart that will not happen when the server cannot restart itself', async () => {
	api.get.mockResolvedValue(system(-60, 'reverted', false));
	render(AddressPrompt, { props: { system: system(-60) } });
	await until(() => expect(api.get).toHaveBeenCalled());
	await vi.advanceTimersByTimeAsync(2_000);
	expect(restart.waiting).toBeNull();
});
