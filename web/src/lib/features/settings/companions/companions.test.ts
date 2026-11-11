// Settings → Companions' blocking rules (CMP-9a): the install confirmation shows every field the
// server's offer carries and nothing installs before it; the request echoes only the offer; a
// revealed credential is dropped on Hide; a pasted secret is never rendered back; refusals are
// plain words; log text is text; the SOCKS5 and route notes show.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { initLocale } from '$lib/i18n/locale.svelte';
import CompanionsScreen from './CompanionsScreen.svelte';
import { wordsFor } from './logic';

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
const reply = (code: number, b: unknown) =>
	new Response(JSON.stringify(b), {
		status: code,
		headers: { 'Content-Type': 'application/json' }
	});
const fail = (code: number, c: string) =>
	reply(code, { error: { code: c, message: `raw ${c} text` } });

beforeEach(() => {
	initLocale('en');
	sent = [];
	answers = {};
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
			if (method !== 'GET') sent.push(`${method} ${path} ${init.body ?? ''}`);
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
			/Temporary password from qBittorrent/
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

afterEach(() => cleanup());
