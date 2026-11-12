// Settings → Outputs (OUT-2) against a stubbed fetch: only Torznab keys are listed; a new key asks
// for the password and sends `torznab` alone, named for the app; Cancel sends nothing; the secret
// shows once in Security's reveal dialog and is gone when it closes; Regenerate says what breaks;
// export_adult asks the 18+ question before it's turned on and nothing to turn it off; the export
// link asks for the bitmagnet format and shows the job's counts, including a cap that cut it short.
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { initLocale } from '$lib/i18n/locale.svelte';
import { dismissAll } from '../form/dialogs';
import { dismissPasswordDialogs } from '../security/confirm';
import { dismissReveals } from '../security/reveal';
import OutputsScreen from './OutputsScreen.svelte';

vi.mock('$app/navigation', () => ({ beforeNavigate: () => {}, goto: vi.fn(async () => {}) }));
vi.setConfig({ testTimeout: 30_000 });
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });

type Reply = { status: number; body: unknown };
let replies: Record<string, (body: unknown) => Reply>;
let calls: { method: string; path: string; body: unknown }[];

const SECRET = 'ls_torznab_secret_0123456789';
const key = (id: number, name: string, scopes: string[], status = 'active') => ({
	id,
	name,
	kind: 'api_key',
	scopes,
	status,
	prefix: `ls_${id}`,
	created_at: 1_700_000_000,
	last_used_at: null,
	expires_at: null,
	revoked_at: null,
	saved_search_id: null
});
const CONTENT = (exportAdult: boolean) => ({
	section: 'content',
	revision: 1,
	restart_required: [],
	fields: { export_adult: { value: exportAdult, env: null, read_only: false, source: 'default' } }
});
let jobs: unknown[];

beforeAll(async () => {
	await Promise.all([
		import('../security/PasswordDialog.svelte'),
		import('../security/Reveal.svelte'),
		import('./AdultDialog.svelte')
	]);
}, 120_000);

beforeEach(() => {
	initLocale('en');
	calls = [];
	jobs = [];
	replies = {
		'GET /keys': () => ({
			status: 200,
			body: {
				keys: [
					key(1, 'Prowlarr', ['torznab']),
					key(2, 'Search app', ['search']),
					key(3, 'Old', ['search', 'torznab'])
				]
			}
		}),
		'POST /keys': (b) => ({
			status: 200,
			body: {
				key: key(9, (b as { name: string }).name, (b as { scopes: string[] }).scopes),
				secret: SECRET
			}
		}),
		'POST /keys/1/regenerate': () => ({
			status: 200,
			body: { key: key(10, 'Prowlarr', ['torznab']), secret: SECRET }
		}),
		'GET /settings/content': () => ({ status: 200, body: CONTENT(false) }),
		'PATCH /settings/content': () => ({ status: 200, body: CONTENT(true) }),
		'GET /jobs': () => ({ status: 200, body: { jobs } })
	};
	vi.stubGlobal(
		'fetch',
		vi.fn(async (url: string, init: RequestInit = {}) => {
			const method = init.method ?? 'GET';
			const path = new URL(url, 'http://x').pathname.replace('/api/v1', '');
			const body = typeof init.body === 'string' ? JSON.parse(init.body) : undefined;
			calls.push({ method, path, body });
			const reply = replies[`${method} ${path}`]?.(body) ?? {
				status: 404,
				body: { error: { code: 'not_found', message: 'none' } }
			};
			return new Response(JSON.stringify(reply.body), {
				status: reply.status,
				headers: { 'Content-Type': 'application/json' }
			});
		})
	);
});
afterEach(async () => {
	dismissAll();
	dismissPasswordDialogs();
	dismissReveals();
	await Promise.resolve();
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
	document.body.innerHTML = '';
});

const writes = () => calls.filter((c) => c.method !== 'GET');

async function password(name: RegExp) {
	const dialog = await screen.findByRole('dialog', { name }, { timeout: 10_000 });
	return dialog;
}

