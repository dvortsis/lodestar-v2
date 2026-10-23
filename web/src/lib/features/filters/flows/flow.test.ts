// The flow over the wire: a scripted fetch, fake timers, and assertions on every request body.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { onLoginRequired } from '$lib/api/client';
import { addRule, coreSwitch, floorRemoval } from './changes';
import { createChangeFlow } from './flow.svelte';
import { mockFetch } from './testing/fetch';
import { applied, finished, running, strengthening, weakening } from './testing/fixtures';

const PW = 'correct horse battery';
const off = coreSwitch('CS-4a', false);

let net: ReturnType<typeof mockFetch>;
beforeEach(() => {
	vi.useFakeTimers();
	net = mockFetch();
	net.on('POST /filters/preview/jobs', { status: 202, body: { job: 7 } });
	net.on('POST /filters/preview/jobs/{id}/cancel', { body: { ok: true } });
});
afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
	onLoginRequired(undefined);
});

const settle = () => vi.advanceTimersByTimeAsync(0);
/** Let the job start, then run polls for `ms`. */
const wait = async (ms: number) => {
	await settle();
	await vi.advanceTimersByTimeAsync(ms);
};

function expectNoPasswordInPreviews() {
	for (const r of net.to('/filters/preview')) {
		expect(r.body ?? '').not.toContain('password');
		expect(r.body ?? '').not.toContain(PW);
		expect(r.url).not.toContain(PW);
	}
}

