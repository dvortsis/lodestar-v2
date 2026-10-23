import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { toasts } from '$lib/ui/feedback/toasts.svelte';
import { dismissAll } from '../form/dialogs';
import { clearIndexCache } from '../form/schema';
import { goldenValues, settingsApi, systemInfo, type Replies } from '../shell/testing';
import ScreenHarness from '../shell/testing/ScreenHarness.svelte';
import { UNDO_MS, type ImportReport } from './backup';

const route = vi.hoisted(() => ({
	page: { params: {} as Record<string, string | undefined>, url: new URL('http://x/'), state: {} }
}));
vi.mock('$app/state', () => ({ page: route.page }));
vi.mock('$app/navigation', () => ({
	beforeNavigate: () => {},
	afterNavigate: () => {},
	goto: vi.fn(),
	replaceState: vi.fn()
}));

beforeAll(async () => {
	await Promise.all([
		import('./BackupPanel.svelte'),
		import('../form/ConfirmDialog.svelte'),
		import('$lib/features/filters/flows/AdultConfirmDialog.svelte')
	]);
});

const reply =
	(body: unknown, status = 200) =>
	() => ({ status, body });
const report = (over: Partial<ImportReport> = {}): ImportReport => ({
	applied: false,
	changes: [
		{
			section: 'crawler',
			field: 'intensity',
			from: 'balanced',
			from_source: 'default',
			restart_required: false,
			to: 'low'
		},
		{
			section: 'crawler',
			field: 'encryption',
			from: 'prefer',
			from_source: 'db',
			restart_required: true,
			to: 'require'
		}
	],
	revisions: { crawler: 4 },
	skipped: [{ key: 'network.dht_port', reason: 'set_by_environment', env: 'LODESTAR_DHT_PORT' }],
	unchanged: 2,
	...over
});
const file = (text: string, size?: number) => {
	const f = new File([text], 'lodestar-settings.toml', { type: 'application/toml' });
	if (size !== undefined) Object.defineProperty(f, 'size', { value: size });
	return f;
};

async function openGeneral(replies: Replies = {}, initial = goldenValues()) {
	route.page.params = { section: 'general' };
	route.page.url = new URL('http://x/settings/general');
	const api = settingsApi({ 'GET /system': reply(systemInfo()), ...replies }, initial);
	render(ScreenHarness, { id: 'general' });
	await screen.findByTestId('settings-backup');
	return api;
}
async function choose(text: string, size?: number) {
	const input = (await screen.findByTestId('settings-import-file')) as HTMLInputElement;
	await fireEvent.change(input, { target: { files: [file(text, size)] } });
}
const posts = (api: ReturnType<typeof settingsApi>, prefix = '/settings/import') =>
	api.calls.filter((c) => c.method === 'POST' && c.path.startsWith(prefix));
const text = (id: string) => screen.getByTestId(id).textContent?.replace(/\s+/g, ' ').trim();

