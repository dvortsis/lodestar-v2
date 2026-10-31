// Settings → About: the stale-tab check, the version and diagnostics text, the links and the
// missing-API slots.
import { render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import AboutScreen from './AboutScreen.svelte';
import {
	diagnosticsText,
	isStale,
	links,
	problemUrl,
	repoBase,
	URL_LIMIT,
	versionText,
	type System
} from './logic';

const info = (over: Partial<System> = {}): System =>
	({
		address_change: null,
		build: {
			commit: '0123456789abcdef0123456789abcdef01234567',
			date: 1_700_000_000,
			rustc: 'rustc 1.90.0',
			target: 'x86_64-unknown-linux-musl',
			ui_build_id: 'ui-1'
		},
		channel: 'stable',
		connections: [],
		data: {
			cold_bytes: null,
			database_bytes: 1024,
			dir: '/home/someone/lodestar',
			filesystem_warning: null,
			free_bytes: null,
			index_bytes: null,
			logs_bytes: null
		},
		data_formats: { index: 'v3-t2', rebuild: null, store_schema: 12 },
		deployment: { image_digest: null, image_tag: '0.5.2', kind: 'docker' },
		diagnostics: {
			changed: [{ name: 'network.proxy_url', source: 'env' }],
			logging: 'normal',
			recent_errors: 0
		},
		fixture: false,
		host: {
			arch: 'x86_64',
			cpus: 4,
			memory_bytes: 2 ** 31,
			os: 'linux',
			time_zone: 'Europe/Berlin'
		},
		repository: 'https://github.com/example/lodestar',
		restart: {} as System['restart'],
		restart_required: [],
		started_at: 1_700_000_000,
		uptime_s: 90,
		version: '0.5.2',
		...over
	}) as System;

describe('stale-tab check', () => {
	it('is stale only when both builds are known and differ', () => {
		expect(isStale('a', 'b')).toBe(true);
		expect(isStale('a', 'a')).toBe(false);
		expect(isStale('a', null)).toBe(false);
		expect(isStale('', 'b')).toBe(false);
		expect(isStale('a', undefined)).toBe(false);
	});
});

describe('text and links', () => {
	it('puts the build details in the version text', () => {
		const t = versionText(info(), 'ui-1');
		expect(t).toContain('Lodestar 0.5.2');
		expect(t).toContain('Target: x86_64-unknown-linux-musl');
		expect(t).toContain('UI build: ui-1');
	});
	it('leaves unknown build values out instead of printing null', () => {
		const t = versionText(info({ build: { ...info().build, commit: null, rustc: null } }), 'x');
		expect(t).not.toMatch(/null|Commit|Rust:/);
	});
	it('keeps the data directory, the time zone and setting values out of the diagnostics', () => {
		const t = diagnosticsText(info(), 'ui-1');
		expect(t).toContain('network.proxy_url (env)');
		expect(t).not.toContain('/home/someone');
		expect(t).not.toContain('Berlin');
	});
	it('only links to an https repository, and points a release at its tag', () => {
		expect(repoBase('http://example.com/x')).toBeNull();
		expect(repoBase('javascript:alert(1)')).toBeNull();
		expect(repoBase('https://github.com/a/b.git/')).toBe('https://github.com/a/b');
		expect(links(info())?.source).toBe('https://github.com/example/lodestar/tree/v0.5.2');
		expect(links(info({ channel: 'nightly' }))?.source).toContain('/tree/0123456789abcdef');
		expect(links(info({ repository: '' }))).toBeNull();
	});
	it('prefills the issue only while the URL is short enough', () => {
		const base = 'https://github.com/a/b';
		expect(problemUrl(base, 'short')).toBe(`${base}/issues/new?body=short`);
		expect(problemUrl(base, 'x'.repeat(URL_LIMIT))).toBeNull();
		expect(problemUrl(null, 'x')).toBeNull();
	});
});

describe('the screen', () => {
	const json = (body: unknown, status = 200) =>
		new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
	beforeEach(() => vi.stubGlobal('fetch', vi.fn()));
	afterEach(() => vi.unstubAllGlobals());

	it('offers a reload when the server runs another UI build', async () => {
		vi.mocked(fetch).mockImplementation(async (input) =>
			String(input).includes('/system') ? json(info()) : json({}, 500)
		);
		render(AboutScreen);
		expect(await screen.findByText(/older version of Lodestar/)).toBeTruthy();
		expect(screen.getByRole('button', { name: 'Reload' })).toBeTruthy();
	});
	it('shows the UI build when the server is unreachable, and the rest of the page when updates fail', async () => {
		vi.mocked(fetch).mockResolvedValue(json({ error: { code: 'x', message: 'down' } }, 500));
		render(AboutScreen);
		await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy());
		expect(screen.getByRole('button', { name: 'Try again' })).toBeTruthy();
	});
});
