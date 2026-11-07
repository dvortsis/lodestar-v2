import { afterEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { mockFetch } from '../filters/flows/testing/fetch';
import SourcesScreen from './SourcesScreen.svelte';
import { approved } from './consent-approved.fixture';
import { darknet, dht, lime, nyaa, source, sourceList, tracker } from './fixtures';

configure({ defaultHidden: true, asyncUtilTimeout: 10_000 });
vi.setConfig({ testTimeout: 30_000 });

const GATE =
	"this source asks for your consent before it's turned on, and this version of Lodestar can't show you an approved consent text yet, so it stays off. It can be turned on in a later version";

let net: ReturnType<typeof mockFetch>;
function setup(...list: ReturnType<typeof source>[]) {
	net = mockFetch();
	net.on('GET /sources', { body: sourceList(dht, ...list) });
	return render(SourcesScreen);
}
const writes = () => net.sent.filter((s) => s.method !== 'GET');
const bodyOf = (i = 0) => JSON.parse(writes()[i]!.body!);
const toggleOf = (id: number) =>
	within(screen.getByTestId(`source-${id}`)).getByRole('switch') as HTMLInputElement;
async function open(id: number) {
	await screen.findByTestId(`source-${id}`);
	await fireEvent.click(toggleOf(id));
	return screen.findByRole('dialog');
}
afterEach(() => vi.unstubAllGlobals());

describe('consent dialogs: the approved texts', () => {
	const fill = (
		t: string,
		s: ReturnType<typeof source>,
		vals: Record<string, string | undefined>
	) =>
		t
			.replaceAll('{name}', s.name)
			.replace(/\{(\w+)\}/g, (_, k: string) => vals[k] ?? `{${k}}`)
			.replaceAll('**', '')
			.replace(/\s+/g, ' ')
			.trim();
	const shown = (dlg: HTMLElement) => {
		const title = document.getElementById(dlg.getAttribute('aria-labelledby')!)!.textContent!;
		const text = [...screen.getByTestId('consent-text').children].map((el) =>
			el.tagName === 'UL' ? [...el.children].map((li) => li.textContent!) : [el.textContent!]
		);
		const buttons = [screen.getByTestId('consent-confirm'), screen.getByTestId('consent-cancel')];
		return {
			lines: [title, ...text.flat()].map((t) => t.replace(/\s+/g, ' ').trim()),
			buttons: buttons.map((b) => b.textContent!.replace(/\s+/g, ' ').trim())
		};
	};
	const cases = [
		{
			cls: 'external' as const,
			s: { ...nyaa, endpoints: ['https://nyaa.example/a', 'http://api.nyaa.example/~me/b'] },
			vals: {
				hosts: 'https://nyaa.example/a, http://api.nyaa.example/~me/b',
				egress: 'direct connection'
			}
		},
		{
			cls: 'account' as const,
			s: tracker,
			vals: { hosts: 'https://tracker.example/api, https://tracker.example/rss' }
		},
		{ cls: 'restricted' as const, s: lime, vals: {} },
		{ cls: 'overlay' as const, s: darknet, vals: { network: 'I2P' } }
	];
	for (const { cls, s, vals } of cases) {
		it(`${cls}: shows exactly the approved text and buttons`, async () => {
			setup(s);
			const dlg = await open(s.id);
			const got = shown(dlg);
			const want = approved[cls];
			expect(got.lines).toEqual(want.lines.map((t) => fill(t, s, vals)));
			expect(got.buttons).toEqual(want.buttons.map((t) => fill(t, s, vals)));
		});
	}

	it('shows the full addresses the acknowledgement covers, so a changed path is seen', async () => {
		const a = source({ ...nyaa, endpoints: ['https://cache.example/~me/{hash}.torrent'] });
		setup(a);
		await open(1);
		const code = () =>
			[...screen.getByTestId('consent-text').querySelectorAll('code')].map((c) => c.textContent);
		expect(code()).toEqual(['https://cache.example/~me/{hash}.torrent']);
	});

	it('sets an address with bold marks literally and never turns it bold', async () => {
		setup(source({ ...nyaa, endpoints: ['https://x.example/**a**/b'] }));
		await open(1);
		const text = screen.getByTestId('consent-text');
		expect(text.querySelector('code')!.textContent).toBe('https://x.example/**a**/b');
		expect(text.querySelectorAll('strong')).toHaveLength(0);
	});

	it('says so when no address is listed', async () => {
		setup(source({ ...nyaa, endpoints: [] }));
		await open(1);
		const p = screen.getByTestId('consent-text').textContent!;
		expect(p).toContain('on your behalf: No address is listed yet. They can see');
	});

	it('keeps the bold the approved text marks', async () => {
		setup(lime);
		await open(3);
		const bold = [...screen.getByTestId('consent-text').querySelectorAll('strong')].map(
			(b) => b.textContent
		);
		expect(bold).toEqual(['Use a VPN.']);
	});
});

describe('consent dialogs: what is sent', () => {
	it('External: acknowledges with the version the server gave; the server answer decides', async () => {
		const on = { ...nyaa, enabled: true, state: 'enabled' as const, needs: null };
		setup(nyaa);
		net.on('PATCH /sources/{id}', { body: on });
		await open(1);
		await fireEvent.click(screen.getByTestId('consent-confirm'));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(bodyOf()).toEqual({ enabled: true, consent: 'external', consent_version: 'v1' });
		expect(toggleOf(1).checked).toBe(true);
	});

	it('shows the consent gate refusal plainly and leaves the source off', async () => {
		setup(nyaa);
		net.on('PATCH /sources/{id}', {
			status: 400,
			body: {
				error: {
					code: 'invalid_setting',
					field: 'enabled',
					message: `The setting wasn't saved: ${GATE}.`
				}
			}
		});
		await open(1);
		await fireEvent.click(screen.getByTestId('consent-confirm'));
		const p = await screen.findByTestId('consent-problem');
		expect(p.textContent!.replace(/\s+/g, ' ')).toContain(
			`The setting wasn't saved: ${GATE}. Nothing was changed.`
		);
		expect(screen.getByRole('dialog')).toBeTruthy();
		expect(toggleOf(1).checked).toBe(false);
		expect(writes()).toHaveLength(1);
	});

	it('Restricted: focus starts on Cancel, the password is required and sent, Escape cancels', async () => {
		setup(lime);
		net.on('PATCH /sources/{id}', { body: { ...lime, enabled: true, needs: null } });
		const dlg = await open(3);
		await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId('consent-cancel')));
		await fireEvent.click(screen.getByTestId('consent-confirm'));
		expect(within(dlg).getByText('Enter your password.')).toBeTruthy();
		expect(writes()).toHaveLength(0);
		const pw = screen.getByTestId('consent-password') as HTMLInputElement;
		expect(pw.type).toBe('password');
		await fireEvent.input(pw, { target: { value: 'hunter2hunter2' } });
		await fireEvent.click(screen.getByTestId('consent-confirm'));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(bodyOf()).toEqual({
			enabled: true,
			consent: 'restricted',
			consent_version: 'v1-ab',
			password: 'hunter2hunter2'
		});
		expect(writes()[0]!.url).not.toContain('hunter2');
	});

	it('a wrong password is said plainly, the field is cleared and the dialog stays', async () => {
		setup(darknet);
		net.error('PATCH /sources/{id}', 401, 'bad_password');
		await open(4);
		const pw = screen.getByTestId('consent-password') as HTMLInputElement;
		await fireEvent.input(pw, { target: { value: 'nope-nope' } });
		await fireEvent.click(screen.getByTestId('consent-confirm'));
		expect((await screen.findByTestId('consent-problem')).textContent).toContain(
			'password is wrong'
		);
		expect(pw.value).toBe('');
		expect(toggleOf(4).checked).toBe(false);
	});

	it('Escape closes without sending anything', async () => {
		setup(darknet);
		const dlg = await open(4);
		await fireEvent.keyDown(dlg, { key: 'Escape' });
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(writes()).toHaveLength(0);
	});
});

