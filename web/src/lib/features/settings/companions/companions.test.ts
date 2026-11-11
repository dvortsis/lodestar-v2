// Settings → Companions' blocking rules (CMP-9a): the install confirmation shows every field the
// server's offer carries and nothing installs before it; the request echoes only the offer; a
// revealed credential is dropped on Hide; a pasted secret is never rendered back; refusals are
// plain words; log text is text; the SOCKS5 and route notes show.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { initLocale } from '$lib/i18n/locale.svelte';
import CompanionsScreen from './CompanionsScreen.svelte';
import { wordsFor } from './logic';

const clip = vi.hoisted(() => ({
	result: 'copied' as 'copied' | 'manual',
	calls: [] as string[]
}));
vi.mock('$lib/util/clipboard', () => ({
	copyText: async (text: string) => {
		clip.calls.push(text);
		return clip.result;
	}
}));
vi.setConfig({ testTimeout: 30_000 });
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });

const SHA = 'a'.repeat(63) + 'b';
const row = (o: Record<string, unknown>) => ({
	available: null,
	credentials: [],
	health: null,
	id: 'qbittorrent',
	installed: null,
	licence: 'GPL-2.0-or-later',
	name: 'qBittorrent',
	opt_in: false,
	refusal: null,
	templates: [],
	...o
});

let companions: ReturnType<typeof row>[];
let sent: string[];
let answers: Record<string, () => Response>;
let bodies: Record<string, unknown>;
const reply = (code: number, b: unknown) =>
	new Response(JSON.stringify(b), {
		status: code,
		headers: { 'Content-Type': 'application/json' }
	});
const fail = (code: number, c: string) =>
	reply(code, { error: { code: c, message: `raw ${c} text` } });

beforeEach(() => {
	initLocale('en');
	clip.result = 'copied';
	clip.calls = [];
	sent = [];
	answers = {};
	bodies = {};
	companions = [
		row({
			available: '5.0.1',
			health: 'healthy',
			installed: '5.0.0',
			credentials: ['webui_password']
		}),
		row({
			id: 'prowlarr',
			name: 'Prowlarr',
			licence: 'GPL-3.0-only',
			refusal: 'native_in_container'
		})
	];
	vi.stubGlobal(
		'fetch',
		vi.fn(async (url: string, init: RequestInit = {}) => {
			const path = new URL(url, 'http://x').pathname.replace('/api/v1', '');
			const method = init.method ?? 'GET';
			if (method !== 'GET') {
				sent.push(`${method} ${path} ${init.body ?? ''}`);
				bodies[`${method} ${path}`] = init.body ? JSON.parse(String(init.body)) : undefined;
			}
			const hit = answers[`${method} ${path}`];
			if (hit) return hit();
			if (path === '/companions') return reply(200, { companions });
			if (path === '/companions/qbittorrent/install' && method === 'GET')
				return reply(200, {
					id: 'qbittorrent',
					name: 'qBittorrent',
					licence: 'GPL-2.0-or-later',
					replaces: '5.0.0',
					sha256: SHA,
					size_bytes: 12_582_912,
					version: '5.0.1'
				});
			if (path === '/companions/qbittorrent/install') return reply(200, companions[0]);
			if (path === '/companions/qbittorrent') return reply(200, companions[0]);
			return fail(404, 'unknown_companion');
		})
	);
});
afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
	document.body.innerHTML = '';
});

const posts = () => sent.filter((s) => s.startsWith('POST'));
const open = async () => {
	render(CompanionsScreen);
	await until(() => expect(screen.getByRole('heading', { name: 'Prowlarr' })).toBeTruthy());
};

