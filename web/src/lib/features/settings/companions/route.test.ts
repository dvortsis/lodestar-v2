// Settings → Companions' route editor (X-cmp-egress-ui): shown only where the server gives an
// `egress`; says whether the route is the companion's own or its source's; offers only `direct` and
// `socks5`; saves through the shared password confirmation and never says "applied"; each refusal
// is its own sentence, naming the field and never the value; the SOCKS5 warnings follow Prowlarr's
// route, saved or chosen.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { initLocale } from '$lib/i18n/locale.svelte';
import { dismissPasswordDialogs } from '../security/confirm';
import CompanionsScreen from './CompanionsScreen.svelte';

vi.setConfig({ testTimeout: 30_000 });
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });

const PROXY = '10.8.0.1:1080';
const row = (o: Record<string, unknown>) => ({
	available: null,
	credentials: [],
	health: 'healthy',
	id: 'prowlarr',
	installed: '1.30.0',
	licence: 'GPL-3.0-only',
	name: 'Prowlarr',
	opt_in: false,
	refusal: null,
	runs: 'lodestar',
	egress: { from: 'saved', setting: { route: 'direct' } },
	templates: [],
	...o
});

let companions: ReturnType<typeof row>[];
let puts: { path: string; body: unknown }[];
let gets: string[];
let answer: () => Response;
const reply = (code: number, b: unknown) =>
	new Response(JSON.stringify(b), {
		status: code,
		headers: { 'Content-Type': 'application/json' }
	});
const fail = (code: number, c: string, field?: string) =>
	reply(code, { error: { code: c, message: `raw ${c} text`, ...(field ? { field } : {}) } });
const changed = (o: Record<string, unknown> = {}) =>
	reply(200, {
		id: 'prowlarr',
		egress: { from: 'saved', setting: { route: 'socks5', proxy: PROXY, dns: 'proxy' } },
		restarting: true,
		not_restarted: null,
		...o
	});

beforeEach(() => {
	initLocale('en');
	puts = [];
	gets = [];
	answer = () => changed();
	companions = [
		row({}),
		row({
			id: 'flaresolverr',
			name: 'FlareSolverr',
			licence: 'MIT',
			opt_in: true,
			egress: null,
			runs: 'yours'
		})
	];
	vi.stubGlobal(
		'fetch',
		vi.fn(async (url: string, init: RequestInit = {}) => {
			const path = new URL(url, 'http://x').pathname.replace('/api/v1', '');
			const method = init.method ?? 'GET';
			if (method === 'PUT') {
				puts.push({ path, body: JSON.parse(String(init.body)) });
				return answer();
			}
			gets.push(path);
			if (path === '/companions') return reply(200, { companions });
			const c = companions.find((x) => `/companions/${x.id}` === path);
			return c ? reply(200, c) : fail(404, 'unknown_companion');
		})
	);
});
afterEach(() => {
	dismissPasswordDialogs();
	cleanup();
	vi.unstubAllGlobals();
	document.body.innerHTML = '';
});

const open = async () => {
	render(CompanionsScreen);
	await until(() => expect(screen.getByRole('heading', { name: 'Prowlarr' })).toBeTruthy());
};
const prowlarr = () => document.querySelector<HTMLElement>('[data-companion="prowlarr"]')!;
const editor = async () => {
	await fireEvent.click(within(prowlarr()).getByRole('button', { name: 'Change route' }));
	await until(() => expect(prowlarr().querySelector('[data-route-editor]')).toBeTruthy());
	return prowlarr().querySelector<HTMLFormElement>('[data-route-editor]')!;
};
const kind = (form: HTMLElement) =>
	within(form).getByLabelText('How its traffic leaves') as HTMLSelectElement;
/** Picks SOCKS5 with the proxy, presses Save and waits for the password dialog. */
const socksAndSave = async (proxy = PROXY, opened?: HTMLFormElement) => {
	const form = opened ?? (await editor());
	await fireEvent.change(kind(form), { target: { value: 'socks5' } });
	await fireEvent.input(within(form).getByLabelText('Proxy address'), {
		target: { value: proxy }
	});
	await fireEvent.click(within(form).getByRole('button', { name: 'Save and restart' }));
	return screen.findByRole('dialog', {}, { timeout: 10_000 });
};
const confirm = async (password = 'pw') => {
	await fireEvent.input(screen.getByTestId('password-dialog-input'), {
		target: { value: password }
	});
	await fireEvent.click(screen.getByTestId('password-dialog-confirm'));
};

