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
	heldCount = 0;
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

/** A running bitmagnet export job started now, as the server lists it. */
const exportJob = (id: number, over: Record<string, unknown> = {}) => ({
	id,
	kind: 'export',
	state: 'running',
	started_at: Math.floor(Date.now() / 1000),
	finished_at: null,
	done: 0,
	skipped: 0,
	total: null,
	total_approx: false,
	rate: null,
	eta_s: null,
	export: { cap: 1_000_000, format: 'bitmagnet', truncated: false },
	...over
});

let heldCount = 0;
/** Holds the next GET of `path` until the returned function runs; its answer is read then. */
function hold(path: string): () => void {
	let release!: () => void;
	const gate = new Promise<void>((done) => (release = done));
	const stub = globalThis.fetch as unknown as import('vitest').Mock;
	const inner = stub.getMockImplementation()!;
	let taken = false;
	stub.mockImplementation(async (url: string, init: RequestInit = {}) => {
		const at = new URL(url, 'http://x').pathname.replace('/api/v1', '');
		if (!taken && (init.method ?? 'GET') === 'GET' && at === path) {
			taken = true;
			heldCount++;
			await gate;
		}
		return inner(url, init);
	});
	return release;
}
/** How many replies `hold` has taken so far. */
const held = () => heldCount;

async function password(name: RegExp) {
	const dialog = await screen.findByRole('dialog', { name }, { timeout: 10_000 });
	return dialog;
}