beforeEach(() => {
	vi.spyOn(console, 'warn').mockImplementation(() => {});
	vi.stubGlobal('matchMedia', () => ({
		matches: true,
		addEventListener: () => {},
		removeEventListener: () => {}
	}));
	clearIndexCache();
	toasts.items = [];
	sessionStorage.clear();
	localStorage.clear();
});
afterEach(() => {
	dismissAll();
	vi.useRealTimers();
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe('General: Backup & portability', () => {
	it('sits under the generated fields with the Danger zone last', async () => {
		await openGeneral();
		expect(screen.getByRole('heading', { level: 2, name: 'Backup & portability' })).toBeTruthy();
		const section = screen.getByTestId('settings-backup-section');
		expect(section.id).toBe('backup');
		const danger = screen.getByTestId('settings-danger');
		expect(section.compareDocumentPosition(danger) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
		expect(document.getElementById('general.data_dir')).not.toBeNull();
	});

	it('exports lodestar-settings.toml, with preferences only when asked', async () => {
		await openGeneral();
		const exported = vi.fn(async (url: string) => new Response(`# ${url}\n`, { status: 200 }));
		const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (
			this: HTMLAnchorElement
		) {
			downloaded.push(this.download);
		});
		const downloaded: string[] = [];
		vi.stubGlobal('fetch', exported);
		URL.createObjectURL = vi.fn(() => 'blob:x');
		URL.revokeObjectURL = vi.fn();
		await fireEvent.click(screen.getByTestId('settings-export'));
		await waitFor(() => expect(downloaded).toEqual(['lodestar-settings.toml']));
		await fireEvent.click(screen.getByRole('switch', { name: 'Include preferences' }));
		await fireEvent.click(screen.getByTestId('settings-export'));
		await waitFor(() => expect(downloaded).toHaveLength(2));
		expect(exported.mock.calls.map((c) => c[0])).toEqual([
			'/api/v1/settings/export?preferences=false',
			'/api/v1/settings/export?preferences=true'
		]);
		expect(click).toHaveBeenCalledTimes(2);
	});

	it('says an export failed', async () => {
		await openGeneral();
		vi.stubGlobal(
			'fetch',
			vi.fn(async () =>
				Response.json({ error: { code: 'internal', message: 'x' } }, { status: 500 })
			)
		);
		await fireEvent.click(screen.getByTestId('settings-export'));
		expect(await screen.findByText("Couldn't export your settings. Try again.")).toBeTruthy();
	});

	it('does not send a file over 64 KiB', async () => {
		const api = await openGeneral();
		await choose('x', 64 * 1024 + 1);
		expect(text('settings-import-message')).toBe("That file is over 64 KiB, so it wasn't sent.");
		expect(posts(api)).toHaveLength(0);
	});

	it('shows the dry run: counts, each change with its label and values, restart, and the skipped ones', async () => {
		const api = await openGeneral({ 'POST /settings/import?dry_run=true': reply(report()) });
		await choose('dht.intensity = "low"');
		expect(await screen.findByTestId('settings-import-counts')).toBeTruthy();
		expect(text('settings-import-counts')).toBe('2 changes · 1 skipped · 2 already set');
		const intensity = screen.getByTestId('settings-import-change-crawler.intensity');
		expect(intensity.textContent?.replace(/\s+/g, ' ').trim()).toBe('Intensity: Balanced → Low');
		const enc = screen.getByTestId('settings-import-change-crawler.encryption');
		expect(enc.textContent).toContain('Connection encryption: Prefer → Require');
		expect(enc.textContent).toContain('Applies after restart');
		expect(intensity.textContent).not.toContain('Applies after restart');
		expect(text('settings-import-skipped')).toBe('network.dht_port: set by LODESTAR_DHT_PORT');
		expect(posts(api)[0]).toMatchObject({
			path: '/settings/import?dry_run=true',
			body: { toml: 'dht.intensity = "low"' }
		});
		expect(posts(api)).toHaveLength(1); // nothing is saved by looking
	});

	it('applies with the dry run’s revisions, then offers Undo for ten seconds', async () => {
		const api = await openGeneral({
			'POST /settings/import?dry_run=true': reply(report()),
			'POST /settings/import': reply(report({ applied: true, revisions: { crawler: 6 } })),
			'PATCH /settings': reply({ sections: {} })
		});
		await choose('toml');
		vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'Date'] });
		await fireEvent.click(await screen.findByTestId('settings-import-apply'));
		await vi.waitFor(() =>
			expect(text('settings-import-message')).toContain('Imported 2 changes.')
		);
		expect(posts(api).at(-1)!.body).toEqual({ toml: 'toml', revisions: { crawler: 4 } });
		await fireEvent.click(screen.getByTestId('settings-import-undo'));
		await vi.waitFor(() => expect(text('settings-import-message')).toBe('Import undone.'));
		const patch = api.calls.find((c) => c.method === 'PATCH')!;
		expect(patch.body).toEqual({
			crawler: { if_revision: 6, intensity: null, encryption: 'prefer' }
		});
		expect(screen.queryByTestId('settings-import-undo')).toBeNull();
	});

	it('takes Undo away after ten seconds', async () => {
		await openGeneral({
			'POST /settings/import?dry_run=true': reply(report()),
			'POST /settings/import': reply(report({ applied: true }))
		});
		await choose('toml');
		vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'Date'] });
		await fireEvent.click(await screen.findByTestId('settings-import-apply'));
		await vi.waitFor(() => expect(screen.queryByTestId('settings-import-undo')).not.toBeNull());
		await vi.advanceTimersByTimeAsync(UNDO_MS + 1);
		expect(screen.queryByTestId('settings-import-undo')).toBeNull();
	});

	it('says Undo is not possible any more on a 409', async () => {
		await openGeneral({
			'POST /settings/import?dry_run=true': reply(report()),
			'POST /settings/import': reply(report({ applied: true })),
			'PATCH /settings': reply({ error: { code: 'stale_revision', message: 'stale' } }, 409)
		});
		await choose('toml');
		await fireEvent.click(await screen.findByTestId('settings-import-apply'));
		await fireEvent.click(await screen.findByTestId('settings-import-undo'));
		await waitFor(() =>
			expect(text('settings-import-message')).toContain("Undo isn't possible any more")
		);
	});

	it('re-runs the dry run and says so when a section changed meanwhile (409 stale_revision)', async () => {
		let applies = 0;
		const api = await openGeneral({
			'POST /settings/import?dry_run=true': () => ({
				status: 200,
				body: report({ revisions: { crawler: 4 + posts(api2()).length } })
			}),
			'POST /settings/import': () => {
				applies++;
				return { status: 409, body: { error: { code: 'stale_revision', message: 'stale' } } };
			}
		});
		const api2 = () => api;
		await choose('toml');
		await fireEvent.click(await screen.findByTestId('settings-import-apply'));
		await waitFor(() =>
			expect(text('settings-import-message')).toContain(
				'Your settings changed while you were looking'
			)
		);
		expect(applies).toBe(1);
		expect(posts(api).filter((c) => c.path.includes('dry_run'))).toHaveLength(2);
		expect(screen.getByTestId('settings-import-apply')).toBeTruthy(); // still there to try again
	});

	it('asks the 18+ confirmation once, then retries once with confirm_adult', async () => {
		let applies = 0;
		const api = await openGeneral({
			'POST /settings/import?dry_run=true': reply(report()),
			'POST /settings/import': () =>
				++applies === 1
					? {
							status: 422,
							body: { error: { code: 'adult_confirmation_required', message: '18+' } }
						}
					: { status: 200, body: report({ applied: true }) }
		});
		await choose('toml');
		await fireEvent.click(await screen.findByTestId('settings-import-apply'));
		const dialog = await screen.findByRole('dialog');
		expect(dialog.textContent).toContain('18');
		await fireEvent.click(
			within(dialog).getByRole('button', { name: /confirm|yes|i am|turn on|continue|show/i })
		);
		await waitFor(() => expect(text('settings-import-message')).toContain('Imported 2 changes.'));
		const applying = posts(api).filter((c) => !c.path.includes('dry_run'));
		expect(applying.map((c) => (c.body as Record<string, unknown>).confirm_adult)).toEqual([
			undefined,
			true
		]);
	});

	it('does not retry when the 18+ confirmation is declined', async () => {
		const api = await openGeneral({
			'POST /settings/import?dry_run=true': reply(report()),
			'POST /settings/import': reply(
				{ error: { code: 'adult_confirmation_required', message: '18+' } },
				422
			)
		});
		await choose('toml');
		await fireEvent.click(await screen.findByTestId('settings-import-apply'));
		const dialog = await screen.findByRole('dialog');
		await fireEvent.click(within(dialog).getByRole('button', { name: /cancel/i }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(posts(api).filter((c) => !c.path.includes('dry_run'))).toHaveLength(1);
	});

	it.each([
		[413, 'import_too_large', 'The file is over 64 KiB.'],
		[422, 'invalid_import', 'This is not a Lodestar settings file.']
	])('shows the server’s message for %i %s', async (status, code, message) => {
		const api = await openGeneral({
			'POST /settings/import?dry_run=true': reply({ error: { code, message } }, status)
		});
		await choose('nonsense');
		await waitFor(() => expect(text('settings-import-message')).toBe(message));
		expect(screen.queryByTestId('settings-import-diff')).toBeNull();
		expect(posts(api)).toHaveLength(1);
	});

	it('says when a file would change nothing, and disables Apply', async () => {
		await openGeneral({
			'POST /settings/import?dry_run=true': reply(
				report({ changes: [], skipped: [], unchanged: 5 })
			)
		});
		await choose('toml');
		await screen.findByTestId('settings-import-diff');
		expect(text('settings-import-counts')).toBe('0 changes · 0 skipped · 5 already set');
		expect(screen.getByText('Nothing in this file would change a setting.')).toBeTruthy();
		expect(screen.getByTestId('settings-import-apply').getAttribute('disabled')).not.toBeNull();
	});
});

