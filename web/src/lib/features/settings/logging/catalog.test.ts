// The catalog reloads when the tab is visible again and on the 30 s tick, never stacks requests,
// and a failed reload keeps the last known state.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { waitFor } from '@testing-library/svelte';
import { LoggingCatalog } from './catalog.svelte';

vi.setConfig({ testTimeout: 30_000 });
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });
const catalogAt = (now: number) => ({ now, events: [], kinds: [], presets: {} });

let loads: number;
let fail: boolean;
let release: (() => void) | undefined;
let hold: boolean;

beforeEach(() => {
	loads = 0;
	fail = false;
	hold = false;
	release = undefined;
	vi.stubGlobal(
		'fetch',
		vi.fn(async (url: string, init: RequestInit = {}) => {
			const path = new URL(url, 'http://x').pathname.replace('/api/v1', '');
			const reply = (status: number, b: unknown) =>
				new Response(JSON.stringify(b), {
					status,
					headers: { 'Content-Type': 'application/json' }
				});
			if (path === '/system') return reply(200, { deployment: { kind: 'docker' } });
			loads++;
			if (hold)
				await new Promise<void>((resolve, reject) => {
					release = resolve;
					init.signal?.addEventListener('abort', () => reject(init.signal?.reason));
				});
			return fail
				? reply(500, { error: { code: 'internal', message: 'down' } })
				: reply(200, catalogAt(1_000 + loads));
		})
	);
});
afterEach(() => {
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

function visible(state: 'visible' | 'hidden') {
	Object.defineProperty(document, 'visibilityState', { value: state, configurable: true });
	document.dispatchEvent(new Event('visibilitychange'));
}

describe('LoggingCatalog refresh', () => {
	it('reloads when the tab becomes visible, not when it hides', async () => {
		const c = new LoggingCatalog();
		const stop = c.start();
		await until(() => expect(c.catalog?.now).toBe(1_001));
		visible('hidden');
		expect(loads).toBe(1);
		visible('visible');
		await until(() => expect(c.catalog?.now).toBe(1_002));
		stop();
		visible('visible');
		expect(loads).toBe(2);
	});

	it('reloads on the tick', async () => {
		const c = new LoggingCatalog();
		await c.load();
		c.tick();
		await until(() => expect(loads).toBe(2));
	});

	it('never stacks requests', async () => {
		const c = new LoggingCatalog();
		const stop = c.start();
		await until(() => expect(c.catalog?.now).toBe(1_001));
		hold = true;
		c.tick();
		await until(() => expect(loads).toBe(2));
		c.tick();
		visible('visible');
		expect(loads).toBe(2);
		hold = false;
		release?.();
		await until(() => expect(c.catalog?.now).toBe(1_002));
		c.tick();
		await until(() => expect(loads).toBe(3));
		stop();
	});

	it('reloads again when a request never settles', async () => {
		const c = new LoggingCatalog();
		await c.load();
		hold = true;
		c.tick();
		await until(() => expect(loads).toBe(2));
		const now = Date.now();
		const spy = vi.spyOn(Date, 'now').mockReturnValue(now + 31_000);
		c.tick();
		await until(() => expect(loads).toBe(3));
		spy.mockRestore();
		hold = false;
		release?.();
	});

	it('counts a timed-out reload as a failed read and keeps the last state', async () => {
		const c = new LoggingCatalog();
		await c.load();
		const before = c.catalog;
		hold = true;
		await c.load(AbortSignal.timeout(50));
		expect(c.error).toBeDefined();
		expect(c.catalog).toBe(before);
		hold = false;
		await c.load();
		expect(c.error).toBeUndefined();
	});

	it('stays silent when a newer load supersedes an aborted one', async () => {
		const c = new LoggingCatalog();
		await c.load();
		hold = true;
		const ctl = new AbortController();
		const slow = c.load(ctl.signal);
		await until(() => expect(loads).toBe(2));
		hold = false;
		const fresh = c.load();
		ctl.abort();
		await Promise.all([slow, fresh]);
		expect(c.error).toBeUndefined();
	});

	it('keeps the last state when a reload fails', async () => {
		const c = new LoggingCatalog();
		await c.load();
		const before = c.catalog;
		fail = true;
		c.tick();
		await until(() => expect(c.error).toBeDefined());
		expect(c.catalog).toBe(before);
		fail = false;
		c.tick();
		await until(() => expect(c.error).toBeUndefined());
		expect(c.catalog?.now).toBe(1_000 + loads);
	});
});
