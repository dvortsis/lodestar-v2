// The wizard's "Install qBittorrent for me" option (CMP-9c). Each test waits on a condition, never
// on a delay.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { clearIndexCache } from '../../settings/form/schema';
import { writeProgress } from '../steps';
import { wizardServer } from '../testing/server';

vi.mock('$app/paths', () => ({
	resolve: (path: string, params?: Record<string, string>) =>
		params ? path.replace(/\[(\w+)\]/g, (_, k: string) => params[k]!) : path
}));
vi.mock('$app/navigation', () => ({ beforeNavigate: () => {}, goto: vi.fn() }));
import WizardPage from '../WizardPage.svelte';

vi.setConfig({ testTimeout: 40_000 });
configure({ asyncUtilTimeout: 20_000 });

const OFFER = {
	id: 'qbittorrent',
	name: 'qBittorrent',
	version: '5.0.4',
	licence: 'GPL-2.0-or-later',
	size_bytes: 41_943_040,
	sha256: 'ab'.repeat(32),
	replaces: null
};
const row = (over: Record<string, unknown> = {}) => ({
	id: 'qbittorrent',
	name: 'qBittorrent',
	licence: 'GPL-2.0-or-later',
	available: '5.0.4',
	installed: null,
	health: null,
	refusal: null,
	opt_in: false,
	credentials: [],
	templates: [],
	...over
});

type Call = { method: string; path: string; body: unknown };
let calls: Call[];
let rows: unknown[];
let post: (body: unknown) => Response | Promise<Response>;
let offerReply: () => Response;
let wiz: ReturnType<typeof wizardServer>;

const ok = (body: unknown) => wiz.json(200, body);
const refuse = (status: number, code: string) =>
	wiz.json(status, { error: { code, message: 'x' } });

