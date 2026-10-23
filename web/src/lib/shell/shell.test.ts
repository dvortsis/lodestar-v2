import { describe, expect, it, vi } from 'vitest';
import { ApiError, sessionOk } from '$lib/api/client';
import { isBareRoute } from './chrome';
import { DESTINATIONS, isActive, withDensity } from './nav';
import { connectionKind, pillState, type CrawlerSnapshot, type RebuildSnapshot } from './status';
import { createShellStatus } from './status.svelte';

const crawler = (over: Partial<CrawlerSnapshot> = {}): CrawlerSnapshot => ({
	at: 1,
	dht_nodes: 100,
	fetch_active: 3,
	fetch_pending: 10,
	fetched: 5,
	harvested: 9,
	paused: false,
	stored: 4,
	torrents: 1_234_567,
	...over
});
const rebuild = (over: Partial<RebuildSnapshot> = {}): RebuildSnapshot => ({
	done: 10,
	error: null,
	from: 'v3',
	phase: 'building',
	to: 'v4',
	total: 100,
	unavailable: [],
	...over
});

describe('pillState', () => {
	it('is "connecting" before anything arrived, and unknown once live with no snapshot', () => {
		expect(pillState({ stream: 'connecting', crawler: null, rebuild: null }).kind).toBe(
			'connecting'
		);
		const live = pillState({ stream: 'live', crawler: null, rebuild: null });
		expect(live).toEqual({ kind: 'unknown', torrents: null, done: null, total: null });
	});

	it('carries the server count, including zero, and never invents one', () => {
		expect(pillState({ stream: 'live', crawler: crawler(), rebuild: null })).toMatchObject({
			kind: 'crawling',
			torrents: 1_234_567
		});
		expect(
			pillState({
				stream: 'live',
				crawler: crawler({ fetch_active: 0, fetch_pending: 0, torrents: 0 }),
				rebuild: null
			})
		).toMatchObject({ kind: 'idle', torrents: 0 });
	});

	it('says "paused" when the server does, with the count, even while fetches drain', () => {
		expect(
			pillState({ stream: 'live', crawler: crawler({ paused: true }), rebuild: null })
		).toMatchObject({ kind: 'paused', torrents: 1_234_567 });
	});

	it('shows the rebuild while it runs, the failure once, and ignores a finished one', () => {
		for (const phase of ['building', 'finishing', 'swapping'] as const) {
			expect(
				pillState({ stream: 'live', crawler: crawler(), rebuild: rebuild({ phase }) })
			).toEqual({
				kind: 'rebuilding',
				torrents: null,
				done: 10,
				total: 100
			});
		}
		expect(
			pillState({ stream: 'live', crawler: crawler(), rebuild: rebuild({ phase: 'failed' }) }).kind
		).toBe('failed');
		expect(
			pillState({ stream: 'live', crawler: crawler(), rebuild: rebuild({ phase: 'done' }) }).kind
		).toBe('crawling');
	});

	it('drops the numbers while the stream is down or busy, so nothing stale is shown', () => {
		for (const [stream, kind] of [
			['offline', 'reconnecting'],
			['busy', 'busy']
		] as const) {
			expect(pillState({ stream, crawler: crawler(), rebuild: null })).toEqual({
				kind,
				torrents: null,
				done: null,
				total: null
			});
		}
		expect(pillState({ stream: 'connecting', crawler: crawler(), rebuild: null }).kind).toBe(
			'reconnecting'
		);
	});

	it('keeps the last state in a stopped (hidden or signed-out) tab', () => {
		expect(pillState({ stream: 'stopped', crawler: crawler(), rebuild: null }).kind).toBe(
			'crawling'
		);
		expect(pillState({ stream: 'stopped', crawler: null, rebuild: null }).kind).toBe('unknown');
	});
});

describe('connectionKind', () => {
	const err = new ApiError(0, 'network', 'down');
	it('is a full-page state only when the server never answered and failed', () => {
		expect(connectionKind({ stream: 'offline', error: err, everLive: false })).toBe('unreachable');
		expect(connectionKind({ stream: 'offline', error: undefined, everLive: false })).toBe('ok');
	});
	it('is not a full-page state when something else got an answer (only the stream is held up)', () => {
		expect(connectionKind({ stream: 'offline', error: err, everLive: false, restOk: true })).toBe(
			'ok'
		);
	});
	it('does not flicker across retries: connecting with the last error is the same outage', () => {
		const kinds = (['offline', 'connecting', 'offline', 'connecting'] as const).map((stream) =>
			connectionKind({ stream, error: err, everLive: false })
		);
		expect(kinds).toEqual(['unreachable', 'unreachable', 'unreachable', 'unreachable']);
		const banner = (['offline', 'connecting'] as const).map((stream) =>
			connectionKind({ stream, error: err, everLive: true })
		);
		expect(banner).toEqual(['reconnecting', 'reconnecting']);
		// A first connect has no error yet, and a live stream clears it.
		expect(connectionKind({ stream: 'connecting', error: undefined, everLive: false })).toBe('ok');
		expect(connectionKind({ stream: 'live', error: undefined, everLive: true })).toBe('ok');
	});
	it('is a banner once it had answered, and fine in every other stream state', () => {
		expect(connectionKind({ stream: 'offline', error: err, everLive: true })).toBe('reconnecting');
		for (const stream of ['live', 'busy', 'stopped'] as const) {
			expect(connectionKind({ stream, error: err, everLive: false })).toBe('ok');
		}
	});
});

