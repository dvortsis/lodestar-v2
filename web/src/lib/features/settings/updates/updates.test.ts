// Settings → Updates' blocking rules (S-updates): the screen shows only what the server says, a
// failed request keeps the last server state, Cancel sends nothing, a locked value can't change,
// the security toggle follows the server per mode, and the notices appear.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { initLocale } from '$lib/i18n/locale.svelte';
import UpdatesScreen from './UpdatesScreen.svelte';
import type { Release, Status } from './logic';
import { cannotAuto, noticesOf, safeHttps, securityDefault, securityVisible } from './logic';

const asked = vi.hoisted(() => ({ answer: 'cancel' as string | null, count: 0 }));
vi.mock('../form/dialogs', () => ({
	ask: async () => {
		asked.count++;
		return asked.answer;
	}
}));
const events = vi.hoisted(() => ({
	on: vi.fn<(t: string, h: (e: unknown) => void) => () => void>(() => () => {})
}));
vi.mock('$lib/state/events.svelte', () => ({ events }));
vi.setConfig({ testTimeout: 30_000 });
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });

const release: Release = {
	version: '0.6.0',
	date: 1_700_000_000,
	level: 'fix',
	security: true,
	manual_steps: false,
	migrates: false,
	reindex: false,
	reindex_bytes: 0,
	eligible: false,
	installs_at: null,
	blocked_by: [],
	notes: 'Fixes a crash.',
	notes_url: 'https://github.com/o/r/releases/tag/v0.6.0'
};
const base = (): Status => ({
	capability: { can_install: true, reason: null },
	history: [],
	notices: [],
	pending: null,
	policy: {
		allow_reindex: false,
		held_in_process: false,
		level: 'fixes',
		mode: 'notify',
		next_window_at: null,
		paused_until: null,
		security_fixes: false,
		wait_days: 3,
		window: '02:00-05:00',
		window_zone: 'UTC'
	},
	releases: [],
	rollback: { available: false, lost_torrents: null, to_version: null },
	running: {
		channel: 'stable',
		image_version: null,
		overlay: false,
		revoked_key: false,
		version: '0.5.2',
		withdrawn: false
	},
	status: { checked_at: 1_699_990_000, error: null, next_check_at: null, state: 'ok' },
	unavailable: null
});

let status: Status;
let fields: Record<string, { value: unknown; env: string | null; read_only: boolean }>;
let sent: string[];
let failNext: string | null;
let failStatus = false;
let rollbackAnswer: Response | null = null;

const reply = (code: number, b: unknown) =>
	new Response(JSON.stringify(b), {
		status: code,
		headers: { 'Content-Type': 'application/json' }
	});

beforeEach(() => {
	initLocale('en');
	status = base();
	fields = {
		mode: { value: 'notify', env: null, read_only: false },
		level: { value: 'fixes', env: null, read_only: false },
		security_fixes: { value: 'auto', env: null, read_only: false }
	};
	sent = [];
	failNext = null;
	failStatus = false;
	rollbackAnswer = null;
	asked.answer = 'cancel';
	asked.count = 0;
	vi.stubGlobal(
		'fetch',
		vi.fn(async (url: string, init: RequestInit = {}) => {
			const path = new URL(url, 'http://x').pathname.replace('/api/v1', '');
			const method = init.method ?? 'GET';
			if (method !== 'GET') sent.push(`${method} ${path} ${init.body ?? ''}`);
			if (method !== 'GET' && failNext === path) {
				failNext = null;
				return reply(500, { error: { code: 'updates_not_saved', message: 'nope' } });
			}
			if (path === '/updates' && method === 'GET' && failStatus)
				return reply(500, { error: { code: 'internal', message: 'down' } });
			if (path === '/updates/rollback' && rollbackAnswer) return rollbackAnswer.clone();
			if (path === '/system') return reply(200, { deployment: { kind: 'docker' } });
			if (path === '/settings/updates' && method === 'GET')
				return reply(200, { section: 'updates', revision: 1, restart_required: [], fields });
			if (path === '/settings/updates') {
				const patch = JSON.parse(String(init.body));
				for (const [k, v] of Object.entries(patch)) if (fields[k]) fields[k].value = v;
				if (patch.mode) status.policy.mode = patch.mode;
				return reply(200, { section: 'updates', revision: 2, restart_required: [], fields });
			}
			if (path === '/updates/pause') {
				status.policy.paused_until = JSON.parse(String(init.body)).days ? 1_800_000_000 : null;
				return reply(200, status);
			}
			if (path === '/updates/check' || path === '/updates/skip') return reply(200, status);
			return reply(200, status);
		})
	);
});
afterEach(() => {
	vi.unstubAllGlobals();
	document.body.innerHTML = '';
});

