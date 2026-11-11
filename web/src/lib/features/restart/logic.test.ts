import { describe, expect, it } from 'vitest';
import { ApiError } from '$lib/api/client';
import {
	GIVE_UP_MS,
	addressPromptOf,
	backoffMs,
	failureOf,
	pendingOf,
	usableUrl,
	waitForServer,
	type System
} from './logic';

type Change = NonNullable<System['address_change']>;

function system(over: Partial<System> = {}): System {
	return {
		address_change: null,
		restart: { available: true, estimated_seconds: 4, hint: null, method: 'reexec' },
		restart_required: [],
		started_at: 100,
		...over
	} as System;
}

const change = (over: Partial<Change> = {}): Change => ({
	deadline: null,
	kind: 'http_bind',
	previous_url: 'http://192.168.1.5:8191',
	state: 'awaiting_restart',
	stored: true,
	url: 'http://192.168.1.5:9000',
	...over
});

describe('pendingOf', () => {
	it('is nothing while no saved value waits', () => {
		expect(pendingOf(undefined)).toBeNull();
		expect(pendingOf(system())).toBeNull();
	});
	it('offers the button when the server can restart itself', () => {
		expect(
			pendingOf(system({ restart_required: ['crawler.timeout_secs', 'network.dht_port'] }))
		).toEqual({
			fields: ['crawler.timeout_secs', 'network.dht_port'],
			canRestart: true,
			hint: null,
			estimateSeconds: 4,
			newUrl: null
		});
	});
	it('carries the hint and no button when it cannot', () => {
		const p = pendingOf(
			system({
				restart_required: ['network.dht_port'],
				restart: {
					available: false,
					estimated_seconds: 4,
					hint: 'Restart the container',
					method: 'manual'
				}
			})
		);
		expect(p).toMatchObject({ canRestart: false, hint: 'Restart the container' });
	});
	it('names the exact new URL when the HTTP address is pending', () => {
		const p = pendingOf(
			system({ restart_required: ['network.http_bind'], address_change: change() })
		);
		expect(p?.newUrl).toBe('http://192.168.1.5:9000');
	});
	it('does not name a URL for another change, or one already being confirmed', () => {
		expect(
			pendingOf(system({ restart_required: ['network.dht_port'], address_change: change() }))
				?.newUrl
		).toBeNull();
		expect(
			pendingOf(
				system({
					restart_required: ['network.http_bind'],
					address_change: change({ state: 'reverted' })
				})
			)?.newUrl
		).toBeNull();
	});
});

describe('addressPromptOf', () => {
	it('shows only while awaiting confirmation, with the seconds left', () => {
		const s = system({
			address_change: change({ state: 'awaiting_confirmation', deadline: 1_300 })
		});
		expect(addressPromptOf(s, 1_000_000)).toEqual({
			url: 'http://192.168.1.5:9000',
			previousUrl: 'http://192.168.1.5:8191',
			secondsLeft: 300
		});
		expect(addressPromptOf(s, 1_299_500)?.secondsLeft).toBe(1);
		expect(addressPromptOf(s, 2_000_000)?.secondsLeft).toBe(0);
	});
	it('is nothing for no change, a change awaiting restart, or a reverted one', () => {
		expect(addressPromptOf(system(), 0)).toBeNull();
		expect(addressPromptOf(system({ address_change: change() }), 0)).toBeNull();
		expect(
			addressPromptOf(system({ address_change: change({ state: 'reverted' }) }), 0)
		).toBeNull();
	});
});

describe('failureOf', () => {
	it('keeps the server message for the two 409s', () => {
		expect(failureOf(new ApiError(409, 'restart_unavailable', 'Restart the container'))).toEqual({
			kind: 'unavailable',
			message: 'Restart the container'
		});
		expect(failureOf(new ApiError(409, 'restart_failed', 'Close it and start it again'))).toEqual({
			kind: 'failed',
			message: 'Close it and start it again'
		});
	});
	it('reads anything else as a plain error', () => {
		expect(failureOf(new ApiError(0, 'network', 'offline')).kind).toBe('error');
		expect(failureOf('boom')).toEqual({ kind: 'error', message: 'boom' });
	});
});