describe('Torznab', () => {
	it('shows the address and only the keys with the torznab scope', async () => {
		render(OutputsScreen);
		expect((screen.getByTestId('outputs-torznab-url') as HTMLInputElement).value).toBe(
			`${location.origin}/api/v1/torznab/api`
		);
		const rows = await screen.findAllByTestId('outputs-key');
		expect(rows.map((r) => within(r).getAllByText(/./)[0]!.textContent)).toEqual([
			'Prowlarr',
			'Old'
		]);
		// The wider key is marked; the torznab-only one isn't.
		expect(screen.getAllByTestId('outputs-key-wider')).toHaveLength(1);
		expect(within(rows[1]!).getByTestId('outputs-key-wider')).toBeTruthy();
		// The new key's name is the first app without one.
		expect((screen.getByRole('textbox', { name: /App/ }) as HTMLInputElement).value).toBe('Sonarr');
	});

	it('Cancel on the password dialog creates nothing', async () => {
		render(OutputsScreen);
		await screen.findAllByTestId('outputs-key');
		await fireEvent.click(screen.getByTestId('outputs-key-create'));
		const dialog = await password(/Create a Torznab key for Sonarr/);
		await fireEvent.click(within(dialog).getByTestId('password-dialog-cancel'));
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(writes()).toEqual([]);
	});

	it('creates a torznab-only key behind the password and shows the secret once', async () => {
		render(OutputsScreen);
		await screen.findAllByTestId('outputs-key');
		await fireEvent.click(screen.getByTestId('outputs-key-create'));
		const dialog = await password(/Create a Torznab key for Sonarr/);
		await fireEvent.input(within(dialog).getByLabelText(/Your password/), {
			target: { value: 'pw' }
		});
		await fireEvent.click(within(dialog).getByTestId('password-dialog-confirm'));
		const reveal = await screen.findByRole(
			'dialog',
			{ name: 'Copy the secret for Sonarr' },
			{ timeout: 10_000 }
		);
		expect(writes()).toEqual([
			{
				method: 'POST',
				path: '/keys',
				body: { name: 'Sonarr', kind: 'api_key', scopes: ['torznab'], password: 'pw' }
			}
		]);
		expect((within(reveal).getByTestId('key-secret') as HTMLInputElement).value).toBe(SECRET);
		expect(reveal.textContent).toContain('the only time Lodestar shows it');
		await fireEvent.click(within(reveal).getByTestId('key-secret-done'));
		await until(() => expect(document.body.innerHTML).not.toContain(SECRET));
		expect(screen.getByTestId('outputs-keys-result').textContent).toContain(
			'Created a key for Sonarr.'
		);
		// The next suggestion moves on.
		expect((screen.getByRole('textbox', { name: /App/ }) as HTMLInputElement).value).toBe('Radarr');
	});

	it('refuses an empty name without asking for the password', async () => {
		render(OutputsScreen);
		await screen.findAllByTestId('outputs-key');
		const input = screen.getByRole('textbox', { name: /App/ });
		await fireEvent.input(input, { target: { value: '  ' } });
		await fireEvent.click(screen.getByTestId('outputs-key-create'));
		expect(await screen.findByText('Enter a name.')).toBeTruthy();
		expect(screen.queryByRole('dialog')).toBeNull();
		expect(writes()).toEqual([]);
	});

	it('Regenerate says what breaks and asks for the password', async () => {
		render(OutputsScreen);
		await screen.findAllByTestId('outputs-key');
		await fireEvent.click(screen.getByRole('button', { name: 'Regenerate the key for Prowlarr' }));
		const dialog = await password(/Regenerate the key for Prowlarr/);
		expect(dialog.textContent).toContain(
			'Prowlarr finds nothing in Lodestar until you paste the new key'
		);
		await fireEvent.input(within(dialog).getByLabelText(/Your password/), {
			target: { value: 'pw' }
		});
		await fireEvent.click(within(dialog).getByTestId('password-dialog-confirm'));
		await screen.findByRole(
			'dialog',
			{ name: 'Copy the secret for Prowlarr' },
			{ timeout: 10_000 }
		);
		expect(writes()).toEqual([
			{ method: 'POST', path: '/keys/1/regenerate', body: { password: 'pw' } }
		]);
	});

	it('a failed list offers Try again', async () => {
		replies['GET /keys'] = () => ({ status: 500, body: { error: { code: 'x', message: 'x' } } });
		render(OutputsScreen);
		expect(await screen.findByText("Couldn't load the keys.")).toBeTruthy();
		replies['GET /keys'] = () => ({ status: 200, body: { keys: [] } });
		await fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
		expect(await screen.findByTestId('outputs-keys-empty')).toBeTruthy();
	});
});