describe('consent dialogs: other refusals', () => {
	it('an expired session is not called a wrong password', async () => {
		setup(darknet);
		net.error('PATCH /sources/{id}', 401, 'login_required');
		await open(4);
		await fireEvent.input(screen.getByTestId('consent-password'), {
			target: { value: 'some-pass' }
		});
		await fireEvent.click(screen.getByTestId('consent-confirm'));
		const p = await screen.findByTestId('consent-problem');
		expect(p.textContent).not.toContain('password is wrong');
	});

	it('a changed notice reloads the list so the next try sends the new version', async () => {
		setup(lime);
		net.on('PATCH /sources/{id}', {
			status: 409,
			body: { error: { code: 'consent_required', message: 'x' } }
		});
		await open(3);
		await fireEvent.input(screen.getByTestId('consent-password'), {
			target: { value: 'some-pass' }
		});
		const before = net.sent.filter((s) => s.method === 'GET').length;
		await fireEvent.click(screen.getByTestId('consent-confirm'));
		expect((await screen.findByTestId('consent-problem')).textContent).toContain('changed');
		await waitFor(() =>
			expect(net.sent.filter((s) => s.method === 'GET').length).toBeGreaterThan(before)
		);
		// Confirm stays off until the dialog is reopened, so the new notice is read first.
		const confirm = screen.getByTestId('consent-confirm') as HTMLButtonElement;
		expect(confirm.disabled).toBe(true);
		await fireEvent.click(confirm);
		expect(writes()).toHaveLength(1);
		await fireEvent.click(screen.getByTestId('consent-cancel'));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		await fireEvent.click(toggleOf(3));
		await screen.findByRole('dialog');
		expect((screen.getByTestId('consent-confirm') as HTMLButtonElement).disabled).toBe(false);
	});

	it('Overlay: keeps its bold and starts on Cancel', async () => {
		setup(darknet);
		await open(4);
		const bold = [...screen.getByTestId('consent-text').querySelectorAll('strong')].map(
			(b) => b.textContent
		);
		expect(bold).toEqual(['Use a VPN', 'Purge']);
		await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId('consent-cancel')));
	});

	it('Restricted: Escape cancels without sending anything', async () => {
		setup(lime);
		const dlg = await open(3);
		await fireEvent.keyDown(dlg, { key: 'Escape' });
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(writes()).toHaveLength(0);
	});
});