describe('the route on the row', () => {
	it('has no editor where egress is null', async () => {
		await open();
		const flare = document.querySelector<HTMLElement>('[data-companion="flaresolverr"]')!;
		expect(within(flare).queryByRole('button', { name: 'Change route' })).toBeNull();
		expect(flare.querySelector('[data-route]')).toBeNull();
		expect(within(prowlarr()).getByRole('button', { name: 'Change route' })).toBeTruthy();
	});
	it('has no editor for a companion that is not installed yet (saving would always be refused)', async () => {
		companions[0] = row({ installed: null, health: null, available: '1.30.0' });
		await open();
		expect(within(prowlarr()).queryByRole('button', { name: 'Change route' })).toBeNull();
	});
	it("says a saved route is the companion's own", async () => {
		await open();
		await editor();
		const line = prowlarr().querySelector('[data-route="saved"]')!;
		expect(line.textContent).toContain('directly, outside any VPN');
		expect(line.textContent).toContain('Set here for this companion.');
	});
	it("says a source's route is copied, and names an interface route without offering it", async () => {
		companions[0] = row({ egress: { from: 'source', setting: { route: 'interface' } } });
		await open();
		const select = kind(await editor());
		const line = prowlarr().querySelector('[data-route="source"]')!;
		expect(line.textContent).toContain('bound to a network interface');
		expect(line.textContent).toContain('Copied from the source it serves');
		expect(select.value).toBe('');
		expect([...select.options].map((o) => o.value)).toEqual(['', 'direct', 'socks5']);
	});
	it("says an unreadable route keeps it stopped, never that it's direct", async () => {
		companions[0] = row({ egress: { from: 'saved', setting: null } });
		await open();
		await editor();
		const line = prowlarr().querySelector('[data-route]')!;
		expect(line.textContent).toContain("can't be read");
		expect(line.textContent).not.toContain('direct');
	});
});

