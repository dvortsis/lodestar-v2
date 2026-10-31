// Settings → Logging's privacy invariants (brief S-logs-2 item 10) against a stubbed fetch: nothing
// turns on without its dialog, Cancel sends nothing, a failed request leaves the kind off, presets
// never touch the opt-ins, the opt-ins offer no "until I turn it off", the banners follow the
// server, and turning kinds off offers to delete the files written meanwhile.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { initLocale } from '$lib/i18n/locale.svelte';
import { dismissAll } from '../form/dialogs';
import Banners from './Banners.svelte';
import DataKinds from './DataKinds.svelte';
import { turnOffAll } from './privacy';
import { LoggingCatalog } from './catalog.svelte';
import type { Catalog, LogKind } from './rules';

vi.setConfig({ testTimeout: 30_000 });
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });

const NOW = 2_000_000_000;
const kind = (k: LogKind['kind'], until = 0): LogKind => ({
	kind: k,
	identifying: !['client', 'size', 'peer_activity', 'filter_changes'].includes(k),
	opt_in: k === 'peer_activity' || k === 'filter_changes',
	default_on: k === 'client' || k === 'size',
	until: k === 'client' || k === 'size' ? -1 : until,
	outputs: { console: false, file: true, live: true, event_log: false }
});
const KINDS = [
	'hash',
	'name',
	'path',
	'url',
	'query',
	'client',
	'peer',
	'size',
	'peer_activity',
	'filter_changes'
] as const;

let server: { catalog: Catalog; files: unknown[] };
let calls: { method: string; path: string; body: unknown }[];
let patchReply: { status: number; body: unknown } | undefined;
let catalogFails = false;

function catalogWith(on: Partial<Record<string, number>> = {}, data = 'private'): Catalog {
	return {
		now: NOW,
		events: [
			{
				id: 'pipeline.stored',
				component: 'pipeline',
				level: 'info',
				default_on: true,
				current: true,
				required: false,
				opt_in: null,
				kinds: ['hash', 'name'],
				label_key: 'logs.event.pipeline.stored.label',
				help_key: 'logs.event.pipeline.stored.help',
				rate_per_hour: 1200
			}
		],
		kinds: KINDS.map((k) => kind(k, on[k] ?? 0)),
		outputs: [],
		presets: { data, verbosity: 'normal', temporary: null }
	};
}

let revision = 3;
const SECTION = {
	section: 'logging',
	get revision() {
		return revision;
	},
	restart_required: [],
	fields: Object.fromEntries(
		Object.entries({
			data: 'private',
			data_until: 0,
			kinds: {},
			console_detail: 'private'
		}).map(([k, value]) => [
			k,
			{ value, env: null, read_only: false, restart_required: false, source: 'default' }
		])
	)
};

beforeEach(() => {
	initLocale('en');
	vi.spyOn(Date, 'now').mockReturnValue(NOW * 1000);
	server = { catalog: catalogWith(), files: [] };
	calls = [];
	patchReply = undefined;
	revision = 3;
	catalogFails = false;
	vi.stubGlobal(
		'fetch',
		vi.fn(async (url: string, init: RequestInit = {}) => {
			const method = init.method ?? 'GET';
			const path = new URL(url, 'http://x').pathname.replace('/api/v1', '');
			const body = init.body ? JSON.parse(String(init.body)) : undefined;
			calls.push({ method, path, body });
			const reply = (status: number, b: unknown) =>
				new Response(JSON.stringify(b), {
					status,
					headers: { 'Content-Type': 'application/json' }
				});
			if (path === '/logs/catalog')
				return catalogFails
					? reply(500, { error: { code: 'internal', message: 'down' } })
					: reply(200, server.catalog);
			if (path === '/settings/logging') return reply(200, { ...SECTION, revision });
			if (path === '/logs/files' && method === 'GET')
				return reply(200, {
					files: server.files,
					folder: '/data/logs',
					index_readable: true,
					reason: null,
					state: 'on'
				});
			if (path === '/logs/files' && method === 'DELETE')
				return reply(200, { deleted: server.files.length, bytes: 0, skipped: [] });
			if (path === '/settings' && method === 'PATCH') {
				if (patchReply) return reply(patchReply.status, patchReply.body);
				return reply(200, { sections: { logging: { if_revision: 3, ...SECTION, revision: 4 } } });
			}
			return reply(404, { error: { code: 'not_found', message: path } });
		})
	);
});
afterEach(() => {
	dismissAll();
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});

async function mount(component: typeof DataKinds | typeof Banners = DataKinds) {
	const catalog = new LoggingCatalog();
	await catalog.load();
	render(component, { catalog });
	return catalog;
}
const patches = () => calls.filter((c) => c.method === 'PATCH').map((c) => c.body);
const names = () => screen.getByRole('switch', { name: 'Torrent names' }) as HTMLInputElement;