describe('account sources (SRC-15c)', () => {
	const KEY = 'sup3r-secret-key-value';
	const withSpec = source({
		...tracker,
		credential_spec: [
			{ name: 'username', kind: 'username', label: 'Username', required: true },
			{ name: 'api_key', kind: 'secret', label: 'API key', required: true }
		]
	});
	const type = async (tid: string, value: string) =>
		fireEvent.input(await screen.findByTestId(tid), { target: { value } });

	it('draws a field per credential_spec: secrets masked, usernames not, nothing prefilled', async () => {
		setup(withSpec);
		await open(5);
		const user = (await screen.findByTestId('credential-username')) as HTMLInputElement;
		const key = screen.getByTestId('credential-api_key') as HTMLInputElement;
		expect(user.type).toBe('text');
		expect(key.type).toBe('password');
		expect([user.value, key.value]).toEqual(['', '']);
	});

	it('saves credentials, clears them, and shows the server refusal plainly', async () => {
		setup(withSpec);
		net.on('PATCH /sources/{id}/credentials', {
			body: { ...withSpec, credentials: ['username', 'api_key'] }
		});
		net.error('PATCH /sources/{id}', 400, 'invalid_setting', {
			message: 'this source asks for your consent before it is turned on, so it stays off.'
		});
		await open(5);
		await type('credential-username', 'me');
		await type('credential-api_key', KEY);
		await fireEvent.click(screen.getByTestId('consent-confirm'));
		const problem = await screen.findByTestId('consent-problem');
		expect(problem.textContent).toContain('stays off');
		expect(problem.textContent).not.toContain(KEY);
		expect(bodyOf(0)).toEqual({ credentials: { username: 'me', api_key: KEY } });
		expect(bodyOf(1)).toEqual({ enabled: true });
		expect((screen.getByTestId('credential-api_key') as HTMLInputElement).value).toBe('');
		expect(document.body.textContent).not.toContain(KEY);
		expect(problem.textContent).not.toContain('Nothing was changed');
		expect((screen.getByTestId('credential-api_key') as HTMLInputElement).autocomplete).toBe(
			'new-password'
		);
		expect(toggleOf(5).checked).toBe(false);
	});

	it('clears the secret when saving it fails, and never echoes it', async () => {
		setup(withSpec);
		net.error('PATCH /sources/{id}/credentials', 400, 'invalid_credentials', {
			message: 'api_key is too long.'
		});
		await open(5);
		await type('credential-username', 'me');
		await type('credential-api_key', KEY);
		await fireEvent.click(screen.getByTestId('consent-confirm'));
		const problem = await screen.findByTestId('consent-problem');
		expect(problem.textContent).not.toContain(KEY);
		expect((screen.getByTestId('credential-api_key') as HTMLInputElement).value).toBe('');
		expect(writes()).toHaveLength(1);
	});

	it('asks for missing required fields and sends nothing', async () => {
		setup(withSpec);
		await open(5);
		await fireEvent.click(screen.getByTestId('consent-confirm'));
		await waitFor(() => expect(screen.getByRole('dialog').textContent).toContain('Enter this'));
		expect(writes()).toHaveLength(0);
	});

	it('a stored credential is not asked for again; Remove asks first, with focus on Keep', async () => {
		setup(source({ ...withSpec, credentials: ['username', 'api_key'] }));
		net.on('PATCH /sources/{id}/credentials', { body: withSpec });
		await open(5);
		await fireEvent.click(await screen.findByTestId('credential-api_key-remove'));
		const keep = await screen.findByTestId('credential-api_key-keep');
		await waitFor(() => expect(document.activeElement).toBe(keep));
		await fireEvent.click(keep);
		expect(writes()).toHaveLength(0);
		await fireEvent.click(await screen.findByTestId('credential-api_key-remove'));
		await fireEvent.click(await screen.findByTestId('credential-api_key-remove-yes'));
		await waitFor(() => expect(writes()).toHaveLength(1));
		expect(bodyOf(0)).toEqual({ credentials: { api_key: null } });
	});

	it('announces missing required fields and focuses the first', async () => {
		setup(withSpec);
		await open(5);
		await fireEvent.click(screen.getByTestId('consent-confirm'));
		expect((await screen.findByTestId('consent-problem')).textContent).toContain('marked fields');
		await waitFor(() =>
			expect(document.activeElement).toBe(screen.getByTestId('credential-username'))
		);
	});

	it('turns Confirm off when the server says the notice changed', async () => {
		setup(withSpec);
		net.on('PATCH /sources/{id}/credentials', { body: withSpec });
		net.error('PATCH /sources/{id}', 400, 'consent_required');
		await open(5);
		await type('credential-username', 'me');
		await type('credential-api_key', KEY);
		await fireEvent.click(screen.getByTestId('consent-confirm'));
		await screen.findByTestId('consent-problem');
		await waitFor(() =>
			expect((screen.getByTestId('consent-confirm') as HTMLButtonElement).disabled).toBe(true)
		);
	});

	it('Escape closes it without sending anything', async () => {
		setup(withSpec);
		const dlg = await open(5);
		await fireEvent.keyDown(dlg, { key: 'Escape' });
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(writes()).toHaveLength(0);
	});
});