describe('saving a route', () => {
	it('warns of the restart, asks for the password, then sends only the route and the password', async () => {
		await open();
		const form = await editor();
		expect(form.querySelector('[data-restart-warning]')?.textContent).toContain(
			'Saving restarts Prowlarr on the new route'
		);
		const dialog = await socksAndSave(PROXY, form);
		expect(dialog.textContent).toContain('Saving restarts Prowlarr on the new route');
		expect(puts).toEqual([]);
		await confirm('secret-pw');
		await until(() => expect(puts).toHaveLength(1));
		expect(puts[0]).toEqual({
			path: '/companions/prowlarr/egress',
			body: { egress: { route: 'socks5', proxy: PROXY, dns: 'proxy' }, password: 'secret-pw' }
		});
	});
	it('says it is restarting on the new route and looks at its health, never "applied"', async () => {
		await open();
		await socksAndSave();
		await confirm();
		await until(() =>
			expect(prowlarr().textContent).toContain('Restarting Prowlarr on the new route')
		);
		expect(gets).toContain('/companions/prowlarr');
		expect(document.body.textContent).not.toMatch(/applied/i);
		expect(prowlarr().querySelector('[data-route-editor]')).toBeNull();
	});
	it.each([
		['not_wired', "Companions can't be started in this copy of Lodestar"],
		['busy', 'Another change to it was under way'],
		['restart_failed', "Its restart couldn't begin."],
		['something_new', "It wasn't restarted now."]
	])('saved but not restarted (%s): used at the next start', async (code, words) => {
		answer = () => changed({ restarting: false, not_restarted: code });
		await open();
		await socksAndSave();
		await confirm();
		await until(() => expect(prowlarr().textContent).toContain('uses it at its next start'));
		expect(prowlarr().textContent).toContain(words);
		expect(prowlarr().textContent).not.toContain(code);
		expect(document.body.textContent).not.toMatch(/applied/i);
	});
	it('sends a direct route with no proxy or DNS, and warns that it leaves outside the VPN', async () => {
		companions[0] = row({
			egress: { from: 'saved', setting: { route: 'socks5', proxy: PROXY, dns: 'local' } }
		});
		await open();
		const form = await editor();
		expect(kind(form).value).toBe('socks5');
		expect((within(form).getByLabelText('Proxy address') as HTMLInputElement).value).toBe(PROXY);
		await fireEvent.change(kind(form), { target: { value: 'direct' } });
		expect(form.querySelector('[data-direct-warning]')).toBeTruthy();
		await fireEvent.click(within(form).getByRole('button', { name: 'Save and restart' }));
		await screen.findByRole('dialog', {}, { timeout: 10_000 });
		await confirm();
		await until(() => expect(puts).toHaveLength(1));
		expect(puts[0]!.body).toEqual({ egress: { route: 'direct' }, password: 'pw' });
	});
	it('always has the proxy look up names: a companion has no other choice', async () => {
		companions[0] = row({
			egress: { from: 'saved', setting: { route: 'socks5', proxy: PROXY, dns: 'local' } }
		});
		await open();
		const form = await editor();
		expect(within(form).queryByLabelText(/looks up names/)).toBeNull();
		await fireEvent.click(within(form).getByRole('button', { name: 'Save and restart' }));
		await screen.findByRole('dialog', {}, { timeout: 10_000 });
		await confirm();
		await until(() => expect(puts).toHaveLength(1));
		expect(puts[0]!.body).toEqual({
			egress: { route: 'socks5', proxy: PROXY, dns: 'proxy' },
			password: 'pw'
		});
	});
	it('asks for the proxy before anything is sent', async () => {
		await open();
		const form = await editor();
		await fireEvent.change(kind(form), { target: { value: 'socks5' } });
		await fireEvent.click(within(form).getByRole('button', { name: 'Save and restart' }));
		await until(() => expect(form.textContent).toContain('Enter the proxy address first.'));
		expect(screen.queryByRole('dialog')).toBeNull();
		expect(puts).toEqual([]);
	});
	it('Cancel in the password dialog sends nothing', async () => {
		await open();
		await socksAndSave();
		await fireEvent.click(screen.getByTestId('password-dialog-cancel'));
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(puts).toEqual([]);
	});
	it('a lock-out stays in the dialog with its countdown, and sends nothing more', async () => {
		answer = () => {
			const r = fail(429, 'rate_limited');
			r.headers.set('Retry-After', '30');
			return r;
		};
		await open();
		const dialog = await socksAndSave();
		await confirm();
		const countdown = await within(dialog).findByTestId('lockout-countdown', undefined, {
			timeout: 10_000
		});
		expect(countdown.textContent).toMatch(/Try again in (30|29) s\./);
		expect((screen.getByTestId('password-dialog-input') as HTMLInputElement).value).toBe('');
		expect(prowlarr().querySelector('[data-route-editor]')?.textContent).not.toContain('Too many');
		expect(document.body.textContent).not.toContain('raw rate_limited text');
		expect(puts).toHaveLength(1);
	});
	it('a session that ended closes the dialog and shows nothing', async () => {
		answer = () => fail(401, 'login_required');
		await open();
		await socksAndSave();
		await confirm();
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(prowlarr().textContent).not.toContain('raw login_required text');
		expect(prowlarr().querySelector('[role="alert"]')).toBeNull();
	});
	it("puts focus on the companion's name after a save", async () => {
		await open();
		await socksAndSave();
		await confirm();
		await until(() =>
			expect(document.activeElement).toBe(
				within(prowlarr()).getByRole('heading', { name: 'Prowlarr' })
			)
		);
	});
	it('a wrong password stays in the dialog, in plain words, with the field cleared', async () => {
		answer = () => fail(401, 'bad_password');
		await open();
		await socksAndSave();
		await confirm('wrong');
		await until(() =>
			expect(screen.getByRole('dialog').textContent).toContain("That password isn't right.")
		);
		expect((screen.getByTestId('password-dialog-input') as HTMLInputElement).value).toBe('');
		expect(document.body.textContent).not.toContain('raw bad_password text');
	});
});

