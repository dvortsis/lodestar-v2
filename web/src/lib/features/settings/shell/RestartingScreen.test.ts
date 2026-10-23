import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import { flow, startWaiting, stopWaiting } from './flow.svelte';
import RestartingScreen from './RestartingScreen.svelte';

const calls: string[] = [];
const state = { health: [] as boolean[], startedAt: 1_000, reachable: false };

beforeEach(() => {
	vi.useFakeTimers();
	calls.length = 0;
	state.health = [];
	state.startedAt = 1_000;
	state.reachable = false;
	vi.stubGlobal(
		'fetch',
		vi.fn(async (input: string | URL, init?: RequestInit) => {
			const url = String(input);
			calls.push(url);
			if (init?.mode === 'no-cors') {
				if (!state.reachable) throw new TypeError('Failed to fetch');
				return { type: 'opaque', ok: false, status: 0 } as unknown as Response;
			}
			if (url === '/healthz') {
				const up = state.health.shift() ?? true;
				if (!up) throw new TypeError('Failed to fetch');
				return { ok: true, status: 200 } as Response;
			}
			// GET /api/v1/system
			return {
				ok: true,
				status: 200,
				headers: new Headers(),
				json: async () => ({ started_at: state.startedAt })
			} as unknown as Response;
		})
	);
	stopWaiting();
});
afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

const phase = () => screen.getByTestId('settings-restarting').getAttribute('data-phase');

describe('the Restarting state', () => {
	it('polls /healthz with a growing wait and reloads when the new process answers', async () => {
		const reload = vi.fn();
		startWaiting({ estimate: 20, before: 1_000 });
		state.health = [false, false, true]; // down, down, up
		state.startedAt = 1_060;
		render(RestartingScreen, { reload });
		expect(screen.getByRole('alertdialog').textContent).toContain('Restarting…');
		await vi.advanceTimersByTimeAsync(0);
		expect(calls.filter((c) => c === '/healthz')).toHaveLength(1);
		await vi.advanceTimersByTimeAsync(999);
		expect(calls.filter((c) => c === '/healthz')).toHaveLength(1);
		await vi.advanceTimersByTimeAsync(1); // 1 s
		expect(calls.filter((c) => c === '/healthz')).toHaveLength(2);
		await vi.advanceTimersByTimeAsync(2_000); // then 2 s
		expect(calls.filter((c) => c === '/healthz')).toHaveLength(3);
		await vi.advanceTimersByTimeAsync(0);
		expect(reload).toHaveBeenCalledOnce();
	});

	it('does not reload while the old process still answers (same start time)', async () => {
		const reload = vi.fn();
		startWaiting({ estimate: 20, before: 1_000 });
		render(RestartingScreen, { reload });
		await vi.advanceTimersByTimeAsync(1_000 + 2_000);
		expect(reload).not.toHaveBeenCalled();
		state.startedAt = 1_090;
		await vi.advanceTimersByTimeAsync(4_000);
		expect(reload).toHaveBeenCalledOnce();
	});

	it('backs off 1, 2, 4, 8 s and stays at 8 s', async () => {
		startWaiting({ estimate: 20, before: 1_000 });
		state.health = Array(40).fill(false);
		render(RestartingScreen, { reload: vi.fn() });
		const polls = () => calls.filter((c) => c === '/healthz').length;
		await vi.advanceTimersByTimeAsync(0);
		const seen: number[] = [];
		for (const wait of [1_000, 2_000, 4_000, 8_000, 8_000]) {
			await vi.advanceTimersByTimeAsync(wait - 1);
			const before = polls();
			await vi.advanceTimersByTimeAsync(1);
			seen.push(polls() - before);
		}
		expect(seen).toEqual([1, 1, 1, 1, 1]);
	});

	it('says what to check after two minutes, and Try again polls anew', async () => {
		const reload = vi.fn();
		startWaiting({ estimate: 20, before: 1_000 });
		state.health = Array(60).fill(false);
		render(RestartingScreen, { reload });
		await vi.advanceTimersByTimeAsync(130_000);
		expect(phase()).toBe('timeout');
		expect(screen.getByRole('alertdialog').textContent).toContain("Lodestar hasn't come back yet");
		expect(screen.getByRole('alertdialog').textContent).toContain('lodestar healthcheck');
		state.health = [];
		state.startedAt = 1_120;
		await fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
		await vi.advanceTimersByTimeAsync(10);
		expect(reload).toHaveBeenCalledOnce();
	});

	it('moves to the new address (and its Network screen) once it answers, with no-cors', async () => {
		const assign = vi.fn();
		startWaiting({ to: 'http://192.168.1.5:9090/', estimate: 20, before: 1_000 });
		render(RestartingScreen, { assign, reload: vi.fn() });
		expect(screen.getByRole('alertdialog').textContent).toContain('Moving to the new address…');
		await vi.advanceTimersByTimeAsync(0);
		expect(calls[0]).toBe('http://192.168.1.5:9090/healthz');
		state.reachable = true;
		await vi.advanceTimersByTimeAsync(1_000);
		expect(assign).toHaveBeenCalledWith('http://192.168.1.5:9090/settings/network');
	});

	it('shows an address that is not http(s) as text and never asks it', async () => {
		const assign = vi.fn();
		startWaiting({ to: 'javascript:alert(1)', estimate: 20, before: 1_000 });
		render(RestartingScreen, { assign, reload: vi.fn() });
		await vi.advanceTimersByTimeAsync(5_000);
		expect(calls).toEqual([]);
		expect(assign).not.toHaveBeenCalled();
		const panel = screen.getByTestId('settings-restarting');
		expect(panel.textContent).toContain('javascript:alert(1)');
		expect(panel.textContent).toContain('Open it yourself');
		expect(panel.querySelector('a')).toBeNull();
		expect(flow.phase).toBe('waiting');
	});
});