describe('purge', () => {
	const job = (state: string, over: object = {}) => ({
		id: 7,
		kind: 'purge',
		state,
		done: 0,
		skipped: 0,
		total: null,
		total_approx: false,
		eta_s: null,
		rate: null,
		started_at: 1,
		finished_at: state === 'running' ? null : 2,
		purge: {
			kept: 5,
			keys_dropped: 0,
			plugin: 'x',
			snapshots_left: 0,
			snapshots_scrubbed: 0,
			source_id: 1,
			stage: 'removing',
			torrents_deleted: 1204,
			unverified_deleted: 12,
			...over
		}
	});

	const preview = {
		claims_deleted: 96402,
		kept: 88120,
		plugin: 'x',
		queue_deleted: 0,
		settled: true,
		source_id: 1,
		state_deleted: 0,
		torrents_deleted: 1204331,
		unverified_deleted: 5210,
		unverified_kept: 0
	};

	it('shows the counts before the password, from a call that sends no body', async () => {
		setup(nyaa);
		net.on('GET /sources/{id}/purge/preview', { body: preview });
		await screen.findByTestId('source-1');
		await fireEvent.click(screen.getByTestId('source-1-purge'));
		expect((await screen.findByTestId('preview-torrents')).textContent).toContain('1,204,331');
		expect(screen.getByTestId('preview-claims').textContent).toContain('96,402');
		expect(screen.getByTestId('preview-unverified').textContent).toContain('5,210');
		expect(screen.getByTestId('preview-kept').textContent).toContain('88,120');
		expect(screen.queryByTestId('preview-unsettled')).toBeNull();
		expect(writes()).toHaveLength(0);
		const sent = net.sent.find((x) => x.url.includes('/purge/preview'))!;
		expect(sent.body ?? '').toBe('');
	});

	it('names the count on the purge button once it has loaded', async () => {
		setup(nyaa);
		net.on('GET /sources/{id}/purge/preview', { body: preview });
		await screen.findByTestId('source-1');
		await fireEvent.click(screen.getByTestId('source-1-purge'));
		await waitFor(() =>
			expect(screen.getByTestId('purge-confirm').textContent).toContain(
				'Delete 1,204,331 torrents found only by Nyaa'
			)
		);
	});

	it('says the counts can grow while the source is on', async () => {
		setup(nyaa);
		net.on('GET /sources/{id}/purge/preview', { body: { ...preview, settled: false } });
		await screen.findByTestId('source-1');
		await fireEvent.click(screen.getByTestId('source-1-purge'));
		expect((await screen.findByTestId('preview-unsettled')).textContent).toContain('can grow');
	});

	it('shows a failed count with a retry, and the purge stays possible', async () => {
		setup(nyaa);
		net.error('GET /sources/{id}/purge/preview', 500, 'internal');
		await screen.findByTestId('source-1');
		await fireEvent.click(screen.getByTestId('source-1-purge'));
		const retry = await screen.findByTestId('purge-preview-retry');
		expect(screen.getByTestId('purge-confirm')).toBeTruthy();
		net.on('GET /sources/{id}/purge/preview', { body: preview });
		await fireEvent.click(retry);
		await screen.findByTestId('preview-torrents');
	});

	it('names what is deleted, starts on Cancel, sends nothing until confirmed', async () => {
		setup(nyaa);
		await screen.findByTestId('source-1');
		await fireEvent.click(screen.getByTestId('source-1-purge'));
		await screen.findByTestId('purge-text');
		const t = screen.getByTestId('purge-text').textContent!;
		for (const part of [
			'claims',
			'unverified items',
			'credentials',
			'consent record',
			"can't be undone"
		])
			expect(t).toContain(part);
		await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId('purge-cancel')));
		await fireEvent.click(screen.getByTestId('purge-confirm'));
		expect(writes()).toHaveLength(0);
		await fireEvent.click(screen.getByTestId('purge-cancel'));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(writes()).toHaveLength(0);
	});

	it('sends the password, shows progress, then the result', async () => {
		setup(nyaa);
		net.on('POST /sources/{id}/purge', { status: 202, body: job('running') });
		net.on('GET /jobs/{id}', { body: job('running') }, { body: job('done', { stage: 'purged' }) });
		await screen.findByTestId('source-1');
		await fireEvent.click(screen.getByTestId('source-1-purge'));
		await fireEvent.input(await screen.findByTestId('purge-password'), {
			target: { value: 'hunter2hunter2' }
		});
		await fireEvent.click(screen.getByTestId('purge-confirm'));
		await screen.findByTestId('purge-stage');
		expect(screen.getByTestId('purge-status').textContent).toContain('Purging Nyaa');
		const r = await screen.findByTestId('purge-result');
		expect(screen.getByTestId('purge-torrents').textContent).toContain('Torrents deleted: 1,204');
		expect(screen.getByTestId('purge-unverified').textContent).toContain(
			'Unverified items deleted: 12'
		);
		expect(screen.getByTestId('purge-kept').textContent).toContain('found them too: 5');
		expect(r).toBeTruthy();
		expect(bodyOf()).toEqual({ password: 'hunter2hunter2' });
	});

	it('says a wrong password plainly', async () => {
		setup(nyaa);
		net.error('POST /sources/{id}/purge', 401, 'bad_password');
		await screen.findByTestId('source-1');
		await fireEvent.click(screen.getByTestId('source-1-purge'));
		await fireEvent.input(await screen.findByTestId('purge-password'), {
			target: { value: 'bad-bad' }
		});
		await fireEvent.click(screen.getByTestId('purge-confirm'));
		await waitFor(() =>
			expect(screen.getByRole('dialog').textContent).toContain('password is wrong')
		);
	});

	it('says plainly that another purge is running', async () => {
		setup(nyaa);
		net.error('POST /sources/{id}/purge', 409, 'purge_running');
		await screen.findByTestId('source-1');
		await fireEvent.click(screen.getByTestId('source-1-purge'));
		await fireEvent.input(await screen.findByTestId('purge-password'), {
			target: { value: 'pw-pw-pw' }
		});
		await fireEvent.click(screen.getByTestId('purge-confirm'));
		expect((await screen.findByTestId('purge-problem')).textContent).toContain('already running');
	});

	it('says what is left when the purge is cancelled', async () => {
		setup(nyaa);
		net.on('POST /sources/{id}/purge', { status: 202, body: job('cancelled') });
		await screen.findByTestId('source-1');
		await fireEvent.click(screen.getByTestId('source-1-purge'));
		await fireEvent.input(await screen.findByTestId('purge-password'), {
			target: { value: 'pw-pw-pw' }
		});
		await fireEvent.click(screen.getByTestId('purge-confirm'));
		await waitFor(() =>
			expect(screen.getByTestId('purge-progress').textContent).toContain('was cancelled')
		);
	});

	it('says what is left when the purge fails', async () => {
		setup(nyaa);
		net.on('POST /sources/{id}/purge', { status: 202, body: job('failed') });
		await screen.findByTestId('source-1');
		await fireEvent.click(screen.getByTestId('source-1-purge'));
		await fireEvent.input(await screen.findByTestId('purge-password'), {
			target: { value: 'pw-pw-pw' }
		});
		await fireEvent.click(screen.getByTestId('purge-confirm'));
		await waitFor(() =>
			expect(screen.getByTestId('purge-progress').textContent).toContain('stays off')
		);
	});
});