describe('Torznab', () => {
	it('shows the address and only the keys with the torznab scope', async () => {
		render(OutputsScreen);
		expect((screen.getByTestId('outputs-torznab-url') as HTMLInputElement).value).toBe(
			`${location.origin}/api/v1/torznab`
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

	it('Revoke asks first: Keep sends nothing, Revoke revokes', async () => {
		replies['POST /keys/1/revoke'] = () => ({
			status: 200,
			body: { ...key(1, 'Prowlarr', ['torznab'], 'revoked'), revoked_at: 1_700_000_500 }
		});
		render(OutputsScreen);
		await screen.findAllByTestId('outputs-key');
		const revoke = () =>
			fireEvent.click(screen.getByRole('button', { name: 'Revoke the key for Prowlarr' }));
		await revoke();
		let dialog = await screen.findByRole(
			'dialog',
			{ name: 'Revoke the key for Prowlarr?' },
			{ timeout: 10_000 }
		);
		expect(dialog.textContent).toContain('Prowlarr stops finding anything in Lodestar');
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Keep key' }));
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(writes()).toEqual([]);
		await revoke();
		dialog = await screen.findByRole(
			'dialog',
			{ name: 'Revoke the key for Prowlarr?' },
			{ timeout: 10_000 }
		);
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Revoke key' }));
		await until(() =>
			expect(writes()).toEqual([{ method: 'POST', path: '/keys/1/revoke', body: {} }])
		);
		const row = (await screen.findAllByTestId('outputs-key'))[0]!;
		await until(() => expect(row.textContent).toContain('Revoked'));
		// A revoked key has nothing left to press.
		expect(within(row).queryByRole('button')).toBeNull();
	});

	it("shows the server's refusal of a name beside the field", async () => {
		replies['POST /keys'] = () => ({
			status: 409,
			body: { error: { code: 'name_taken', message: 'A key has that name.', field: 'name' } }
		});
		render(OutputsScreen);
		await screen.findAllByTestId('outputs-key');
		await fireEvent.click(screen.getByTestId('outputs-key-create'));
		const dialog = await password(/Create a Torznab key for Sonarr/);
		await fireEvent.input(within(dialog).getByLabelText(/Your password/), {
			target: { value: 'pw' }
		});
		await fireEvent.click(within(dialog).getByTestId('password-dialog-confirm'));
		expect(await screen.findByText('A key has that name.')).toBeTruthy();
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(document.body.innerHTML).not.toContain(SECRET);
	});

	it('refuses a name over 200 characters without asking for the password', async () => {
		render(OutputsScreen);
		await screen.findAllByTestId('outputs-key');
		await fireEvent.input(screen.getByRole('textbox', { name: /App/ }), {
			target: { value: 'x'.repeat(201) }
		});
		await fireEvent.click(screen.getByTestId('outputs-key-create'));
		expect(await screen.findByText('Use at most 200 characters.')).toBeTruthy();
		expect(screen.queryByRole('dialog')).toBeNull();
		expect(writes()).toEqual([]);
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

	it('a refused change says so and the switch shows the server value', async () => {
		replies['PATCH /settings/content'] = () => ({
			status: 500,
			body: { error: { code: 'internal', message: 'x' } }
		});
		render(OutputsScreen);
		await fireEvent.click(await screen.findByTestId('outputs-adult'));
		const dialog = await screen.findByRole(
			'dialog',
			{ name: 'Include adult torrents in exports?' },
			{ timeout: 10_000 }
		);
		await fireEvent.click(within(dialog).getByTestId('outputs-adult-include'));
		expect(await screen.findByText("Couldn't change this setting. Try again.")).toBeTruthy();
		await until(() =>
			expect((screen.getByTestId('outputs-adult') as HTMLInputElement).checked).toBe(false)
		);
	});

	it('a setting locked by the environment shows locked, and sends nothing', async () => {
		replies['GET /settings/content'] = () => ({
			status: 200,
			body: {
				...CONTENT(false),
				fields: {
					export_adult: {
						value: false,
						env: 'LODESTAR_EXPORT_ADULT',
						read_only: true,
						source: 'env'
					}
				}
			}
		});
		render(OutputsScreen);
		const toggle = (await screen.findByTestId('outputs-adult')) as HTMLInputElement;
		expect(toggle.disabled).toBe(true);
		expect(screen.getByText(/Set by LODESTAR_EXPORT_ADULT/)).toBeTruthy();
		expect(writes()).toEqual([]);
	});

	it('the switch waits while its change is saving', async () => {
		let release!: () => void;
		const held = new Promise<void>((done) => (release = done));
		const fetchStub = globalThis.fetch as unknown as import('vitest').Mock;
		const inner = fetchStub.getMockImplementation()!;
		fetchStub.mockImplementation(async (url: string, init: RequestInit = {}) => {
			if (init.method === 'PATCH') await held;
			return inner(url, init);
		});
		render(OutputsScreen);
		await fireEvent.click(await screen.findByTestId('outputs-adult'));
		const dialog = await screen.findByRole(
			'dialog',
			{ name: 'Include adult torrents in exports?' },
			{ timeout: 10_000 }
		);
		await fireEvent.click(within(dialog).getByTestId('outputs-adult-include'));
		await until(() =>
			expect((screen.getByTestId('outputs-adult') as HTMLInputElement).disabled).toBe(true)
		);
		release();
		await until(() =>
			expect((screen.getByTestId('outputs-adult') as HTMLInputElement).disabled).toBe(false)
		);
		expect((screen.getByTestId('outputs-adult') as HTMLInputElement).checked).toBe(true);
	});

	it('never shows an export that finished before the click as the new one', async () => {
		const now = Math.floor(Date.now() / 1000);
		const old = {
			id: 3,
			kind: 'export',
			state: 'done',
			// The server's clock runs ahead: the old export looks as if it started after the click.
			started_at: now + 60,
			finished_at: now + 61,
			done: 7,
			skipped: 0,
			total: 7,
			total_approx: false,
			rate: null,
			eta_s: null,
			export: { cap: 1_000_000, format: 'bitmagnet', truncated: false }
		};
		jobs = [old];
		render(OutputsScreen);
		const link = screen.getByTestId('outputs-bitmagnet-start');
		await until(() => expect(link.dataset.idsKnown).toBe('true'));
		link.addEventListener('click', (e) => e.preventDefault());
		await fireEvent.click(link);
		const polls = () => calls.filter((c) => c.path === '/jobs').length;
		await until(() => expect(polls()).toBeGreaterThanOrEqual(3));
		expect(screen.getByTestId('outputs-bitmagnet-progress').textContent).toContain(
			'Starting the export'
		);
		jobs = [{ ...old, id: 4, state: 'running', finished_at: null, done: 2, total: 9 }, old];
		await until(() =>
			expect(screen.getByTestId('outputs-bitmagnet-count').textContent).toContain(
				'Exported 2 of 9 torrents'
			)
		);
	});

	it('keeps the last progress when /jobs stops answering after the job was found', async () => {
		render(OutputsScreen);
		const link = screen.getByTestId('outputs-bitmagnet-start');
		await until(() => expect(link.dataset.idsKnown).toBe('true'));
		link.addEventListener('click', (e) => e.preventDefault());
		jobs = [
			{
				id: 1,
				kind: 'export',
				state: 'running',
				started_at: 0,
				finished_at: null,
				done: 5,
				skipped: 0,
				total: 10,
				total_approx: false,
				rate: null,
				eta_s: null,
				export: { cap: 1_000_000, format: 'bitmagnet', truncated: false }
			}
		];
		await fireEvent.click(link);
		await until(() =>
			expect(screen.getByTestId('outputs-bitmagnet-count').textContent).toContain(
				'Exported 5 of 10'
			)
		);
		replies['GET /jobs'] = () => ({ status: 500, body: { error: { code: 'x', message: 'x' } } });
		await until(() => expect(screen.getByTestId('outputs-bitmagnet-stale')).toBeTruthy());
		expect(screen.getByTestId('outputs-bitmagnet-count').textContent).toContain('Exported 5 of 10');
		expect(screen.getByTestId('outputs-bitmagnet-progress').textContent).not.toContain(
			"couldn't find"
		);
	});

	it('a press before the first /jobs read answers still finds its job', async () => {
		// The server creates the export's job before it answers the read the screen sent when it
		// opened, so that answer already holds the new job: ids can't tell it apart, the clock can.
		const release = hold('/jobs');
		render(OutputsScreen);
		const link = screen.getByTestId('outputs-bitmagnet-start');
		link.addEventListener('click', (e) => e.preventDefault());
		await until(() => expect(held()).toBe(1));
		await fireEvent.click(link);
		jobs = [exportJob(1, { done: 3, total: 10 })];
		release();
		await until(() =>
			expect(screen.getByTestId('outputs-bitmagnet-count').textContent).toContain(
				'Exported 3 of 10'
			)
		);
	});

	it("a second press never shows the first press's answer", async () => {
		render(OutputsScreen);
		const link = screen.getByTestId('outputs-bitmagnet-start');
		await until(() => expect(link.dataset.idsKnown).toBe('true'));
		link.addEventListener('click', (e) => e.preventDefault());
		const region = screen.getByTestId('outputs-bitmagnet-progress');
		// The first press's poll is in flight when the second press comes.
		const release = hold('/jobs');
		jobs = [exportJob(1, { done: 1, total: 10 })];
		await fireEvent.click(link);
		await until(() => expect(held()).toBe(1));
		const seenTexts: string[] = [];
		const watch = new MutationObserver(() => seenTexts.push(region.textContent ?? ''));
		watch.observe(region, { subtree: true, childList: true, characterData: true });
		await fireEvent.click(link);
		// The first press's poll answers now, with its own job: the second press must ignore it.
		const answered = calls.filter((c) => c.path === '/jobs').length;
		release();
		await until(() =>
			expect(calls.filter((c) => c.path === '/jobs').length).toBeGreaterThan(answered)
		);
		jobs = [exportJob(2, { done: 7, total: 10 }), exportJob(1, { done: 1, total: 10 })];
		await until(() => expect(region.textContent).toContain('Exported 7 of 10'));
		watch.disconnect();
		expect(seenTexts.filter((t) => t.includes('Exported 1 of 10'))).toEqual([]);
	});

	it("a later press doesn't take an earlier press's job that is still running", async () => {
		render(OutputsScreen);
		const link = screen.getByTestId('outputs-bitmagnet-start');
		await until(() => expect(link.dataset.idsKnown).toBe('true'));
		link.addEventListener('click', (e) => e.preventDefault());
		const region = screen.getByTestId('outputs-bitmagnet-progress');
		jobs = [exportJob(1, { done: 4, total: 10 })];
		await fireEvent.click(link);
		await until(() => expect(region.textContent).toContain('Exported 4 of 10'));
		const seenTexts: string[] = [];
		const watch = new MutationObserver(() => seenTexts.push(region.textContent ?? ''));
		watch.observe(region, { subtree: true, childList: true, characterData: true });
		await fireEvent.click(link);
		const polls = calls.filter((c) => c.path === '/jobs').length;
		await until(() =>
			expect(calls.filter((c) => c.path === '/jobs').length).toBeGreaterThan(polls + 1)
		);
		expect(region.textContent).toContain('Starting the export');
		jobs = [exportJob(2, { done: 2, total: 9 }), exportJob(1, { done: 5, total: 10 })];
		await until(() => expect(region.textContent).toContain('Exported 2 of 9'));
		watch.disconnect();
		expect(seenTexts.filter((t) => /Exported [45] of 10/.test(t))).toEqual([]);
	});

	it("an older press's failed reply doesn't cost the newer press its retries", async () => {
		render(OutputsScreen);
		const link = screen.getByTestId('outputs-bitmagnet-start');
		await until(() => expect(link.dataset.idsKnown).toBe('true'));
		link.addEventListener('click', (e) => e.preventDefault());
		const release = hold('/jobs');
		await fireEvent.click(link);
		await until(() => expect(held()).toBe(1));
		await fireEvent.click(link);
		const before = calls.filter((c) => c.path === '/jobs').length;
		// The first press's reply comes back as a failure, and so do the second press's.
		replies['GET /jobs'] = () => ({ status: 500, body: { error: { code: 'x', message: 'x' } } });
		release();
		await until(() =>
			expect(screen.getByTestId('outputs-bitmagnet-progress').textContent).toContain(
				"couldn't find this export's progress"
			)
		);
		// The released reply, then the second press's own three tries.
		expect(calls.filter((c) => c.path === '/jobs').length - before).toBe(1 + 3);
	});

	it('an empty /jobs list is no restart: the press keeps matching by id', async () => {
		const now = Math.floor(Date.now() / 1000);
		jobs = [exportJob(5, { state: 'done', finished_at: now, done: 9, total: 9 })];
		render(OutputsScreen);
		const link = screen.getByTestId('outputs-bitmagnet-start');
		await until(() => expect(link.dataset.idsKnown).toBe('true'));
		link.addEventListener('click', (e) => e.preventDefault());
		const region = screen.getByTestId('outputs-bitmagnet-progress');
		const seenTexts: string[] = [];
		const watch = new MutationObserver(() => seenTexts.push(region.textContent ?? ''));
		watch.observe(region, { subtree: true, childList: true, characterData: true });
		// The old job has expired from the list, and the new one isn't listed yet.
		jobs = [];
		await fireEvent.click(link);
		const polls = calls.filter((c) => c.path === '/jobs').length;
		await until(() =>
			expect(calls.filter((c) => c.path === '/jobs').length).toBeGreaterThan(polls)
		);
		// The old job shows up again, started "now" by the clock: ids still say it isn't ours.
		jobs = [exportJob(5, { state: 'done', finished_at: now, done: 9, total: 9 })];
		const again = calls.filter((c) => c.path === '/jobs').length;
		await until(() =>
			expect(calls.filter((c) => c.path === '/jobs').length).toBeGreaterThan(again)
		);
		jobs = [exportJob(6, { done: 1, total: 4 }), ...jobs];
		await until(() => expect(region.textContent).toContain('Exported 1 of 4'));
		watch.disconnect();
		expect(seenTexts.filter((t) => t.includes('Done: 9'))).toEqual([]);
	});

	it('finds the job after a server restart reset the ids', async () => {
		jobs = [exportJob(5, { state: 'done', finished_at: 1, done: 9, total: 9 })];
		render(OutputsScreen);
		const link = screen.getByTestId('outputs-bitmagnet-start');
		await until(() => expect(link.dataset.idsKnown).toBe('true'));
		link.addEventListener('click', (e) => e.preventDefault());
		// Restarted: the old jobs are gone and ids begin again at 1.
		jobs = [exportJob(1, { done: 6, total: 8 })];
		await fireEvent.click(link);
		await until(() =>
			expect(screen.getByTestId('outputs-bitmagnet-count').textContent).toContain('Exported 6 of 8')
		);
	});

	it('says a capped file is incomplete inside the announced region', async () => {
		render(OutputsScreen);
		const link = screen.getByTestId('outputs-bitmagnet-start');
		await until(() => expect(link.dataset.idsKnown).toBe('true'));
		link.addEventListener('click', (e) => e.preventDefault());
		jobs = [
			exportJob(1, {
				state: 'done',
				finished_at: Math.floor(Date.now() / 1000),
				done: 1_000_000,
				total: 1_200_000,
				export: { cap: 1_000_000, format: 'bitmagnet', truncated: true }
			})
		];
		await fireEvent.click(link);
		const region = screen.getByTestId('outputs-bitmagnet-progress');
		await until(() =>
			expect(within(region).getByTestId('outputs-bitmagnet-truncated').textContent).toContain(
				"Warning: The file stopped at the limit of 1,000,000 torrents, so it isn't complete."
			)
		);
	});

	it('an unreadable setting shows no switch, and Try again reads it', async () => {
		replies['GET /settings/content'] = () => ({
			status: 500,
			body: { error: { code: 'internal', message: 'x' } }
		});
		render(OutputsScreen);
		expect(await screen.findByText("Couldn't load this setting.")).toBeTruthy();
		expect(screen.queryByTestId('outputs-adult')).toBeNull();
		replies['GET /settings/content'] = () => ({ status: 200, body: CONTENT(true) });
		await fireEvent.click(
			within(screen.getByTestId('outputs-bitmagnet')).getByRole('button', { name: 'Try again' })
		);
		await until(() =>
			expect((screen.getByTestId('outputs-adult') as HTMLInputElement).checked).toBe(true)
		);
	});

	it('says the progress is lost when /jobs keeps failing', async () => {
		replies['GET /jobs'] = () => ({ status: 500, body: { error: { code: 'x', message: 'x' } } });
		render(OutputsScreen);
		const link = screen.getByTestId('outputs-bitmagnet-start');
		link.addEventListener('click', (e) => e.preventDefault());
		await fireEvent.click(link);
		await until(() =>
			expect(screen.getByTestId('outputs-bitmagnet-progress').textContent).toContain(
				"couldn't find this export's progress"
			)
		);
		// The read at mount, then three tries before giving up.
		expect(calls.filter((c) => c.path === '/jobs').length).toBe(4);
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