describe('backoffMs', () => {
	it('grows from one second and stops at five', () => {
		expect([0, 1, 2, 3, 10].map(backoffMs)).toEqual([1_000, 1_500, 2_250, 3_375, 5_000]);
	});
});

/** A fake clock: sleeping moves time on and costs nothing. */
function clock() {
	let t = 0;
	return {
		now: () => t,
		sleep: (ms: number) => {
			t += ms;
			return Promise.resolve();
		}
	};
}

describe('waitForServer', () => {
	it('is up once it answers after having been down', async () => {
		const c = clock();
		const answers = [true, false, false, true];
		const result = await waitForServer({ ...c, probe: async () => answers.shift() ?? true });
		expect(result).toBe('up');
	});
	it('does not take the old process for the new one', async () => {
		const c = clock();
		let asked = 0;
		const result = await waitForServer({
			...c,
			probe: async () => true,
			isNewRun: async () => ++asked >= 3
		});
		expect(result).toBe('up');
		expect(asked).toBe(3);
	});
	it('gives up after two minutes', async () => {
		const c = clock();
		const result = await waitForServer({ ...c, probe: async () => false });
		expect(result).toBe('timeout');
		expect(c.now()).toBeGreaterThanOrEqual(GIVE_UP_MS);
		expect(c.now()).toBeLessThan(GIVE_UP_MS + 10_000);
	});
	it('stops when aborted', async () => {
		const c = clock();
		const ctl = new AbortController();
		ctl.abort();
		expect(await waitForServer({ ...c, probe: async () => true, signal: ctl.signal })).toBe(
			'aborted'
		);
	});
});

describe('usableUrl', () => {
	it("puts the host the browser used in place of the server's placeholder", () => {
		expect(usableUrl("http://<this host's address>:9000/", 'nas.local')).toBe(
			'http://nas.local:9000/'
		);
		expect(usableUrl('http://0.0.0.0:9000/', '192.168.1.5')).toBe('http://192.168.1.5:9000/');
		expect(usableUrl("http://<this host's address>:9000/", '::1')).toBe('http://[::1]:9000/');
	});
	it('leaves a real address alone', () => {
		expect(usableUrl('http://192.168.1.5:9000/', 'nas.local')).toBe('http://192.168.1.5:9000/');
	});
	it('is used for the dialog and the prompt', () => {
		const s = system({
			restart_required: ['network.http_bind'],
			address_change: change({ url: "http://<this host's address>:9000/" })
		});
		expect(pendingOf(s, 'nas.local')?.newUrl).toBe('http://nas.local:9000/');
		const w = system({
			address_change: change({
				state: 'awaiting_confirmation',
				deadline: 10,
				url: 'http://0.0.0.0:9000/',
				previous_url: "http://<this host's address>:8090/"
			})
		});
		expect(addressPromptOf(w, 0, 'nas.local')).toMatchObject({
			url: 'http://nas.local:9000/',
			previousUrl: 'http://nas.local:8090/'
		});
	});
});

describe('waitForServer with a new address', () => {
	it('does not take an answer in the first seconds for the new server', async () => {
		const c = clock();
		const result = await waitForServer({ ...c, probe: async () => true, settleMs: 4_000 });
		expect(result).toBe('up');
		expect(c.now()).toBeGreaterThanOrEqual(4_000);
	});
	it('takes an answer at once once it was seen down', async () => {
		const c = clock();
		const answers = [false, true];
		const result = await waitForServer({
			...c,
			probe: async () => answers.shift() ?? true,
			settleMs: 60_000
		});
		expect(result).toBe('up');
		expect(c.now()).toBeLessThan(10_000);
	});
});