describe('createShellStatus', () => {
	function fake() {
		const handlers: Record<string, (d: unknown) => void> = {};
		const off = vi.fn();
		const client = {
			status: 'connecting' as 'connecting' | 'live' | 'offline',
			error: undefined as ApiError | undefined,
			on: vi.fn((topic: string, h: (d: unknown) => void) => {
				handlers[topic] = h;
				return off;
			})
		};
		return { client, handlers, off };
	}

	it('subscribes to crawler and rebuild, follows them, and unsubscribes', () => {
		const { client, handlers, off } = fake();
		const s = createShellStatus(client as never);
		const stop = s.start();
		expect(client.on.mock.calls.map((c) => c[0])).toEqual(['crawler', 'rebuild']);
		client.status = 'live';
		handlers.crawler!(crawler());
		expect(s.pill.kind).toBe('crawling');
		handlers.rebuild!(rebuild());
		expect(s.pill.kind).toBe('rebuilding');
		stop();
		expect(off).toHaveBeenCalledTimes(2);
	});

	it('calls the server unreachable when the stream and the probe both fail', async () => {
		const { client } = fake();
		const probe = vi.fn(async () => false);
		const s = createShellStatus(client as never, probe);
		client.status = 'offline';
		client.error = new ApiError(0, 'network', 'x');
		s.observe();
		await Promise.resolve();
		expect(probe).toHaveBeenCalledOnce();
		expect(s.connection).toBe('unreachable');
	});

	it('does not, when the stream is held up but the probe gets an answer', async () => {
		const { client } = fake();
		const s = createShellStatus(client as never, async () => true);
		client.status = 'offline';
		client.error = new ApiError(0, 'network', 'x');
		s.observe();
		await vi.waitFor(() => expect(s.connection).toBe('ok'));
	});

	it('does not, once any request has succeeded', () => {
		const { client } = fake();
		const probe = vi.fn(async () => false);
		const s = createShellStatus(client as never, probe);
		sessionOk('/search');
		client.status = 'offline';
		client.error = new ApiError(0, 'network', 'x');
		s.observe();
		expect(s.connection).toBe('ok');
		expect(probe).not.toHaveBeenCalled();
	});

	it('a stream that answered once and then fails is a banner, not a full page', () => {
		const { client } = fake();
		const s = createShellStatus(client as never, async () => false);
		client.status = 'live';
		s.observe();
		client.status = 'offline';
		client.error = new ApiError(0, 'network', 'x');
		s.observe();
		expect(s.connection).toBe('reconnecting');
	});
});

describe('navigation', () => {
	const search = DESTINATIONS[0]!;
	const settings = DESTINATIONS.find((d) => d.id === 'settings')!;

	it('lists the five stable destinations in order', () => {
		expect(DESTINATIONS.map((d) => d.id)).toEqual([
			'search',
			'discover',
			'alerts',
			'insights',
			'settings'
		]);
	});

	it('marks a destination active for its path and its children only', () => {
		expect(isActive(search, '/search')).toBe(true);
		expect(isActive(search, '/search/')).toBe(true);
		expect(isActive(settings, '/settings/about')).toBe(true);
		expect(isActive(settings, '/settingsx')).toBe(false);
		expect(isActive(search, '/t/abc')).toBe(false);
	});

	it('keeps density when moving between destinations, and only then', () => {
		expect(withDensity('/alerts', new URLSearchParams('density=compact&q=x'))).toBe(
			'/alerts?density=compact'
		);
		expect(withDensity('/alerts', new URLSearchParams('q=x'))).toBe('/alerts');
	});
});

describe('chrome', () => {
	it('renders the first-run and sign-in screens bare', () => {
		for (const p of ['/login', '/setup', '/setup/token', '/welcome'])
			expect(isBareRoute(p)).toBe(true);
		for (const p of ['/search', '/loginx', '/settings/general']) expect(isBareRoute(p)).toBe(false);
	});
});