beforeEach(() => {
	sessionStorage.clear();
	clearIndexCache();
	wiz = wizardServer();
	calls = [];
	rows = [row()];
	post = () => ok(row({ installed: '5.0.4', health: 'healthy', available: null }));
	offerReply = () => ok(OFFER);
	vi.stubGlobal('fetch', async (url: string, init: RequestInit = {}) => {
		const method = (init.method ?? 'GET').toUpperCase();
		const path = String(url).replace(/^\/api\/v1/, '');
		if (path.startsWith('/companions')) {
			const body = init.body === undefined ? undefined : JSON.parse(String(init.body));
			calls.push({ method, path, body });
			if (path === '/companions')
				return rows === null ? refuse(503, 'companions_unavailable') : ok({ companions: rows });
			if (method === 'GET') return offerReply();
			return post(body);
		}
		return wiz.fetch(url, init);
	});
	writeProgress({ step: 'download', skipped: [], done: [] });
	vi.spyOn(console, 'warn').mockImplementation(() => {});
});
afterEach(() => {
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

const installs = () => calls.filter((c) => c.method === 'POST');
const open = async () => {
	render(WizardPage);
	await screen.findByRole('heading', { level: 2, name: 'Download client' });
};
const askButton = () => screen.findByRole('button', { name: 'Install qBittorrent for me' });

describe('Install qBittorrent for me', () => {
	it('is offered on a native build, where the server lists it as installable', async () => {
		await open();
		expect(await askButton()).toBeTruthy();
	});

	it.each([
		['Docker and Unraid', 'native_in_container'],
		['Windows', 'guided_only'],
		['an unsupported system', 'unsupported_platform']
	])('is not offered on %s', async (_name, refusal) => {
		// `available` stays set: the refusal alone must hide it.
		rows = [row({ refusal })];
		await open();
		await waitFor(() => expect(calls.some((c) => c.path === '/companions')).toBe(true));
		await screen.findByRole('button', { name: 'Add a download client' });
		expect(screen.queryByTestId('wizard-cmp')).toBeNull();
		expect(screen.queryByText('Install qBittorrent for me')).toBeNull();
	});

	it('is not offered when companions are off, or it is already installed', async () => {
		rows = null as never;
		await open();
		await screen.findByRole('button', { name: 'Add a download client' });
		expect(screen.queryByTestId('wizard-cmp')).toBeNull();
	});

	it('is not offered when it is already installed', async () => {
		rows = [row({ installed: '5.0.3' })];
		await open();
		await screen.findByRole('button', { name: 'Add a download client' });
		expect(screen.queryByTestId('wizard-cmp')).toBeNull();
	});

	it('shows every field of the offer and installs nothing before it is confirmed', async () => {
		await open();
		await fireEvent.click(await askButton());
		const offer = await screen.findByTestId('wizard-cmp-offer');
		for (const text of ['qBittorrent', '5.0.4', 'GPL-2.0-or-later', '40.0\u00a0MiB', OFFER.sha256])
			expect(offer.textContent).toContain(text);
		expect(offer.textContent).toContain('On this computer');
		expect(installs()).toEqual([]);
		// Backing out sends nothing either.
		await fireEvent.click(screen.getByRole('button', { name: 'Not now' }));
		await askButton();
		expect(installs()).toEqual([]);
	});

	it("sends the offer's own version and hash, once, and then reloads the client list", async () => {
		await open();
		await fireEvent.click(await askButton());
		await fireEvent.click(await screen.findByRole('button', { name: 'Confirm and install' }));
		await screen.findByText('qBittorrent is installed');
		expect(installs()).toEqual([
			{
				method: 'POST',
				path: '/companions/qbittorrent/install',
				body: { version: '5.0.4', sha256: OFFER.sha256 }
			}
		]);
		// No second offer button, and the install can't be started again from here.
		expect(screen.queryByRole('button', { name: 'Confirm and install' })).toBeNull();
	});

	it('shows progress while installing, with no cancel, (the banner announces it)', async () => {
		let release!: () => void;
		post = () =>
			new Promise<Response>((resolve) => {
				release = () => resolve(ok(row({ installed: '5.0.4' })));
			});
		await open();
		await fireEvent.click(await askButton());
		await fireEvent.click(await screen.findByRole('button', { name: 'Confirm and install' }));
		const busy = await screen.findByRole('button', { name: 'Installing qBittorrent…' });
		expect(busy.getAttribute('aria-busy')).toBe('true');
		expect(screen.queryByRole('button', { name: /cancel/i })).toBeNull();
		release();
		await screen.findByText('qBittorrent is installed');
	});

	it.each([
		['native_in_container', 409, /running in a container/],
		['awaiting_pin', 409, /doesn't have a checked qBittorrent download/],
		['not_wired', 409, /isn't available yet/],
		['confirmation_mismatch', 409, /no longer matches/],
		['something_new', 500, /Something went wrong/]
	])('says a refused install (%s) in plain words, never the code', async (code, status, words) => {
		post = () => refuse(status, code);
		await open();
		await fireEvent.click(await askButton());
		await fireEvent.click(await screen.findByRole('button', { name: 'Confirm and install' }));
		const banner = await screen.findByText(words);
		expect(banner.textContent).not.toContain(code);
		expect(screen.getByText("qBittorrent wasn't installed")).toBeTruthy();
	});

	it('says a refused offer in plain words', async () => {
		offerReply = () => refuse(409, 'guided_only');
		await open();
		await fireEvent.click(await askButton());
		await screen.findByText(/you install qBittorrent yourself/);
		expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();
		expect(installs()).toEqual([]);
	});

	it('retries through a fresh offer, never from the old one', async () => {
		let first = true;
		post = () => {
			if (first) {
				first = false;
				return refuse(409, 'confirmation_mismatch');
			}
			return ok(row({ installed: '5.0.5' }));
		};
		await open();
		await fireEvent.click(await askButton());
		await fireEvent.click(await screen.findByRole('button', { name: 'Confirm and install' }));
		await fireEvent.click(await screen.findByRole('button', { name: 'Try again' }));
		await screen.findByTestId('wizard-cmp-offer');
		expect(calls.filter((c) => c.method === 'GET' && c.path.endsWith('/install'))).toHaveLength(2);
		expect(installs()).toHaveLength(1);
	});

	it('never shows a credential, even if a reply carried one', async () => {
		post = () =>
			ok(
				row({
					installed: '5.0.4',
					credentials: ['webui_password'],
					webui_password: 'hunter2-secret'
				})
			);
		await open();
		await fireEvent.click(await askButton());
		await fireEvent.click(await screen.findByRole('button', { name: 'Confirm and install' }));
		await screen.findByText('qBittorrent is installed');
		expect(document.body.textContent).not.toContain('hunter2-secret');
		expect(calls.some((c) => c.path.includes('/reveal'))).toBe(false);
	});
});