describe('the list', () => {
	it('shows health, version, the update notice and a refusal in words', async () => {
		await open();
		expect(screen.getByText('Running')).toBeTruthy();
		expect(screen.getByText(/Installed: 5\.0\.0/)).toBeTruthy();
		expect(screen.getByText('Update available: 5.0.1')).toBeTruthy();
		expect(document.body.textContent).not.toContain('native_in_container');
		expect(screen.getByText(/can't install this program itself/)).toBeTruthy();
	});
	it('says the route is copied at first start and that a later change does not reach it', async () => {
		await open();
		expect(screen.getAllByText(/copied from the source that needs it/).length).toBeGreaterThan(0);
		expect(screen.getAllByText(/keeps the route it started with/).length).toBeGreaterThan(0);
	});
	it('warns about email, a FlareSolverr pairing and indexer proxies on SOCKS5', async () => {
		await open();
		expect(screen.getByText(/email notifications connect directly/)).toBeTruthy();
		expect(screen.getByText(/a FlareSolverr pairing leaves directly/)).toBeTruthy();
		expect(screen.getByText(/per-indexer proxies .* bypass the route/)).toBeTruthy();
	});
});

describe('the install confirmation', () => {
	it('shows every field and installs nothing until confirmed, then echoes only the offer', async () => {
		await open();
		await fireEvent.click(screen.getByRole('button', { name: 'Update' }));
		const dialog = await screen.findByRole('dialog', {}, { timeout: 10_000 });
		const text = dialog.textContent ?? '';
		for (const want of [
			'qBittorrent',
			'5.0.0 to 5.0.1',
			'GPL-2.0-or-later',
			'12.0',
			SHA,
			'this computer'
		])
			expect(text).toContain(want);
		expect(posts()).toEqual([]);
		await fireEvent.click(
			within(screen.getByRole('dialog')).getByRole('button', { name: 'Update' })
		);
		await until(() => expect(posts()).toHaveLength(1));
		expect(JSON.parse(posts()[0]!.split(' ').slice(2).join(' '))).toEqual({
			version: '5.0.1',
			sha256: SHA
		});
	});
	it('Cancel sends nothing', async () => {
		await open();
		await fireEvent.click(screen.getByRole('button', { name: 'Update' }));
		await screen.findByRole('dialog', {}, { timeout: 10_000 });
		await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(posts()).toEqual([]);
	});
	it('a refused install shows plain words, never the code or the server text', async () => {
		answers['POST /companions/qbittorrent/install'] = () => fail(409, 'confirmation_mismatch');
		await open();
		await fireEvent.click(screen.getByRole('button', { name: 'Update' }));
		await screen.findByRole('dialog', {}, { timeout: 10_000 });
		await fireEvent.click(
			within(screen.getByRole('dialog')).getByRole('button', { name: 'Update' })
		);
		await until(() => expect(screen.getByText(/no longer matches/)).toBeTruthy());
		expect(document.body.textContent).not.toContain('confirmation_mismatch');
		expect(document.body.textContent).not.toContain('raw ');
	});
	it('an unknown code gets a generic sentence that does not print it', () => {
		const words = wordsFor('some_new_code_9');
		expect(words).toBeTruthy();
		expect(words).not.toContain('some_new_code_9');
		expect(wordsFor('__proto__')).toBe(words);
	});
});

describe('credentials', () => {
	const openCreds = async () => {
		await open();
		await fireEvent.click(screen.getAllByRole('button', { name: 'Passwords and keys' })[0]!);
		await fireEvent.click(await screen.findByRole('button', { name: 'Show' }, { timeout: 10_000 }));
	};
	it('shows a revealed value once and drops it on Hide; the password is never kept', async () => {
		answers['POST /companions/qbittorrent/credentials/webui_password/reveal'] = () =>
			reply(200, { field: 'webui_password', id: 'qbittorrent', value: 'S3cret-value' });
		await openCreds();
		const pw = screen.getByLabelText(/Your Lodestar password/) as HTMLInputElement;
		expect(pw.type).toBe('password');
		await fireEvent.input(pw, { target: { value: 'admin-pass' } });
		await fireEvent.click(screen.getByRole('button', { name: 'Show it' }));
		await until(() =>
			expect(document.querySelector('[data-revealed]')?.textContent).toBe('S3cret-value')
		);
		expect(document.body.textContent).not.toContain('admin-pass');
		await fireEvent.click(screen.getByRole('button', { name: 'Hide' }));
		expect(document.body.textContent).not.toContain('S3cret-value');
		// Showing again asks for the password again; the value is not there until it's sent.
		await fireEvent.click(screen.getByRole('button', { name: 'Show' }));
		expect(document.body.textContent).not.toContain('S3cret-value');
		expect((screen.getByLabelText(/Your Lodestar password/) as HTMLInputElement).value).toBe('');
	});
	it('a wrong password shows words only', async () => {
		answers['POST /companions/qbittorrent/credentials/webui_password/reveal'] = () =>
			fail(401, 'bad_password');
		await openCreds();
		await fireEvent.input(screen.getByLabelText(/Your Lodestar password/), {
			target: { value: 'nope-pass' }
		});
		await fireEvent.click(screen.getByRole('button', { name: 'Show it' }));
		await until(() => expect(screen.getByText("That password isn't right.")).toBeTruthy());
		expect(document.body.textContent).not.toContain('nope-pass');
	});
	it('a pasted secret goes in a password field, is sent once and is never echoed', async () => {
		companions[0] = row({ refusal: 'guided_only' });
		answers['POST /companions/qbittorrent/paste'] = () => fail(409, 'generated_pending');
		await open();
		await fireEvent.click(screen.getAllByRole('button', { name: 'Passwords and keys' })[0]!);
		const input = (await screen.findByLabelText(
			/Temporary password for qBittorrent/
		)) as HTMLInputElement;
		expect(input.type).toBe('password');
		await fireEvent.input(input, { target: { value: 'tmp-Pass-123' } });
		await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
		await until(() => expect(posts()).toHaveLength(1));
		await until(() => expect(screen.getByText(/new password may already be in use/)).toBeTruthy());
		expect(input.value).toBe('');
		expect(document.body.innerHTML).not.toContain('tmp-Pass-123');
	});
});

describe('logs', () => {
	it('shows a log line as text, never as HTML', async () => {
		answers['GET /companions/qbittorrent/logs'] = () =>
			reply(200, { id: 'qbittorrent', lines: ['<img src=x onerror=alert(1)>', 'ok'] });
		await open();
		await fireEvent.click(screen.getAllByRole('button', { name: 'Logs' })[0]!);
		await until(() => expect(document.querySelector('[data-logs]')).toBeTruthy());
		const pre = document.querySelector('[data-logs]')!;
		expect(pre.textContent).toContain('<img src=x onerror=alert(1)>');
		expect(document.querySelector('img')).toBeNull();
	});
});

describe('remove, regenerate, restart', () => {
	beforeEach(() => {
		answers['GET /companions/qbittorrent/remove'] = () =>
			reply(200, {
				id: 'qbittorrent',
				removed: false,
				deletes: [
					{ kind: 'folder', name: 'companions/qbittorrent' },
					{ kind: 'setting', name: 'companions.qbittorrent.password' }
				]
			});
	});
	it('lists what removal deletes; Cancel sends nothing; Remove echoes only the id', async () => {
		answers['POST /companions/qbittorrent/remove'] = () =>
			reply(200, { id: 'qbittorrent', removed: true, deletes: [] });
		await open();
		await fireEvent.click(screen.getAllByRole('button', { name: 'Remove' })[0]!);
		const dialog = await screen.findByRole('dialog', {}, { timeout: 10_000 });
		expect(dialog.textContent).toContain('companions/qbittorrent');
		expect(dialog.textContent).toContain('companions.qbittorrent.password');
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(posts()).toEqual([]);
		await fireEvent.click(screen.getAllByRole('button', { name: 'Remove' })[0]!);
		const again = await screen.findByRole('dialog', {}, { timeout: 10_000 });
		await fireEvent.click(within(again).getByRole('button', { name: 'Remove' }));
		await until(() => expect(posts()).toHaveLength(1));
		expect(bodies['POST /companions/qbittorrent/remove']).toEqual({ confirm: 'qbittorrent' });
	});
	it('Cancel on regenerate sends nothing and an open reveal is dropped when it proceeds', async () => {
		answers['POST /companions/qbittorrent/credentials/webui_password/reveal'] = () =>
			reply(200, { field: 'webui_password', id: 'qbittorrent', value: 'S3cret-value' });
		answers['POST /companions/qbittorrent/credentials/webui_password/regenerate'] = () =>
			reply(202, { id: 'qbittorrent' });
		await open();
		await fireEvent.click(screen.getAllByRole('button', { name: 'Passwords and keys' })[0]!);
		await fireEvent.click(await screen.findByRole('button', { name: 'Show' }, { timeout: 10_000 }));
		await fireEvent.input(screen.getByLabelText(/Your Lodestar password/), {
			target: { value: 'pw' }
		});
		await fireEvent.click(screen.getByRole('button', { name: 'Show it' }));
		await until(() => expect(document.querySelector('[data-revealed]')).toBeTruthy());
		await fireEvent.click(screen.getByRole('button', { name: 'Make a new one' }));
		const dialog = await screen.findByRole('dialog', {}, { timeout: 10_000 });
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(posts().filter((p) => p.includes('regenerate'))).toEqual([]);
		await fireEvent.click(screen.getByRole('button', { name: 'Make a new one' }));
		const again = await screen.findByRole('dialog', {}, { timeout: 10_000 });
		await fireEvent.click(within(again).getByRole('button', { name: 'Make a new one' }));
		await until(() => expect(posts().filter((p) => p.includes('regenerate'))).toHaveLength(1));
		expect(document.body.textContent).not.toContain('S3cret-value');
	});
	it('Restart posts and says it is restarting', async () => {
		answers['POST /companions/qbittorrent/restart'] = () => reply(202, { id: 'qbittorrent' });
		await open();
		await fireEvent.click(screen.getByRole('button', { name: 'Restart' }));
		await until(() => expect(screen.getByText(/Restarting/)).toBeTruthy());
		expect(posts()).toHaveLength(1);
	});
});

describe('detect, add and templates', () => {
	it('adds a detected companion with the server target, not a guess', async () => {
		answers['POST /companions/detect'] = () =>
			reply(200, {
				found: [{ id: 'qbittorrent', version: '5.0.0', target: { kind: 'container', port: 8080 } }]
			});
		answers['POST /companions/qbittorrent/target'] = () =>
			reply(200, {
				id: 'qbittorrent',
				version: '5.0.0',
				target: { kind: 'container', port: 8080 }
			});
		await open();
		await fireEvent.click(screen.getByRole('button', { name: 'Look for companions' }));
		await fireEvent.click(await screen.findByRole('button', { name: 'Add qBittorrent' }));
		await until(() => expect(bodies['POST /companions/qbittorrent/target']).toBeTruthy());
		expect(bodies['POST /companions/qbittorrent/target']).toEqual({
			kind: 'container',
			port: 8080
		});
	});
	it('a refused address shows words, and an empty form says what is missing', async () => {
		answers['POST /companions/qbittorrent/target'] = () => fail(409, 'address_refused');
		await open();
		await fireEvent.click(screen.getByRole('button', { name: 'Add' }));
		await until(() => expect(screen.getByText('Choose a companion first.')).toBeTruthy());
		expect(posts()).toEqual([]);
		await fireEvent.change(screen.getByLabelText('Companion'), {
			target: { value: 'qbittorrent' }
		});
		await fireEvent.input(screen.getByLabelText(/Address/), {
			target: { value: 'http://10.0.0.5:8080' }
		});
		await fireEvent.click(screen.getByRole('button', { name: 'Add' }));
		await until(() => expect(screen.getByText("That address can't be used.")).toBeTruthy());
		expect(document.body.textContent).not.toContain('address_refused');
	});
	it('sends the chosen route, warns on direct, and shows the template as text', async () => {
		companions[0] = row({ installed: '5.0.0', templates: ['compose'] });
		answers['POST /companions/qbittorrent/template'] = () =>
			reply(200, { id: 'qbittorrent', kind: 'compose', compose: 'x: <b>y</b>', unraid: [] });
		await open();
		await fireEvent.click(screen.getAllByRole('button', { name: 'Run as its own container' })[0]!);
		await until(() => expect(document.querySelector('[data-direct-warning]')).toBeTruthy());
		await fireEvent.change(screen.getByLabelText('How its traffic leaves'), {
			target: { value: 'gluetun' }
		});
		await until(() => expect(document.querySelector('[data-direct-warning]')).toBeNull());
		await fireEvent.click(screen.getByRole('button', { name: 'Compose service' }));
		await until(() => expect(document.querySelector('pre')?.textContent).toBe('x: <b>y</b>'));
		expect(document.querySelector('pre b')).toBeNull();
		expect(bodies['POST /companions/qbittorrent/template']).toEqual({
			kind: 'compose',
			route: 'gluetun'
		});
	});
});

describe('states', () => {
	it('says companions are off when the server says so', async () => {
		answers['GET /companions'] = () => fail(503, 'companions_unavailable');
		render(CompanionsScreen);
		await until(() => expect(screen.getByText(/switched off in this copy/)).toBeTruthy());
	});
	it('a failed load offers Try again; a later answer shows the list', async () => {
		let n = 0;
		answers['GET /companions'] = () =>
			n++ === 0 ? fail(500, 'internal') : reply(200, { companions });
		render(CompanionsScreen);
		await until(() => expect(screen.getByText(/couldn't be loaded/)).toBeTruthy());
		await fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
		await until(() => expect(screen.getByRole('heading', { name: 'Prowlarr' })).toBeTruthy());
	});
	it('shows the size in the shared format, with a test id', async () => {
		await open();
		await fireEvent.click(screen.getByRole('button', { name: 'Update' }));
		await screen.findByRole('dialog', {}, { timeout: 10_000 });
		expect(screen.getByTestId('offer-field-3').textContent).toContain('12.0');
	});
});

describe('busy guards, focus and a late error', () => {
	const hold = () => {
		let release: () => void = () => {};
		const gate = new Promise<void>((r) => (release = r));
		return { gate, release };
	};
	it('a second press of Update while the install runs sends nothing more', async () => {
		const { gate, release } = hold();
		answers['POST /companions/qbittorrent/install'] = (() =>
			gate.then(() => reply(200, companions[0]))) as never;
		await open();
		await fireEvent.click(screen.getByRole('button', { name: 'Update' }));
		const dialog = await screen.findByRole('dialog', {}, { timeout: 10_000 });
		const go = within(dialog).getByRole('button', { name: 'Update' });
		await fireEvent.click(go);
		await until(() => expect(posts()).toHaveLength(1));
		await fireEvent.click(go);
		await fireEvent.click(go);
		expect(posts()).toHaveLength(1);
		release();
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
	});
	it('a second press of Remove while the removal runs sends nothing more', async () => {
		const { gate, release } = hold();
		answers['GET /companions/qbittorrent/remove'] = () =>
			reply(200, {
				id: 'qbittorrent',
				removed: false,
				deletes: [{ kind: 'folder', name: 'companions/qbittorrent' }]
			});
		answers['POST /companions/qbittorrent/remove'] = (() =>
			gate.then(() => reply(200, { id: 'qbittorrent', removed: true, deletes: [] }))) as never;
		await open();
		await fireEvent.click(screen.getAllByRole('button', { name: 'Remove' })[0]!);
		const dialog = await screen.findByRole('dialog', {}, { timeout: 10_000 });
		const go = within(dialog).getByRole('button', { name: 'Remove' });
		await fireEvent.click(go);
		await until(() => expect(posts()).toHaveLength(1));
		await fireEvent.click(go);
		await fireEvent.click(go);
		expect(posts()).toHaveLength(1);
		release();
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
	});
	it('after an install the row name has focus', async () => {
		await open();
		await fireEvent.click(screen.getByRole('button', { name: 'Update' }));
		const dialog = await screen.findByRole('dialog', {}, { timeout: 10_000 });
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Update' }));
		await until(() =>
			expect(document.activeElement).toBe(screen.getByRole('heading', { name: 'qBittorrent' }))
		);
	});
	it('after a removal the row name has focus', async () => {
		answers['GET /companions/qbittorrent/remove'] = () =>
			reply(200, {
				id: 'qbittorrent',
				removed: false,
				deletes: [{ kind: 'folder', name: 'companions/qbittorrent' }]
			});
		answers['POST /companions/qbittorrent/remove'] = () =>
			reply(200, { id: 'qbittorrent', removed: true, deletes: [] });
		await open();
		await fireEvent.click(screen.getAllByRole('button', { name: 'Remove' })[0]!);
		const dialog = await screen.findByRole('dialog', {}, { timeout: 10_000 });
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Remove' }));
		await until(() =>
			expect(document.activeElement).toBe(screen.getByRole('heading', { name: 'qBittorrent' }))
		);
	});
	it('an error that arrives after the dialog was closed shows in the row', async () => {
		const { gate, release } = hold();
		answers['POST /companions/qbittorrent/install'] = (() =>
			gate.then(() => fail(409, 'busy'))) as never;
		await open();
		await fireEvent.click(screen.getByRole('button', { name: 'Update' }));
		const dialog = await screen.findByRole('dialog', {}, { timeout: 10_000 });
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Update' }));
		await until(() => expect(posts()).toHaveLength(1));
		await fireEvent.keyDown(dialog, { key: 'Escape' });
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		release();
		await until(() => expect(screen.getByText(/Something else is changing/)).toBeTruthy());
	});
});

describe('words and copy', () => {
	it('tells the user to remove their own download client first', () => {
		expect(wordsFor('managed_client_in_use')).toMatch(/Remove it in Integrations first/);
		expect(wordsFor('sink_refused')).toMatch(/download client refused/);
		expect(wordsFor('timed_out')).toMatch(/too long/);
	});
	it('an unexpected failure does not claim nothing changed', () => {
		expect(wordsFor('internal')).not.toMatch(/Nothing was changed/);
	});
	it('a revealed value has a Copy button (old test)', async () => {
		answers['POST /companions/qbittorrent/credentials/webui_password/reveal'] = () =>
			reply(200, { field: 'webui_password', id: 'qbittorrent', value: 'S3cret-value' });
		await open();
		await fireEvent.click(screen.getAllByRole('button', { name: 'Passwords and keys' })[0]!);
		await fireEvent.click(await screen.findByRole('button', { name: 'Show' }, { timeout: 10_000 }));
		await fireEvent.input(screen.getByLabelText(/Your Lodestar password/), {
			target: { value: 'pw' }
		});
		await fireEvent.click(screen.getByRole('button', { name: 'Show it' }));
		await until(() => expect(screen.getByRole('button', { name: 'Copy' })).toBeTruthy());
	});
	it('a detected companion with no version has no trailing space', async () => {
		answers['POST /companions/detect'] = () =>
			reply(200, {
				found: [{ id: 'qbittorrent', version: null, target: { kind: 'loopback', port: 8080 } }]
			});
		await open();
		await fireEvent.click(screen.getByRole('button', { name: 'Look for companions' }));
		await until(() => expect(document.querySelector('[data-detected] li span')).toBeTruthy());
		expect(document.querySelector('[data-detected] li span')?.textContent?.trim()).toBe(
			'Found qBittorrent'
		);
		expect(document.querySelector('[data-detected] li span')?.textContent).toBe(
			'Found qBittorrent'
		);
	});
});

describe('the Copy button', () => {
	const reveal = async (value = 'S3cret-value') => {
		answers['POST /companions/qbittorrent/credentials/webui_password/reveal'] = () =>
			reply(200, { field: 'webui_password', id: 'qbittorrent', value });
		await fireEvent.click(await screen.findByRole('button', { name: 'Show' }, { timeout: 10_000 }));
		await fireEvent.input(screen.getByLabelText(/Your Lodestar password/), {
			target: { value: 'pw' }
		});
		await fireEvent.click(screen.getByRole('button', { name: 'Show it' }));
		await until(() => expect(document.querySelector('[data-revealed]')).toBeTruthy());
	};
	const openPanel = async () => {
		await open();
		await fireEvent.click(screen.getAllByRole('button', { name: 'Passwords and keys' })[0]!);
	};
	it('copies the revealed value once, says Copied, and says Copy again after Hide and a new reveal', async () => {
		await openPanel();
		await reveal();
		await fireEvent.click(screen.getByRole('button', { name: 'Copy' }));
		await until(() => expect(screen.getByRole('button', { name: 'Copied' })).toBeTruthy());
		expect(clip.calls).toEqual(['S3cret-value']);
		await fireEvent.click(screen.getByRole('button', { name: 'Hide' }));
		await reveal('Another-value');
		expect(screen.getByRole('button', { name: 'Copy' })).toBeTruthy();
		expect(screen.queryByRole('button', { name: 'Copied' })).toBeNull();
		expect(clip.calls).toEqual(['S3cret-value']);
	});
	it('after a regenerate the label is back to Copy', async () => {
		answers['POST /companions/qbittorrent/credentials/webui_password/regenerate'] = () =>
			reply(202, { id: 'qbittorrent' });
		await openPanel();
		await reveal();
		await fireEvent.click(screen.getByRole('button', { name: 'Copy' }));
		await until(() => expect(screen.getByRole('button', { name: 'Copied' })).toBeTruthy());
		await fireEvent.click(screen.getByRole('button', { name: 'Make a new one' }));
		const dialog = await screen.findByRole('dialog', {}, { timeout: 10_000 });
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Make a new one' }));
		await until(() => expect(document.querySelector('[data-revealed]')).toBeNull());
		await reveal('Fresh-value');
		expect(screen.getByRole('button', { name: 'Copy' })).toBeTruthy();
	});
	it('when nothing could copy it, says so and selects the text', async () => {
		clip.result = 'manual';
		await openPanel();
		await reveal();
		await fireEvent.click(screen.getByRole('button', { name: 'Copy' }));
		await until(() => expect(screen.getByText(/press Ctrl\+C/)).toBeTruthy());
		expect(screen.getByRole('button', { name: 'Copy' })).toBeTruthy();
		expect(getSelection()?.toString()).toBe('S3cret-value');
	});
});
