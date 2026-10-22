import { describe, expect, it, vi } from 'vitest';
import { ApiError } from '$lib/api/client';
import { isBareRoute } from './chrome';
import { loginTarget, safeNext } from './handoff';
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
	it('is a banner once it had answered, and fine in every other stream state', () => {
		expect(connectionKind({ stream: 'offline', error: err, everLive: true })).toBe('reconnecting');
		for (const stream of ['connecting', 'live', 'busy', 'stopped'] as const) {
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

	it('remembers that the stream answered once, so a later failure is a banner', () => {
		const { client } = fake();
		const s = createShellStatus(client as never);
		client.status = 'offline';
		client.error = new ApiError(0, 'network', 'x');
		s.observe();
		expect(s.connection).toBe('unreachable');
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

describe('login hand-off', () => {
	it('sends a first run to /setup and everyone else to /login with the way back', () => {
		expect(loginTarget('/search?q=a', { setupRequired: true })).toBe('/setup');
		expect(loginTarget('/search?q=a b', { setupRequired: false })).toBe(
			'/login?next=%2Fsearch%3Fq%3Da%20b'
		);
		expect(loginTarget('/', { setupRequired: false })).toBe('/login');
	});

	it('never carries a next that could leave the site', () => {
		for (const bad of [
			'//evil.example',
			'https://evil.example',
			'/\\evil',
			'javascript:alert(1)'
		]) {
			expect(safeNext(bad)).toBe('/');
		}
		expect(safeNext('/alerts/3')).toBe('/alerts/3');
	});
});