describe('data kinds', () => {
	it('says which enabled events would carry a kind', async () => {
		await mount();
		expect(screen.getByTestId('log-kind-name').textContent).toContain(
			"Would appear in events you've turned on (1)"
		);
		expect(screen.getByTestId('log-kind-peer').textContent).toContain('None of the events');
	});

	it('asks before an identifying kind turns on; Cancel sends nothing and leaves it off', async () => {
		await mount();
		await fireEvent.click(names());
		const dialog = await screen.findByRole(
			'dialog',
			{ name: 'Write torrent details to the log?' },
			{ timeout: 10_000 }
		);
		expect(dialog.textContent).toContain('Torrent names will be written to the log file');
		expect(document.activeElement?.textContent?.trim()).toBe('Cancel');
		await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(patches()).toEqual([]);
		expect(names().checked).toBe(false);
	});

	it('sends the chosen deadline, and shows it on once the server has it', async () => {
		await mount();
		await fireEvent.click(names());
		const hour = await screen.findByRole('button', { name: 'For 1 hour' }, { timeout: 10_000 });
		server.catalog = catalogWith({ name: NOW + 3600 });
		await fireEvent.click(hour);
		await until(() =>
			expect(patches()).toEqual([{ logging: { if_revision: 3, kinds: { name: NOW + 3600 } } }])
		);
		await until(() => expect(names().checked).toBe(true));
		expect(screen.getByTestId('log-kind-left-name').textContent).toContain('1 h');
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		await fireEvent.click(names()); // turning off asks nothing
		await until(() => expect(patches()).toHaveLength(2));
		expect(patches()[1]).toEqual({ logging: { if_revision: 3, kinds: {} } });
	});

	it('a failed request leaves the kind off and says so', async () => {
		await mount();
		patchReply = { status: 500, body: { error: { code: 'internal', message: 'disk full' } } };
		await fireEvent.click(names());
		await fireEvent.click(
			await screen.findByRole('button', { name: 'For 24 hours' }, { timeout: 10_000 })
		);
		await until(() => expect(screen.getByRole('alert').textContent).toContain('stays off'));
		expect(patches()).toEqual([{ logging: { if_revision: 3, kinds: { name: NOW + 86_400 } } }]);
		expect(names().checked).toBe(false);
	});

	it('a preset asks first, never touches the opt-ins, and Cancel keeps Private', async () => {
		await mount();
		const full = screen.getByRole('radio', { name: 'Full' });
		await fireEvent.click(full);
		await fireEvent.click(
			await screen.findByRole('button', { name: 'Cancel' }, { timeout: 10_000 })
		);
		await until(() =>
			expect(screen.getByRole('radio', { name: 'Private' }).getAttribute('aria-checked')).toBe(
				'true'
			)
		);
		expect(patches()).toEqual([]);
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		await fireEvent.click(screen.getByRole('radio', { name: 'Full' }));
		await fireEvent.click(
			await screen.findByRole('button', { name: 'Until I turn it off' }, { timeout: 10_000 })
		);
		await until(() =>
			expect(patches()).toEqual([
				{ logging: { if_revision: 3, data: 'full', data_until: -1, kinds: {} } }
			])
		);
	});

	it('builds the change after the dialog, at the revision it read then', async () => {
		await mount();
		await fireEvent.click(names());
		const hour = await screen.findByRole('button', { name: 'For 1 hour' }, { timeout: 10_000 });
		revision = 7; // another tab saved while the dialog was open
		await fireEvent.click(hour);
		await until(() =>
			expect(patches()).toEqual([{ logging: { if_revision: 7, kinds: { name: NOW + 3600 } } }])
		);
	});

	it('a refused stale change leaves the kind off', async () => {
		await mount();
		patchReply = {
			status: 409,
			body: { error: { code: 'stale_revision', message: 'changed meanwhile' } }
		};
		await fireEvent.click(names());
		await fireEvent.click(
			await screen.findByRole('button', { name: 'For 1 hour' }, { timeout: 10_000 })
		);
		await until(() => expect(screen.getByRole('alert').textContent).toContain('stays off'));
		expect(names().checked).toBe(false);
	});

	it("says so when the state can't be read after a change, and reloads on the next tick", async () => {
		const catalog = await mount();
		catalogFails = true;
		await fireEvent.click(screen.getByRole('switch', { name: 'Sign-in and API client addresses' }));
		await until(() => expect(screen.getByRole('alert').textContent).toContain("couldn't be read"));
		catalogFails = false;
		server.catalog = catalogWith();
		const loads = calls.filter((c) => c.path === '/logs/catalog').length;
		catalog.tick();
		await until(() =>
			expect(calls.filter((c) => c.path === '/logs/catalog').length).toBe(loads + 1)
		);
		await until(() => expect(catalog.error).toBeUndefined());
	});

	it('offers to delete files when a preset turns identifying kinds off', async () => {
		server.catalog = catalogWith({ hash: NOW + 600, name: NOW + 600 }, 'full');
		server.files = [{ name: 'a.jsonl', size: 10, details: true, opt_in: false, current: true }];
		await mount();
		await fireEvent.click(screen.getByRole('radio', { name: 'Hashes' }));
		const hour = await screen.findByRole('button', { name: 'For 1 hour' }, { timeout: 10_000 });
		server.catalog = catalogWith({ hash: NOW + 3600 }, 'hashes');
		await fireEvent.click(hour);
		await screen.findByRole(
			'dialog',
			{ name: 'Also delete log files that contain torrent details?' },
			{ timeout: 10_000 }
		);
	});

	it('an opt-in has its own warning and no "until I turn it off"', async () => {
		await mount();
		await fireEvent.click(screen.getByRole('switch', { name: 'Peer activity' }));
		const dialog = await screen.findByRole(
			'dialog',
			{ name: 'Record peer activity?' },
			{ timeout: 10_000 }
		);
		expect(dialog.textContent).toContain('never keeps which peer had which torrent');
		expect(screen.queryByRole('button', { name: 'Until I turn it off' })).toBeNull();
		await fireEvent.click(screen.getByRole('button', { name: 'For 24 hours' }));
		await until(() =>
			expect(patches()).toEqual([
				{ logging: { if_revision: 3, peer_activity_until: NOW + 86_400 } }
			])
		);
	});
});