describe('settings form (SRC-15c)', () => {
	const cache = source({
		id: 8,
		name: 'Torrent cache',
		editable: ['enabled', 'priority', 'config'],
		config_schema: {
			type: 'object',
			required: ['caches', 'level'],
			properties: {
				caches: {
					type: 'array',
					title: 'Torrent caches',
					items: { type: 'string', maxLength: 40 },
					maxItems: 2,
					uniqueItems: true
				},
				level: { type: 'integer', minimum: 1, maximum: 5, default: 2 }
			}
		}
	});
	const odd = source({
		...cache,
		id: 9,
		name: 'Odd',
		config_schema: {
			type: 'object',
			properties: { extra: { oneOf: [{ type: 'string' }], title: 'Extra' }, n: { type: 'integer' } }
		}
	});
	const openSettings = async (id = 8) => {
		await screen.findByTestId(`source-${id}`);
		await fireEvent.click(screen.getByTestId(`source-${id}-settings`));
		return screen.findByRole('dialog');
	};

	it('draws the fields, notes what it cannot draw, and checks before sending', async () => {
		setup(cache);
		await openSettings();
		await fireEvent.input(screen.getByLabelText(/level/i), { target: { value: '9' } });
		await fireEvent.click(screen.getByTestId('cfg-save'));
		await waitFor(() => expect(screen.getByRole('dialog').textContent).toContain('5 or less'));
		expect(screen.getByTestId('cfg-problem')).toBeTruthy();
		expect(writes()).toHaveLength(0);
	});

	it('turns Save off and says why when a setting cannot be drawn', async () => {
		setup(odd);
		await openSettings(9);
		expect((screen.getByTestId('cfg-save') as HTMLButtonElement).disabled).toBe(true);
		expect(screen.getByTestId('cfg-save-off').textContent).toContain('Use the API');
		expect(screen.getByTestId('cfg-unsupported').textContent).toContain('edit through the API');
	});

	it('sends the typed config and shows what the server then says', async () => {
		setup(cache);
		net.error('PATCH /sources/{id}', 400, 'invalid_config', { message: 'caches: not an address.' });
		await openSettings();
		await fireEvent.click(screen.getByTestId('cfg-caches-add'));
		await fireEvent.input(screen.getAllByRole('textbox', { name: /Torrent caches 1/ })[0]!, {
			target: { value: 'https://c.example/{hash}' }
		});
		await fireEvent.click(screen.getByTestId('cfg-save'));
		expect((await screen.findByTestId('cfg-problem')).textContent).toContain('not an address');
		expect(bodyOf()).toEqual({ config: { caches: ['https://c.example/{hash}'], level: 2 } });
	});
});
