// The one session check at boot (X-setup-token-boot, UI review I14): where the first load goes.
import { describe, expect, it, vi } from 'vitest';
import { _bootTarget } from './+layout';

const answer = (status: number, body: unknown) =>
	vi.fn(async () => new Response(JSON.stringify(body), { status })) as unknown as typeof fetch;
const status = (logged_in: boolean, setup_required: boolean, notice_required = false) => ({
	logged_in,
	notice_required,
	setup_required
});

describe('_bootTarget', () => {
	it('sends a fresh instance to setup', async () => {
		expect(await _bootTarget(answer(200, status(false, true)), '/search')).toBe('/setup');
	});

	it('sends a signed-out visitor to sign-in, returning to the page and its query', async () => {
		expect(await _bootTarget(answer(200, status(false, false)), '/alerts?x=1')).toBe(
			'/login?next=%2Falerts%3Fx%3D1'
		);
		expect(await _bootTarget(answer(200, status(false, false)), '/')).toBe('/login?next=%2F');
	});

	it('sends a session with the Legal Notice pending to sign-in, which shows it', async () => {
		expect(await _bootTarget(answer(200, status(true, false, true)), '/settings')).toBe(
			'/login?next=%2Fsettings'
		);
	});

	it('lets a signed-in visitor stay', async () => {
		expect(await _bootTarget(answer(200, status(true, false)), '/search')).toBeNull();
	});

	it('asks GET /auth once', async () => {
		const fetch = answer(200, status(true, false));
		await _bootTarget(fetch, '/search');
		const calls = (fetch as unknown as ReturnType<typeof vi.fn>).mock.calls;
		expect(calls).toHaveLength(1);
		expect(calls[0]![0]).toBe('/api/v1/auth');
	});

	const html = vi.fn(async () => new Response('<html>', { status: 200 }));
	const offline = vi.fn(() => Promise.reject(new TypeError('offline')));
	it.each([
		['an error status', answer(503, {})],
		['a body that is not an auth status', answer(200, { ok: true })],
		[
			'flags that are not booleans',
			answer(200, { logged_in: 0, notice_required: 0, setup_required: 1 })
		],
		['no JSON', html as unknown as typeof fetch],
		['an unreachable server', offline as unknown as typeof fetch]
	])('stays put on %s', async (_, fetch) => {
		expect(await _bootTarget(fetch, '/search')).toBeNull();
	});

	it('gives up on a server that never answers, and stays put', async () => {
		const hang = vi.fn(
			(_: string, init: RequestInit) =>
				new Promise<Response>((_resolve, reject) =>
					init.signal!.addEventListener('abort', () =>
						reject(new DOMException('timed out', 'TimeoutError'))
					)
				)
		);
		const target = _bootTarget(hang as unknown as typeof fetch, '/search');
		const signal = (hang.mock.calls[0]![1] as RequestInit).signal!;
		expect(signal.aborted).toBe(false);
		// The deadline is AbortSignal.timeout's own timer; this waits for it, with a margin.
		expect(await target).toBeNull();
		expect(signal.aborted).toBe(true);
	}, 10_000);
});