const mutations = () => sent.filter((s) => !s.startsWith('GET'));

describe('logic', () => {
	it('security fixes default on under automatic, off under notify, hidden otherwise', () => {
		expect(securityDefault('auto')).toBe(true);
		expect(securityDefault('notify')).toBe(false);
		expect(securityVisible('auto') && securityVisible('notify')).toBe(true);
		expect(securityVisible('off') || securityVisible('image')).toBe(false);
	});
	it('only https links are used', () => {
		expect(safeHttps('https://github.com/o/r')).toBe('https://github.com/o/r');
		for (const bad of ['http://x.test', 'javascript:alert(1)', 'https://u:p@x.test', '', null])
			expect(safeHttps(bad)).toBeNull();
	});
	it('names the reason automatic installs are off', () => {
		const s = base();
		expect(cannotAuto(s as never)).toBeNull();
		s.capability = { can_install: false, reason: 'noexec' };
		expect(cannotAuto(s as never)).toBe('noexec');
	});
	it('lists the three F-updater notices', () => {
		const s = base();
		s.notices = ['check_not_saved', 'updater_unreadable', 'rolled_back'];
		s.policy.held_in_process = true;
		expect(noticesOf(s as never)).toEqual(['updater_unreadable', 'check_not_saved']);
		s.notices = [];
		expect(noticesOf(s as never)).toEqual(['held_in_process']);
	});
});