describe('createChangeFlow', () => {
	it('a weakening change: job, progress, confirm with the password, done', async () => {
		net.on('GET /filters/preview/jobs/7', { body: running() }, { body: finished(weakening()) });
		net.on('POST /filters/apply', { body: applied(weakening()) });
		const flow = createChangeFlow();
		flow.start(off);
		expect(flow.state).toEqual({ kind: 'previewing', progress: null });
		await wait(250);
		expect(flow.state).toMatchObject({ kind: 'previewing', progress: { done: 40, total: 100 } });
		await wait(250);
		expect(flow.state.kind).toBe('confirm');
		flow.confirm(PW);
		await settle();
		expect(flow.state.kind).toBe('done');
		const apply = net.to('/filters/apply');
		expect(apply).toHaveLength(1);
		expect(JSON.parse(apply[0]!.body!)).toEqual({ change: off, password: PW });
		expect(net.to('/filters/preview/jobs')[0]!.body).toBe(JSON.stringify({ change: off }));
		expectNoPasswordInPreviews();
		expect(JSON.stringify(flow.state)).not.toContain(PW);
	});

	it('a strengthening change applies without a password key', async () => {
		const rule = addRule({ query: 'hash:' + 'a'.repeat(40), action: 'hide' });
		net.on('GET /filters/preview/jobs/7', { body: finished(strengthening()) });
		net.on('POST /filters/apply', { body: applied(strengthening(), { id: 3 }) });
		const flow = createChangeFlow();
		flow.start(rule);
		await wait(250);
		expect(flow.state.kind).toBe('review');
		flow.apply();
		await settle();
		expect(Object.keys(JSON.parse(net.to('/filters/apply')[0]!.body!))).toEqual(['change']);
		expect(flow.state.kind).toBe('done');
	});

	it('cancel during a job calls cancel, and a later poll answer is ignored', async () => {
		let release!: () => void;
		const late = new Promise<void>((r) => (release = r));
		net.on('GET /filters/preview/jobs/7', async () => {
			await late;
			return { body: finished(weakening()) };
		});
		const flow = createChangeFlow();
		flow.start(off);
		await wait(250);
		flow.cancel();
		release();
		await wait(2000);
		expect(flow.state.kind).toBe('closed');
		expect(net.to('/filters/preview/jobs/7/cancel')).toHaveLength(1);
	});

	it('a job 404 starts one new preview, then fails', async () => {
		net.error('GET /filters/preview/jobs/7', 404, 'preview_not_found');
		const flow = createChangeFlow();
		flow.start(off);
		await wait(250);
		await wait(250);
		expect(
			net.sent.filter((s) => s.method === 'POST' && s.path === '/filters/preview/jobs')
		).toHaveLength(2);
		expect(flow.state.kind).toBe('failed');
	});

	it('401 bad_password shows the error and never triggers the login handler', async () => {
		const login = vi.fn();
		onLoginRequired(login);
		net.on('GET /filters/preview/jobs/7', { body: finished(weakening()) });
		net.error('POST /filters/apply', 401, 'bad_password', { field: 'password' });
		const flow = createChangeFlow();
		flow.start(off);
		await wait(250);
		flow.confirm('wrong');
		await settle();
		expect(flow.state).toMatchObject({ kind: 'confirm', error: 'bad_password' });
		expect(login).not.toHaveBeenCalled();
	});

	it('429 rate_limited: no request until retryAt', async () => {
		net.on('GET /filters/preview/jobs/7', { body: finished(weakening()) });
		net.error('POST /filters/apply', 429, 'rate_limited', {}, { 'Retry-After': '12' });
		const flow = createChangeFlow();
		flow.start(off);
		await wait(250);
		flow.confirm(PW);
		await settle();
		expect(flow.state).toMatchObject({ kind: 'confirm', error: 'rate_limited' });
		await vi.advanceTimersByTimeAsync(11_000);
		flow.confirm(PW);
		await settle();
		expect(net.to('/filters/apply')).toHaveLength(1);
		await vi.advanceTimersByTimeAsync(1_000);
		flow.confirm(PW);
		await settle();
		expect(net.to('/filters/apply')).toHaveLength(2);
	});

	it('409 changed_since_preview previews again, without the password', async () => {
		net.on('GET /filters/preview/jobs/7', { body: finished(strengthening()) });
		net.error('POST /filters/apply', 409, 'changed_since_preview');
		const flow = createChangeFlow();
		flow.start(off);
		await wait(250);
		flow.apply();
		await settle();
		expect(flow.state.kind).toBe('previewing');
		expect(
			net.sent.filter((s) => s.method === 'POST' && s.path === '/filters/preview/jobs')
		).toHaveLength(2);
		expectNoPasswordInPreviews();
	});

	it('a changed 428 returns to confirm and sends nothing more', async () => {
		net.on('GET /filters/preview/jobs/7', { body: finished(strengthening()) });
		net.error('POST /filters/apply', 428, 'confirm_required', { assessment: weakening() });
		const flow = createChangeFlow();
		flow.start(off);
		await wait(250);
		flow.apply();
		await wait(5000);
		expect(flow.state).toMatchObject({ kind: 'confirm', changed: true });
		expect(net.to('/filters/apply')).toHaveLength(1);
	});

	it('a double confirm sends one request', async () => {
		net.on('GET /filters/preview/jobs/7', { body: finished(weakening()) });
		net.on('POST /filters/apply', { body: applied(weakening()) });
		const flow = createChangeFlow();
		flow.start(off);
		await wait(250);
		flow.confirm(PW);
		flow.confirm(PW);
		await settle();
		expect(net.to('/filters/apply')).toHaveLength(1);
	});

	it('a floor removal sends exactly the shown floor_entries', async () => {
		const entries = ['1'.repeat(40), '2'.repeat(40)];
		net.on('GET /filters/preview/jobs/7', {
			body: finished(weakening({ floor_entries: entries }))
		});
		net.on('POST /filters/apply', { body: applied(weakening(), { settled: 2 }) });
		const flow = createChangeFlow();
		flow.start(floorRemoval('file:/config/a.txt', true));
		await wait(250);
		flow.confirm(PW);
		await settle();
		const body = JSON.parse(net.to('/filters/apply')[0]!.body!);
		expect(body.change.hashes).toEqual(entries);
		expect(JSON.parse(net.to('/filters/preview/jobs')[0]!.body!).change).not.toHaveProperty(
			'hashes'
		);
	});
});