describe('General: Danger zone', () => {
	const values = (db: Record<string, string[]>) =>
		goldenValues().map((s) => ({
			...s,
			revision: 7,
			fields: Object.fromEntries(
				Object.entries(s.fields).map(([k, v]) => [
					k,
					{ ...v, source: db[s.section]?.includes(k) ? 'db' : v.source }
				])
			)
		}));

	it('counts the saved values outside Preferences, asks with Cancel focused, then resets them in one change', async () => {
		const api = await openGeneral(
			{ 'PATCH /settings': reply({ sections: {} }) },
			values({
				crawler: ['intensity', 'timeout_secs'],
				search: ['heap_mib'],
				preferences: ['theme', 'mode']
			}) as never
		);
		await fireEvent.click(screen.getByTestId('settings-reset-all'));
		const dialog = await screen.findByRole('dialog', { name: 'Reset all settings?' });
		expect(dialog.textContent).toContain("This removes 3 saved settings, and it can't be undone");
		expect(dialog.textContent).toContain("config.toml and environment variables aren't touched");
		await waitFor(() =>
			expect(document.activeElement).toBe(within(dialog).getByRole('button', { name: 'Cancel' }))
		);
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Reset 3 saved settings' }));
		await waitFor(() => expect(api.calls.some((c) => c.method === 'PATCH')).toBe(true));
		expect(api.calls.find((c) => c.method === 'PATCH')!.body).toEqual({
			crawler: { if_revision: 7, intensity: null, timeout_secs: null },
			search: { if_revision: 7, heap_mib: null }
		});
		await waitFor(() => expect(toasts.items.at(-1)?.text).toBe('Reset 3 saved settings.'));
	});

	it('sends nothing if the question is cancelled', async () => {
		const api = await openGeneral({}, values({ crawler: ['intensity'] }) as never);
		await fireEvent.click(screen.getByTestId('settings-reset-all'));
		const dialog = await screen.findByRole('dialog', { name: 'Reset all settings?' });
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(api.calls.filter((c) => c.method === 'PATCH')).toHaveLength(0);
	});

	it('says so, without a dialog, when nothing is saved in the app', async () => {
		const api = await openGeneral();
		await fireEvent.click(screen.getByTestId('settings-reset-all'));
		await waitFor(() => expect(toasts.items.at(-1)?.text).toContain('nothing to reset'));
		expect(screen.queryByRole('dialog')).toBeNull();
		expect(api.calls.filter((c) => c.method === 'PATCH')).toHaveLength(0);
	});

	it('reports a refused reset and changes nothing', async () => {
		await openGeneral(
			{ 'PATCH /settings': reply({ error: { code: 'stale_revision', message: 'stale' } }, 409) },
			values({ crawler: ['intensity'] }) as never
		);
		await fireEvent.click(screen.getByTestId('settings-reset-all'));
		await fireEvent.click(await screen.findByRole('button', { name: 'Reset 1 saved settings' }));
		await waitFor(() => expect(toasts.items.at(-1)?.text).toContain("Couldn't reset the settings"));
	});
});