describe('the Updates screen', () => {
	it('says up to date only with a response saying so', async () => {
		render(UpdatesScreen);
		expect((await screen.findByTestId('updates-state')).textContent).toContain('up to date');
		document.body.innerHTML = '';
		status.status = { checked_at: null, error: null, next_check_at: null, state: 'idle' };
		render(UpdatesScreen);
		await until(() =>
			expect(screen.getByTestId('updates-state').textContent).toContain('not checked yet')
		);
		expect(screen.getByTestId('updates-state').textContent).not.toContain('up to date');
	});

	it('keeps Off off until the user picks, and says checking contacts GitHub', async () => {
		fields.mode!.value = 'off';
		status.policy.mode = 'off';
		render(UpdatesScreen);
		expect(((await screen.findByTestId('updates-mode-off')) as HTMLInputElement).checked).toBe(
			true
		);
		expect(screen.getByTestId('updates-contacts').textContent).toContain('contacts GitHub');
		expect(screen.queryByTestId('updates-security-toggle')).toBeNull();
		expect(mutations()).toEqual([]);
	});

	it('a failed change leaves the server’s choice on screen with an error', async () => {
		render(UpdatesScreen);
		const off = (await screen.findByTestId('updates-mode-off')) as HTMLInputElement;
		failNext = '/settings/updates';
		await fireEvent.click(off);
		await until(() => expect(screen.getByTestId('updates-error')).toBeTruthy());
		await until(() =>
			expect((screen.getByTestId('updates-mode-notify') as HTMLInputElement).checked).toBe(true)
		);
		expect(off.checked).toBe(false);
	});

	it('a locked value shows its reason and no control changes it', async () => {
		fields.mode!.env = 'LODESTAR_UPDATES';
		render(UpdatesScreen);
		expect((await screen.findByTestId('updates-locked-mode')).textContent).toContain(
			'LODESTAR_UPDATES'
		);
		for (const id of ['off', 'notify', 'auto'])
			expect((screen.getByTestId(`updates-mode-${id}`) as HTMLInputElement).disabled).toBe(true);
		expect(mutations()).toEqual([]);
	});

	it('the security toggle shows the server’s effective value and saves a change', async () => {
		status.policy.security_fixes = true;
		render(UpdatesScreen);
		const t = (await screen.findByTestId('updates-security-toggle')) as HTMLInputElement;
		expect(t.checked).toBe(true);
		await fireEvent.click(t);
		await until(() => expect(mutations().join()).toContain('"security_fixes":"off"'));
	});

	it('Skip asks first: Cancel sends nothing, Skip sends the version', async () => {
		status.releases = [release];
		render(UpdatesScreen);
		const skip = await screen.findByTestId('updates-skip');
		await fireEvent.click(skip);
		await until(() => expect(asked.count).toBe(1));
		expect(mutations()).toEqual([]);
		asked.answer = 'skip';
		await fireEvent.click(skip);
		await until(() => expect(mutations().join()).toContain('/updates/skip'));
		expect(mutations().join()).toContain('0.6.0');
	});

	// Install now is never offered with damaged trust data (the server would refuse it anyway).
	it('offers no Install now with damaged trust data', async () => {
		status.releases = [release];
		status.notices = ['trust_damaged'];
		status.capability = { can_install: false, reason: 'trust_damaged' };
		render(UpdatesScreen);
		expect((await screen.findByTestId('updates-notice-trust_damaged')).textContent).toContain(
			'blocked'
		);
		expect(screen.queryByTestId('updates-install')).toBeNull();
		expect(screen.queryByRole('button', { name: /install now/i })).toBeNull();
		expect(screen.getByTestId('updates-cannot-auto').textContent).toContain('trust data');
	});

	describe('Install now', () => {
		const install = () => fireEvent.click(screen.getByTestId('updates-install'));
		const posts = () => mutations().filter((x) => x.includes('/updates/install'));
		let answer: [number, unknown] | null = null;
		beforeEach(() => {
			answer = null;
			status.releases = [release];
			const inner = globalThis.fetch as unknown as (
				u: string,
				i?: RequestInit
			) => Promise<Response>;
			vi.stubGlobal('fetch', async (u: string, i: RequestInit = {}) => {
				if (new URL(u, 'http://x').pathname.endsWith('/updates/install') && i.method === 'POST') {
					sent.push(`POST /updates/install ${i.body}`);
					return answer ? reply(answer[0], answer[1]) : reply(202, status);
				}
				return inner(u, i);
			});
		});

		it('posts the version, now, and no manual confirmation', async () => {
			render(UpdatesScreen);
			await screen.findByTestId('updates-install');
			await install();
			await until(() => expect(posts()).toHaveLength(1));
			expect(JSON.parse(posts()[0]!.split('/updates/install ')[1]!)).toEqual({
				version: '0.6.0',
				when: 'now',
				confirm_manual: false
			});
			expect(asked.count).toBe(0);
		});

		it('is hidden while an install is under way', async () => {
			status.installing = { version: '0.6.0', when: 'now' };
			render(UpdatesScreen);
			await screen.findByTestId('updates-available');
			expect(screen.queryByTestId('updates-install')).toBeNull();
		});

		it('asks about manual steps: Cancel sends nothing, Install sends the confirmation', async () => {
			status.releases = [{ ...release, manual_steps: true }];
			render(UpdatesScreen);
			await screen.findByTestId('updates-install');
			await install();
			await until(() => expect(asked.count).toBe(1));
			expect(posts()).toHaveLength(0);
			asked.answer = 'install';
			await install();
			await until(() => expect(posts()).toHaveLength(1));
			expect(posts()[0]).toContain('"confirm_manual":true');
		});

		it('is offered in the revoked-key notice, and hidden while a step shows', async () => {
			status.notices = ['revoked_key'];
			render(UpdatesScreen);
			await fireEvent.click(await screen.findByTestId('updates-install-revoked'));
			await until(() => expect(posts()).toHaveLength(1));
			document.body.innerHTML = '';
			status.notices = [];
			render(UpdatesScreen);
			await screen.findByTestId('updates-install');
			const handler = events.on.mock.calls.findLast((c) => c[0] === 'updates')![1];
			handler({
				state: 'downloading',
				version: '0.6.0',
				bytes_done: 1,
				bytes_total: 2,
				error: null
			});
			await until(() => expect(screen.getByTestId('updates-progress')).toBeTruthy());
			expect(screen.queryByTestId('updates-install')).toBeNull();
			handler({
				state: 'failed',
				version: '0.6.0',
				bytes_done: null,
				bytes_total: null,
				error: 'download_io'
			});
			await until(() =>
				expect(screen.getByTestId('updates-error').textContent).toContain('free disk space')
			);
			expect(screen.getByTestId('updates-install')).toBeTruthy();
		});

		it('keeps Skip while an install is queued, and words an unmapped code generally', async () => {
			status.installing = { version: '0.6.0', when: 'window' };
			render(UpdatesScreen);
			expect(await screen.findByTestId('updates-skip')).toBeTruthy();
			expect(screen.queryByTestId('updates-install')).toBeNull();
			document.body.innerHTML = '';
			status.installing = null;
			answer = [503, { error: { code: 'bin_unavailable', message: 'x' } }];
			render(UpdatesScreen);
			await fireEvent.click(await screen.findByTestId('updates-install'));
			await until(() =>
				expect(screen.getByTestId('updates-error').textContent).toContain("install didn't start")
			);
		});

		it.each([
			[404, 'unknown_version', 'no longer offered'],
			[409, 'version_skipped', 'skipped that version'],
			[409, 'install_unavailable', 'Another update is already running', 'busy'],
			[409, 'install_unavailable', 'trust data is damaged', 'trust_damaged'],
			[422, 'manual_confirmation_required', 'manual steps'],
			[500, 'updates_not_saved', "wasn't saved"]
		])('shows the %i %s answer and stays usable', async (code, name, text, reason?: string) => {
			const tail = reason ? ` (${reason})` : '';
			answer = [code, { error: { code: name, message: `refused${tail}`, reason } }];
			render(UpdatesScreen);
			await screen.findByTestId('updates-install');
			await install();
			await until(() => expect(screen.getByTestId('updates-error').textContent).toContain(text));
			expect((screen.getByTestId('updates-install') as HTMLButtonElement).disabled).toBe(false);
			expect(screen.getByTestId('updates-skip')).toBeTruthy();
		});

		it("reads install_unavailable's reason from error.reason, never the message", async () => {
			answer = [
				409,
				{
					error: { code: 'install_unavailable', message: 'refused (trust_damaged)', reason: 'busy' }
				}
			];
			render(UpdatesScreen);
			await screen.findByTestId('updates-install');
			await install();
			await until(() =>
				expect(screen.getByTestId('updates-error').textContent).toContain(
					'Another update is already running'
				)
			);
			expect(screen.getByTestId('updates-error').textContent).not.toContain(
				'trust data is damaged'
			);
		});
	});

	it('shows the three F-updater notices', async () => {
		status.notices = ['updater_unreadable', 'check_not_saved'];
		render(UpdatesScreen);
		expect(await screen.findByTestId('updates-notice-updater_unreadable')).toBeTruthy();
		expect(screen.getByTestId('updates-notice-check_not_saved')).toBeTruthy();
		document.body.innerHTML = '';
		status.notices = [];
		status.policy.held_in_process = true;
		render(UpdatesScreen);
		expect(await screen.findByTestId('updates-notice-held_in_process')).toBeTruthy();
	});

	it('disables Install automatically with the reason beside it', async () => {
		status.capability = { can_install: false, reason: 'noexec' };
		render(UpdatesScreen);
		expect(((await screen.findByTestId('updates-mode-auto')) as HTMLInputElement).disabled).toBe(
			true
		);
		expect(screen.getByTestId('updates-cannot-auto').textContent).toContain('noexec');
	});

	it('pauses and resumes, and a failed pause keeps the last state', async () => {
		render(UpdatesScreen);
		failNext = '/updates/pause';
		await fireEvent.click(await screen.findByTestId('updates-pause-7'));
		await until(() => expect(screen.getByTestId('updates-error')).toBeTruthy());
		expect(screen.queryByTestId('updates-paused')).toBeNull();
		await fireEvent.click(screen.getByTestId('updates-pause-7'));
		await until(() => expect(screen.getByTestId('updates-paused')).toBeTruthy());
		await fireEvent.click(screen.getByTestId('updates-resume'));
		await until(() => expect(screen.queryByTestId('updates-paused')).toBeNull());
	});

	it('release notes link only to https, in a new tab', async () => {
		status.releases = [release, { ...release, version: '0.5.9', notes_url: 'http://bad.test' }];
		render(UpdatesScreen);
		const a = (await screen.findByRole('link', { name: 'Read on GitHub' })) as HTMLAnchorElement;
		expect(a.getAttribute('rel')).toBe('noopener noreferrer');
		expect(a.target).toBe('_blank');
		expect(screen.getAllByRole('link', { name: 'Read on GitHub' })).toHaveLength(1);
	});

	it('explains the newest rollback, not the oldest', async () => {
		status.notices = ['rolled_back'];
		status.history = [
			{
				at: 1,
				from: '0.4.0',
				level: 'fix',
				lost_torrents: null,
				reason: 'old reason',
				result: 'rolled_back',
				version: '0.5.0'
			},
			{
				at: 2,
				from: '0.5.2',
				level: 'fix',
				lost_torrents: null,
				reason: 'health check failed',
				result: 'rolled_back',
				version: '0.6.0'
			}
		];
		render(UpdatesScreen);
		const text = (await screen.findByTestId('updates-rolled-back')).textContent ?? '';
		expect(text).toContain('0.6.0');
		expect(text).toContain('health check failed');
		expect(text).not.toContain('old reason');
	});

	it('shows the saved security choice when the status can’t load', async () => {
		fields.mode!.value = 'auto';
		fields.security_fixes!.value = 'off';
		failStatus = true;
		render(UpdatesScreen);
		const t = (await screen.findByTestId('updates-security-toggle')) as HTMLInputElement;
		expect(t.checked).toBe(false);
	});

	it('a failed Check now keeps what was shown, with an error', async () => {
		render(UpdatesScreen);
		const before = (await screen.findByTestId('updates-state')).textContent;
		failNext = '/updates/check';
		await fireEvent.click(screen.getByTestId('updates-check'));
		await until(() => expect(screen.getByTestId('updates-error')).toBeTruthy());
		expect(screen.getByTestId('updates-state').textContent).toBe(before);
	});

	it('a locked level and a locked security choice can’t be changed', async () => {
		fields.mode!.value = 'auto';
		fields.level!.env = 'LODESTAR_UPDATES_LEVEL';
		fields.security_fixes!.env = 'LODESTAR_UPDATES_SECURITY_FIXES';
		render(UpdatesScreen);
		expect(((await screen.findByTestId('updates-level-fixes')) as HTMLInputElement).disabled).toBe(
			true
		);
		expect((screen.getByTestId('updates-security-toggle') as HTMLInputElement).disabled).toBe(true);
		expect(mutations()).toEqual([]);
	});

	it('Clear history: Cancel sends nothing', async () => {
		status.history = [
			{
				at: 1,
				from: '0.5.1',
				level: 'fix',
				lost_torrents: null,
				reason: null,
				result: 'installed',
				version: '0.5.2'
			}
		];
		render(UpdatesScreen);
		await fireEvent.click(await screen.findByTestId('updates-history-clear'));
		await until(() => expect(asked.count).toBe(1));
		expect(mutations()).toEqual([]);
	});

	it('names no version for an install step the server gave none for', async () => {
		let push: (e: unknown) => void = () => {};
		events.on.mockImplementation((_t: string, h: (e: unknown) => void) => {
			push = h;
			return () => {};
		});
		render(UpdatesScreen);
		await screen.findByTestId('updates-state');
		push({ state: 'installing', version: null, bytes_done: null, bytes_total: null, error: null });
		const text = (await screen.findByTestId('updates-progress')).textContent ?? '';
		expect(text).not.toContain('0.5.2');
		expect(text).toContain('Installing an update');
		push({
			state: 'installing',
			version: '0.6.0',
			bytes_done: null,
			bytes_total: null,
			error: null
		});
		await until(() =>
			expect(screen.getByTestId('updates-progress').textContent).toContain('0.6.0')
		);
	});

	it('shows a notice code it has no words for', async () => {
		status.notices = ['brand_new_code'];
		render(UpdatesScreen);
		expect((await screen.findByTestId('updates-notice-brand_new_code')).textContent).toContain(
			'no explanation'
		);
	});

	describe('Roll back', () => {
		const offer = () => {
			status.rollback = { available: true, lost_torrents: 3, reason: null, to_version: '0.5.0' };
		};
		const refuse = (code: string, reason?: string) =>
			(rollbackAnswer = reply(409, { error: { code, message: 'x', reason } }));

		it('is hidden when unavailable', async () => {
			status.rollback = {
				available: false,
				lost_torrents: null,
				reason: 'busy',
				to_version: null
			};
			render(UpdatesScreen);
			await screen.findByTestId('updates-state');
			expect(screen.queryByTestId('updates-rollback')).toBeNull();
		});

		it('names the previous version; Cancel sends nothing', async () => {
			offer();
			render(UpdatesScreen);
			const b = await screen.findByTestId('updates-rollback');
			expect(b.textContent).toContain('0.5.0');
			await fireEvent.click(b);
			await until(() => expect(asked.count).toBe(1));
			expect(mutations()).toEqual([]);
		});

		it('Confirm sends confirm: true', async () => {
			offer();
			asked.answer = 'rollback';
			render(UpdatesScreen);
			await fireEvent.click(await screen.findByTestId('updates-rollback'));
			await until(() => expect(mutations()).toEqual(['POST /updates/rollback {"confirm":true}']));
		});

		it.each([
			['not_supervised', "isn't running under its updater"],
			['image_mode', 'pulling a new container image'],
			['busy', 'check or install is running'],
			['no_previous_version', 'no previous version'],
			['snapshot_missing', 'snapshot of your data'],
			['something_new', 'gave no reason']
		])('shows the refusal %s in words', async (reason, words) => {
			offer();
			asked.answer = 'rollback';
			refuse('rollback_unavailable', reason);
			render(UpdatesScreen);
			await fireEvent.click(await screen.findByTestId('updates-rollback'));
			const t = (await screen.findByTestId('updates-error')).textContent ?? '';
			expect(t).toContain(words);
			expect(mutations()).toHaveLength(1); // no retry
		});

		it('shows updates_unavailable in words', async () => {
			offer();
			asked.answer = 'rollback';
			refuse('updates_unavailable');
			render(UpdatesScreen);
			await fireEvent.click(await screen.findByTestId('updates-rollback'));
			expect((await screen.findByTestId('updates-error')).textContent).toContain(
				'no update engine'
			);
		});
	});

	describe('install_failed', () => {
		const fail = async (error: string) => {
			let push: (e: unknown) => void = () => {};
			events.on.mockImplementation((_t: string, h: (e: unknown) => void) => {
				push = h;
				return () => {};
			});
			render(UpdatesScreen);
			await screen.findByTestId('updates-state');
			push({ state: 'failed', version: '0.6.0', bytes_done: null, bytes_total: null, error });
			return (await screen.findByTestId('updates-error')).textContent ?? '';
		};
		it('words a known code, not the code', async () => {
			const t = await fail('download_mismatch');
			expect(t).toContain("didn't match what was signed");
			expect(t).not.toContain('download_mismatch');
		});
		it('falls back to a generic sentence for an unknown code', async () => {
			const t = await fail('brand_new_code');
			expect(t).toContain('Something went wrong while installing');
			expect(t).not.toContain('brand_new_code');
		});
	});
});