describe('bitmagnet export', () => {
	it('says what is always left out, and links to the bitmagnet format', async () => {
		render(OutputsScreen);
		expect(screen.getByTestId('outputs-bitmagnet-left-out').textContent).toMatch(
			/private sources.*hidden.*unverified.*blocklisted/
		);
		const link = screen.getByTestId('outputs-bitmagnet-start') as HTMLAnchorElement;
		expect(link.getAttribute('href')).toBe('/api/v1/export?format=bitmagnet');
		expect(link.hasAttribute('download')).toBe(true);
	});

	it('asks the 18+ question before including adult torrents, and nothing to stop', async () => {
		render(OutputsScreen);
		const toggle = (await screen.findByTestId('outputs-adult')) as HTMLInputElement;
		expect(toggle.checked).toBe(false);
		await fireEvent.click(toggle);
		const dialog = await screen.findByRole(
			'dialog',
			{ name: 'Include adult torrents in exports?' },
			{ timeout: 10_000 }
		);
		await fireEvent.click(within(dialog).getByTestId('outputs-adult-cancel'));
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(writes()).toEqual([]);
		expect((screen.getByTestId('outputs-adult') as HTMLInputElement).checked).toBe(false);

		await fireEvent.click(screen.getByTestId('outputs-adult'));
		const again = await screen.findByRole(
			'dialog',
			{ name: 'Include adult torrents in exports?' },
			{ timeout: 10_000 }
		);
		await fireEvent.click(within(again).getByTestId('outputs-adult-include'));
		await until(() =>
			expect(writes()).toEqual([
				{
					method: 'PATCH',
					path: '/settings/content',
					body: { export_adult: true, confirm_adult: true }
				}
			])
		);
		await until(() =>
			expect((screen.getByTestId('outputs-adult') as HTMLInputElement).checked).toBe(true)
		);
		await fireEvent.click(screen.getByTestId('outputs-adult'));
		await until(() =>
			expect(writes()[1]).toEqual({
				method: 'PATCH',
				path: '/settings/content',
				body: { export_adult: false }
			})
		);
		// Turning it off asks nothing.
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(writes()).toHaveLength(2);
	});

	it('shows the export job from start to a capped end', async () => {
		render(OutputsScreen);
		const link = screen.getByTestId('outputs-bitmagnet-start');
		// jsdom doesn't download; stop it navigating.
		link.addEventListener('click', (e) => e.preventDefault());
		const now = Math.floor(Date.now() / 1000);
		const job = (over: object) => ({
			id: 4,
			kind: 'export',
			state: 'running',
			started_at: now,
			finished_at: null,
			done: 1200,
			skipped: 3,
			total: 5000,
			total_approx: false,
			rate: null,
			eta_s: null,
			export: { cap: 1_000_000, format: 'bitmagnet', truncated: false },
			...over
		});
		jobs = [job({})];
		await fireEvent.click(link);
		await until(() =>
			expect(screen.getByTestId('outputs-bitmagnet-count').textContent).toContain(
				'Exported 1,200 of 5,000 torrents'
			)
		);
		jobs = [
			job({
				state: 'done',
				finished_at: now + 1,
				done: 1_000_000,
				export: { cap: 1_000_000, format: 'bitmagnet', truncated: true }
			})
		];
		await until(() =>
			expect(screen.getByTestId('outputs-bitmagnet-truncated').textContent).toContain(
				'limit of 1,000,000 torrents'
			)
		);
		expect(screen.getByTestId('outputs-bitmagnet-skipped').textContent).toContain('3 left out');
	});
});