describe('a refused route change', () => {
	it.each([
		['unknown_companion', 404, "doesn't know that companion"],
		['takes_no_route', 409, 'takes no route'],
		['not_installed', 409, "didn't install this companion"],
		['busy', 409, 'Something else is changing this companion'],
		['cant_follow_route', 409, "can't use that kind of route"],
		['route_unavailable', 409, "can't be used for a companion"],
		['not_wired', 409, "can't be started in this copy"],
		['bad_body', 422, "couldn't read that change"]
	])('%s gets its own sentence, never the code or the server text', async (code, status, words) => {
		answer = () => fail(status, code);
		await open();
		await socksAndSave();
		await confirm();
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		await until(() => expect(prowlarr().textContent).toContain(words));
		expect(prowlarr().textContent).toContain('Nothing was saved.');
		expect(document.body.textContent).not.toContain(`raw ${code} text`);
		expect(prowlarr().textContent).not.toContain(code);
	});
	it('keeps its sentence when the refreshed row has no route any more, and closes the editor', async () => {
		answer = () => {
			companions[0] = row({ egress: null, runs: 'yours' });
			return fail(409, 'takes_no_route');
		};
		await open();
		await socksAndSave();
		await confirm();
		await until(() => expect(prowlarr().querySelector('[data-route-editor]')).toBeNull());
		expect(prowlarr().textContent).toContain('This companion takes no route.');
		expect(prowlarr().querySelector('[data-socks-warnings]')).toBeNull();
		expect(within(prowlarr()).queryByRole('button', { name: 'Change route' })).toBeNull();
	});
	it.each([
		['egress.proxy', 'Check the proxy address'],
		['egress.dns', 'A direct route takes no proxy'],
		['egress', 'Check the route.']
	])('invalid_egress names the field (%s), never the value', async (field, words) => {
		const typed = '999.1.1.1:0';
		answer = () => fail(400, 'invalid_egress', field);
		await open();
		await socksAndSave(typed);
		await confirm();
		await until(() => expect(prowlarr().textContent).toContain(words));
		expect(prowlarr().textContent).not.toContain(typed);
		expect(document.body.textContent).not.toContain('raw invalid_egress text');
	});
});

describe('the SOCKS5 warnings', () => {
	it('are off while Prowlarr is direct', async () => {
		await open();
		expect(document.querySelector('[data-socks-warnings]')).toBeNull();
	});
	it("show on Prowlarr's row and FlareSolverr's while Prowlarr's saved route is SOCKS5", async () => {
		companions[0] = row({
			egress: { from: 'saved', setting: { route: 'socks5', proxy: PROXY } }
		});
		await open();
		const list = prowlarr().querySelector('[data-socks-warnings="saved"]')!;
		expect(list.textContent).toContain('Email notifications connect directly');
		expect(list.textContent).toContain('A FlareSolverr pairing leaves directly');
		expect(list.textContent).toContain('Per-indexer proxies you add in Prowlarr bypass the route');
		const flare = document.querySelector<HTMLElement>('[data-companion="flaresolverr"]')!;
		const pairing = flare.querySelector('[data-socks-warnings]')!;
		expect(pairing.textContent).toContain('A FlareSolverr pairing leaves directly');
		expect(pairing.textContent).not.toContain('Email');
	});
	it('show live while SOCKS5 is chosen in the editor, and go when it is not', async () => {
		await open();
		const form = await editor();
		await fireEvent.change(kind(form), { target: { value: 'socks5' } });
		await until(() =>
			expect(prowlarr().querySelector('[data-socks-warnings="draft"]')).toBeTruthy()
		);
		expect(prowlarr().textContent).toContain('If you save a SOCKS5 proxy for Prowlarr');
		await fireEvent.change(kind(form), { target: { value: 'direct' } });
		await until(() => expect(prowlarr().querySelector('[data-socks-warnings]')).toBeNull());
		await fireEvent.change(kind(form), { target: { value: 'socks5' } });
		await until(() => expect(prowlarr().querySelector('[data-socks-warnings]')).toBeTruthy());
		await fireEvent.click(within(prowlarr()).getByRole('button', { name: 'Change route' }));
		await until(() => expect(prowlarr().querySelector('[data-socks-warnings]')).toBeNull());
	});
});