describe('banners', () => {
	it('show what the server has on, with Turn off now and the delete offer', async () => {
		server.catalog = catalogWith({ name: NOW + 600, filter_changes: NOW + 3000 }, 'custom');
		server.files = [
			{ name: 'a.jsonl', size: 2048, details: true, opt_in: false, current: true },
			{ name: 'b.jsonl.gz', size: 1024, details: false, opt_in: false, current: false }
		];
		await mount(Banners);
		expect(screen.getByTestId('logging-banner-identifying').textContent).toContain('10 min');
		expect(screen.getByTestId('logging-banner-filter_changes').textContent).toContain('50 min');
		expect(screen.queryByTestId('logging-banner-peer_activity')).toBeNull();
		const [first] = screen.getAllByRole('button', { name: 'Turn off now' });
		server.catalog = catalogWith({ filter_changes: NOW + 3000 });
		await fireEvent.click(first!);
		const offer = await screen.findByRole(
			'dialog',
			{
				name: 'Also delete log files that contain torrent details?'
			},
			{ timeout: 10_000 }
		);
		expect(offer.textContent).toContain('Log files written while torrent details were on: 1');
		expect(document.activeElement?.textContent?.trim()).toBe('Keep the files');
		await fireEvent.click(screen.getByRole('button', { name: 'Delete log files (1)' }));
		await until(() =>
			expect(calls.find((c) => c.method === 'DELETE')?.body).toEqual({ which: 'details' })
		);
		expect(patches()).toEqual([
			{ logging: { if_revision: 3, data: 'private', data_until: 0, kinds: {} } }
		]);
		await until(() => expect(screen.queryByTestId('logging-banner-identifying')).toBeNull());
		expect(screen.getByTestId('logging-banner-filter_changes')).toBeTruthy();
	});

	it('Keep sends no delete', async () => {
		server.catalog = catalogWith({ peer_activity: NOW + 600 });
		server.files = [{ name: 'a.jsonl', size: 10, details: false, opt_in: true, current: true }];
		await mount(Banners);
		server.catalog = catalogWith();
		await fireEvent.click(screen.getByRole('button', { name: 'Turn off now' }));
		await fireEvent.click(
			await screen.findByRole('button', { name: 'Keep the files' }, { timeout: 10_000 })
		);
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(patches()).toEqual([{ logging: { if_revision: 3, peer_activity_until: 0 } }]);
		expect(calls.some((c) => c.method === 'DELETE')).toBe(false);
	});

	it('/logs Turn off now turns off the identifying kinds and both opt-ins', async () => {
		server.catalog = catalogWith({ name: NOW + 600, peer_activity: NOW + 600 }, 'custom');
		const catalog = new LoggingCatalog();
		await catalog.load();
		server.catalog = catalogWith();
		await turnOffAll({ catalog });
		expect(patches()).toEqual([
			{
				logging: {
					if_revision: 3,
					data: 'private',
					data_until: 0,
					kinds: {},
					peer_activity_until: 0,
					change_details_until: 0
				}
			}
		]);
		expect(catalog.catalog?.kinds.find((k) => k.kind === 'name')?.until).toBe(0);
	});
});