describe('View as config.toml', () => {
	it('lists the saved values as config.toml and LODESTAR_ lines, never a default, with Copy', async () => {
		route.page.params = { section: 'crawler' };
		route.page.url = new URL('http://x/settings/crawler');
		const initial = goldenValues();
		const crawler = initial.find((s) => s.section === 'crawler')!;
		crawler.fields.intensity = { ...crawler.fields.intensity!, source: 'db', value: 'low' };
		crawler.fields.timeout_secs = { ...crawler.fields.timeout_secs!, source: 'file', value: 30 };
		settingsApi({ 'GET /system': reply(systemInfo()) }, initial);
		const write = vi.fn(async () => {});
		vi.stubGlobal('navigator', { ...navigator, clipboard: { writeText: write } });
		render(ScreenHarness, { id: 'crawler' });
		const details = await screen.findByTestId('settings-config-view');
		(details as HTMLDetailsElement).open = true;
		await fireEvent(details, new Event('toggle'));
		const toml = await screen.findByTestId('settings-config-toml');
		expect(toml.textContent).toBe(
			'dht.intensity = "low"  # LODESTAR_INTENSITY\nfetch.timeout_secs = 30'
		);
		expect(screen.getByTestId('settings-config-env').textContent).toBe('LODESTAR_INTENSITY=low');
		await fireEvent.click(screen.getByTestId('settings-config-copy-toml'));
		await waitFor(() =>
			expect(write).toHaveBeenCalledWith(
				'dht.intensity = "low"  # LODESTAR_INTENSITY\nfetch.timeout_secs = 30'
			)
		);
	});

	it('says so when nothing differs from its default', async () => {
		route.page.params = { section: 'crawler' };
		route.page.url = new URL('http://x/settings/crawler');
		settingsApi({ 'GET /system': reply(systemInfo()) });
		render(ScreenHarness, { id: 'crawler' });
		const details = await screen.findByTestId('settings-config-view');
		(details as HTMLDetailsElement).open = true;
		await fireEvent(details, new Event('toggle'));
		expect((await screen.findByTestId('settings-config-none')).textContent).toBe(
			'Nothing on this screen differs from its default.'
		);
	});
});
