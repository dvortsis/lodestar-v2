// Settings → Security (M-security-settings) against a stubbed fetch: the password confirmation
// (Cancel sends nothing, a wrong password shows the server's words, the lock-out, the field
// cleared), Change password's rules and result, and the read-only secure cookies status.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { initLocale } from '$lib/i18n/locale.svelte';
import { screens } from '../shell/screens';
import SecurityScreen from './SecurityScreen.svelte';

type Nav = {
	type: string;
	from: { url: URL } | null;
	to: { url: URL } | null;
	cancel: () => void;
};
const nav = vi.hoisted(() => ({
	guards: [] as ((n: Nav) => void)[],
	goto: undefined as unknown as import('vitest').Mock
}));
vi.mock('$app/navigation', () => {
	nav.goto = vi.fn(async () => {});
	return { beforeNavigate: (f: (n: Nav) => void) => nav.guards.push(f), goto: nav.goto };
});
vi.setConfig({ testTimeout: 30_000 });
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });

type Reply = { status: number; body: unknown; headers?: Record<string, string> };
let replies: Record<string, (body: unknown) => Reply>;
let calls: { method: string; path: string; body: unknown }[];

const err = (status: number, code: string, message: string, field?: string): Reply => ({
	status,
	body: { error: { code, message, field } }
});
const NETWORK = (env: string | null, value = false) => ({
	section: 'network',
	revision: 1,
	restart_required: [],
	fields: {
		secure_cookies: { value, env, read_only: true, restart_required: true, source: 'default' }
	}
});

beforeEach(() => {
	initLocale('en');
	localStorage.clear();
	sessionStorage.clear();
	calls = [];
	nav.guards.length = 0;
	nav.goto.mockClear();
	replies = {
		'GET /settings/network': () => ({ status: 200, body: NETWORK(null) }),
		'POST /sessions/revoke-others': () => ({ status: 200, body: { ok: true } }),
		'POST /password': () => ({ status: 200, body: { ok: true } })
	};
	vi.stubGlobal(
		'fetch',
		vi.fn(async (url: string, init: RequestInit = {}) => {
			const method = init.method ?? 'GET';
			const path = new URL(url, 'http://x').pathname.replace('/api/v1', '');
			const body = typeof init.body === 'string' ? JSON.parse(init.body) : undefined;
			calls.push({ method, path, body });
			const reply = replies[`${method} ${path}`]?.(body) ?? err(404, 'not_found', 'none');
			return new Response(JSON.stringify(reply.body), {
				status: reply.status,
				headers: { 'Content-Type': 'application/json', ...reply.headers }
			});
		})
	);
});
afterEach(() => {
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
	vi.useRealTimers();
	document.body.innerHTML = '';
});

const writes = () => calls.filter((c) => c.method !== 'GET');

async function openRevoke() {
	render(SecurityScreen);
	await fireEvent.click(await screen.findByTestId('security-revoke-others'));
	const dialog = await screen.findByRole(
		'dialog',
		{ name: 'Sign out other sessions?' },
		{ timeout: 10_000 }
	);
	const input = within(dialog).getByLabelText(/Your password/) as HTMLInputElement;
	await until(() => expect(document.activeElement).toBe(input));
	return { dialog, input };
}

async function confirm(dialog: HTMLElement, input: HTMLInputElement, pw: string) {
	await fireEvent.input(input, { target: { value: pw } });
	await fireEvent.click(within(dialog).getByTestId('password-dialog-confirm'));
}

describe('Security screen', () => {
	it('is registered under System', () => {
		const s = screens.find((s) => s.id === 'security');
		expect(s?.group).toBe('system');
		expect(s?.title()).toBe('Security');
	});

	it('Cancel on the password confirmation sends nothing', async () => {
		const { dialog, input } = await openRevoke();
		await fireEvent.input(input, { target: { value: 'hunter2hunter2' } });
		await fireEvent.click(within(dialog).getByTestId('password-dialog-cancel'));
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(writes()).toEqual([]);
		expect(screen.getByTestId('security-sessions-result').textContent?.trim()).toBe('');
	});

	it('an empty password sends nothing', async () => {
		const { dialog } = await openRevoke();
		await fireEvent.click(within(dialog).getByTestId('password-dialog-confirm'));
		expect(await within(dialog).findByText('Enter your password.')).toBeTruthy();
		expect(writes()).toEqual([]);
	});

	it('a wrong password shows the server error; the field is cleared after each try', async () => {
		replies['POST /sessions/revoke-others'] = (b) =>
			(b as { password: string }).password === 'right-password'
				? { status: 200, body: { ok: true } }
				: err(401, 'bad_password', 'That password is wrong.', 'password');
		const { dialog, input } = await openRevoke();
		await confirm(dialog, input, 'wrong-password');
		expect(await within(dialog).findByText('That password is wrong.')).toBeTruthy();
		expect(input.value).toBe('');
		await until(() => expect(document.activeElement).toBe(input));
		await confirm(dialog, input, 'right-password');
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(writes()).toEqual([
			{ method: 'POST', path: '/sessions/revoke-others', body: { password: 'wrong-password' } },
			{ method: 'POST', path: '/sessions/revoke-others', body: { password: 'right-password' } }
		]);
		expect(screen.getByTestId('security-sessions-result').textContent).toContain(
			'Every other browser signed in with the password was signed out.'
		);
	});

	it('shows the lock-out and holds the button until it ends', async () => {
		replies['POST /sessions/revoke-others'] = () => ({
			...err(429, 'rate_limited', 'Too many attempts.'),
			headers: { 'Retry-After': '30' }
		});
		const { dialog, input } = await openRevoke();
		await confirm(dialog, input, 'any-password');
		const countdown = await within(dialog).findByTestId('lockout-countdown');
		expect(countdown.textContent).toMatch(/Try again in (30|29) s\./);
		expect(within(dialog).getByText(/Too many attempts\./)).toBeTruthy();
		// It's not the password's fault: the field isn't marked.
		expect(input.getAttribute('aria-invalid')).toBeNull();
		const button = within(dialog).getByTestId('password-dialog-confirm');
		expect(button.getAttribute('aria-disabled')).toBe('true');
		await confirm(dialog, input, 'again');
		expect(writes()).toHaveLength(1);
	});

	it('keeps no password in storage or the console', async () => {
		const spies = (['log', 'info', 'warn', 'error', 'debug'] as const).map((k) =>
			vi.spyOn(console, k)
		);
		const { dialog, input } = await openRevoke();
		await confirm(dialog, input, 'secret-pw-123');
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		const stored = JSON.stringify({ ...localStorage }) + JSON.stringify({ ...sessionStorage });
		expect(stored).not.toContain('secret-pw-123');
		expect(location.href).not.toContain('secret-pw-123');
		for (const spy of spies) expect(JSON.stringify(spy.mock.calls)).not.toContain('secret-pw-123');
		expect(document.body.innerHTML).not.toContain('secret-pw-123');
	});

	it('a session that ended mid-flow closes the dialog', async () => {
		replies['POST /sessions/revoke-others'] = () => err(401, 'login_required', 'Sign in again.');
		const { dialog, input } = await openRevoke();
		await confirm(dialog, input, 'any-password');
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
	});
});

describe('Change password', () => {
	const fields = () => ({
		current: screen.getByLabelText(/Current password/) as HTMLInputElement,
		next: screen.getByLabelText(/^New password/) as HTMLInputElement,
		again: screen.getByLabelText(/Confirm new password/) as HTMLInputElement
	});
	async function fill(current: string, next: string, again = next) {
		render(SecurityScreen);
		await screen.findByTestId('security-password');
		const f = fields();
		await fireEvent.input(f.current, { target: { value: current } });
		await fireEvent.input(f.next, { target: { value: next } });
		await fireEvent.input(f.again, { target: { value: again } });
		return f;
	}
	const submit = () => fireEvent.click(screen.getByTestId('security-password-submit'));

	it('shows the rules before anything is sent', async () => {
		render(SecurityScreen);
		await screen.findByTestId('security-password');
		expect(screen.getByText(/at least 10 characters/i)).toBeTruthy();
		await fireEvent.input(fields().next, { target: { value: 'aaaaaaaaaaaa' } });
		expect(screen.getByText(/Strength: weak/)).toBeTruthy();
	});

	it('refuses a short or mismatched password without sending', async () => {
		await fill('old-password', 'short');
		await submit();
		expect(
			screen.getByText(/at least 10 characters/i, { selector: '[id$="-error"] *, [id$="-error"]' })
		).toBeTruthy();
		await fireEvent.input(fields().next, { target: { value: 'long-enough-1' } });
		await submit();
		expect(screen.getByText(/match/i)).toBeTruthy();
		expect(writes()).toEqual([]);
	});

	it('changes it, clears every field and says the other sessions were signed out', async () => {
		const f = await fill('old-password', 'new-password-1');
		await submit();
		await until(() =>
			expect(screen.getByTestId('security-password-result').textContent).toContain(
				'Every other browser signed in with the password was signed out'
			)
		);
		expect(writes()).toEqual([
			{ method: 'POST', path: '/password', body: { old: 'old-password', new: 'new-password-1' } }
		]);
		expect([f.current.value, f.next.value, f.again.value]).toEqual(['', '', '']);
	});

	it('a wrong current password shows the server error on that field, cleared', async () => {
		replies['POST /password'] = () => err(401, 'bad_password', 'That password is wrong.', 'old');
		const f = await fill('bad-password', 'new-password-1');
		await submit();
		expect(await screen.findByText('That password is wrong.')).toBeTruthy();
		expect(f.current.getAttribute('aria-invalid')).toBe('true');
		expect([f.current.value, f.next.value, f.again.value]).toEqual(['', '', '']);
	});

	it('a weak password the server refuses shows on New password', async () => {
		replies['POST /password'] = () =>
			err(400, 'weak_password', 'The password needs at least 10 characters.', 'new');
		const f = await fill('old-password', 'new-password-1');
		await submit();
		expect(await screen.findByText('The password needs at least 10 characters.')).toBeTruthy();
		expect(f.next.getAttribute('aria-invalid')).toBe('true');
		expect(f.current.getAttribute('aria-invalid')).toBeNull();
	});

	it('the lock-out ends, and the next try goes through', async () => {
		let n = 0;
		replies['POST /password'] = () =>
			n++ === 0
				? { ...err(429, 'rate_limited', 'Too many attempts.'), headers: { 'Retry-After': '1' } }
				: { status: 200, body: { ok: true } };
		await fill('old-password', 'new-password-1');
		await submit();
		await screen.findByTestId('lockout-countdown');
		await until(() => expect(screen.getByText('You can try again now.')).toBeTruthy());
		const button = screen.getByTestId('security-password-submit');
		await until(() => expect(button.getAttribute('aria-disabled')).toBeNull());
		const f = fields();
		await fireEvent.input(f.current, { target: { value: 'old-password' } });
		await fireEvent.input(f.next, { target: { value: 'new-password-1' } });
		await fireEvent.input(f.again, { target: { value: 'new-password-1' } });
		await submit();
		await until(() =>
			expect(screen.getByTestId('security-password-result').textContent).toContain('signed out')
		);
		expect(screen.queryByText('You can try again now.')).toBeNull();
		expect(writes()).toHaveLength(2);
	});

	it('shows the lock-out', async () => {
		replies['POST /password'] = () => err(429, 'rate_limited', 'Too many attempts.');
		await fill('old-password', 'new-password-1');
		await submit();
		expect((await screen.findByTestId('lockout-countdown')).textContent).toMatch(
			/Try again in (12|11) s\./
		);
		expect(screen.getByTestId('security-password-submit').getAttribute('aria-disabled')).toBe(
			'true'
		);
	});
});

describe('the leave guard', () => {
	const go = (type: string, to: string, from = `${location.origin}/settings/security`) => {
		const cancel = vi.fn();
		for (const g of nav.guards)
			g({ type, from: { url: new URL(from) }, to: { url: new URL(to) }, cancel });
		return cancel;
	};
	async function typed() {
		render(SecurityScreen);
		await screen.findByTestId('security-password');
		const current = screen.getByLabelText(/Current password/) as HTMLInputElement;
		await fireEvent.input(current, { target: { value: 'half-typed' } });
		return current;
	}

	it('lets the user leave freely with nothing typed', async () => {
		render(SecurityScreen);
		await screen.findByTestId('security-password');
		expect(go('link', `${location.origin}/settings/general`)).not.toHaveBeenCalled();
	});

	it('asks first; Keep editing stays, Discard clears the fields and goes', async () => {
		const current = await typed();
		expect(go('link', `${location.origin}/settings/general`)).toHaveBeenCalled();
		let dialog = await screen.findByRole(
			'dialog',
			{ name: 'Leave without saving?' },
			{ timeout: 10_000 }
		);
		expect(
			within(dialog).getByText('What you typed in Change password will be cleared.')
		).toBeTruthy();
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Keep editing' }));
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(nav.goto).not.toHaveBeenCalled();
		expect(current.value).toBe('half-typed');

		go('link', `${location.origin}/settings/general`);
		dialog = await screen.findByRole('dialog', {}, { timeout: 10_000 });
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Discard' }));
		await until(() => expect(nav.goto).toHaveBeenCalledWith(`${location.origin}/settings/general`));
		expect(current.value).toBe('');
	});

	it('a hash change on this page is not leaving; closing the tab lets the browser ask', async () => {
		await typed();
		expect(go('link', `${location.origin}/settings/security#x`)).not.toHaveBeenCalled();
		expect(go('leave', `${location.origin}/settings/security`)).toHaveBeenCalled();
		expect(screen.queryByRole('dialog')).toBeNull();
	});
});

describe('Secure cookies', () => {
	it('says where it is set when no variable set it', async () => {
		render(SecurityScreen);
		await until(() => expect(screen.getByTestId('security-cookies').textContent).toContain('Off'));
		expect(screen.getByText(/http\.secure_cookies in config\.toml/)).toBeTruthy();
	});

	it('shows the status read-only, and the variable that set it', async () => {
		replies['GET /settings/network'] = () => ({
			status: 200,
			body: NETWORK('LODESTAR_SECURE_COOKIES', true)
		});
		render(SecurityScreen);
		await until(() => expect(screen.getByTestId('security-cookies').textContent).toContain('On'));
		expect(screen.getByText(/Set by LODESTAR_SECURE_COOKIES/)).toBeTruthy();
		const cookies = screen.getByTestId('security-cookies').closest('section')!;
		expect(within(cookies as HTMLElement).queryByRole('switch')).toBeNull();
	});

	it('says so when it can’t be read', async () => {
		replies['GET /settings/network'] = () => err(500, 'internal', 'boom');
		render(SecurityScreen);
		await until(() =>
			expect(screen.getByTestId('security-cookies').textContent).toContain("Couldn't read")
		);
	});
});

describe('the password confirmation and the screen', () => {
	it('closes unanswered when the screen goes, sending nothing', async () => {
		const view = render(SecurityScreen);
		await fireEvent.click(await screen.findByTestId('security-revoke-others'));
		const dialog = await screen.findByRole('dialog', {}, { timeout: 10_000 });
		await fireEvent.input(within(dialog).getByLabelText(/Your password/), {
			target: { value: 'typed-then-left' }
		});
		view.unmount();
		await until(() => expect(document.body.innerHTML).not.toContain('typed-then-left'));
		expect(screen.queryByRole('dialog')).toBeNull();
		expect(writes()).toEqual([]);
	});
});

const SECRET = 'lsk_AbCdEfGh_0123456789abcdefghijklmnopqrstuvwxyzABCDEFG';
const KEY = (id: number, name: string, extra: Record<string, unknown> = {}) => ({
	id,
	name,
	kind: 'api_key',
	prefix: `lsk_${String(id).padStart(8, 'k')}`,
	scopes: ['torznab'],
	status: 'active',
	created_at: 1_790_000_000,
	expires_at: null,
	last_used_at: null,
	revoked_at: null,
	saved_search_id: null,
	...extra
});
const PROXY = (extra: Record<string, unknown> = {}) => ({
	enabled: false,
	forced_off: false,
	header: 'Remote-User',
	trusted: [],
	allowed_users: [],
	...extra
});
/** Every place a secret could be kept on the client. */
const leaked = (secret: string) =>
	[
		document.body.innerHTML,
		JSON.stringify({ ...localStorage }),
		JSON.stringify({ ...sessionStorage }),
		location.href
	].filter((s) => s.includes(secret));

async function passwordDialog(title: string) {
	const dialog = await screen.findByRole('dialog', { name: title }, { timeout: 10_000 });
	const input = within(dialog).getByLabelText(/Your password/) as HTMLInputElement;
	await until(() => expect(document.activeElement).toBe(input));
	return { dialog, input };
}

describe('API keys and feed tokens', () => {
	beforeEach(() => {
		replies['GET /keys'] = () => ({
			status: 200,
			body: {
				keys: [KEY(1, 'Sonarr'), KEY(2, 'Old', { status: 'revoked', revoked_at: 1_790_000_100 })]
			}
		});
		replies['GET /auth/proxy'] = () => ({ status: 200, body: PROXY() });
	});

	async function keysSection() {
		render(SecurityScreen);
		const section = await screen.findByTestId('security-keys', {}, { timeout: 10_000 });
		await within(section).findByText('Sonarr');
		return section;
	}

	it('lists the keys; a revoked one has no actions', async () => {
		const section = await keysSection();
		const rows = within(section).getAllByTestId('security-key');
		expect(rows).toHaveLength(2);
		expect(rows[0]!.textContent).toContain('lsk_kkkkkkk1…');
		expect(rows[0]!.textContent).toContain('Never used');
		expect(within(rows[0]!).getByRole('button', { name: 'Regenerate Sonarr…' })).toBeTruthy();
		expect(rows[1]!.textContent).toContain('Revoked');
		expect(within(rows[1]!).queryByRole('button')).toBeNull();
	});

	it('says so when the list can’t be read, and tries again', async () => {
		replies['GET /keys'] = () => err(500, 'internal', 'boom');
		render(SecurityScreen);
		expect(
			await screen.findByText("Couldn't load the keys.", {}, { timeout: 10_000 })
		).toBeTruthy();
		replies['GET /keys'] = () => ({ status: 200, body: { keys: [] } });
		await fireEvent.click(screen.getAllByRole('button', { name: 'Try again' })[0]!);
		expect(await screen.findByTestId('security-keys-empty')).toBeTruthy();
	});

	it('refuses a nameless key or one with no scope without sending', async () => {
		const section = await keysSection();
		await fireEvent.click(within(section).getByTestId('security-key-create'));
		expect(within(section).getByText('Enter a name.')).toBeTruthy();
		await fireEvent.input(within(section).getByLabelText(/^Name/), { target: { value: 'Radarr' } });
		await fireEvent.click(within(section).getByLabelText('Search API'));
		await fireEvent.click(within(section).getByTestId('security-key-create'));
		expect(within(section).getByText('Choose at least one.')).toBeTruthy();
		expect(writes()).toEqual([]);
		expect(screen.queryByRole('dialog')).toBeNull();
	});

	it('create needs the password; Cancel sends nothing', async () => {
		const section = await keysSection();
		await fireEvent.input(within(section).getByLabelText(/^Name/), { target: { value: 'Radarr' } });
		await fireEvent.click(within(section).getByTestId('security-key-create'));
		const { dialog, input } = await passwordDialog('Create Radarr?');
		await fireEvent.input(input, { target: { value: 'hunter2hunter2' } });
		await fireEvent.click(within(dialog).getByTestId('password-dialog-cancel'));
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(writes()).toEqual([]);
	});

	it('shows a new secret once, with Copy, and keeps it nowhere afterwards', async () => {
		const logs = (['log', 'info', 'warn', 'error', 'debug'] as const).map((k) =>
			vi.spyOn(console, k)
		);
		const writeText = vi.fn(async () => {});
		vi.stubGlobal('navigator', { ...navigator, clipboard: { writeText } });
		replies['POST /keys'] = () => ({
			status: 201,
			body: { key: KEY(3, 'Feeds', { kind: 'feed_token', scopes: ['feeds'] }), secret: SECRET }
		});
		const section = await keysSection();
		await fireEvent.input(within(section).getByLabelText(/^Name/), { target: { value: 'Feeds' } });
		await fireEvent.click(within(section).getByLabelText(/^Feed token/));
		await fireEvent.click(within(section).getByTestId('security-key-create'));
		const { dialog, input } = await passwordDialog('Create Feeds?');
		await fireEvent.input(input, { target: { value: 'right-password' } });
		await fireEvent.click(within(dialog).getByTestId('password-dialog-confirm'));
		const reveal = await screen.findByRole(
			'dialog',
			{ name: 'Copy the secret for Feeds' },
			{ timeout: 10_000 }
		);
		expect(writes()).toEqual([
			{
				method: 'POST',
				path: '/keys',
				body: { name: 'Feeds', kind: 'feed_token', password: 'right-password' }
			}
		]);
		const shown = within(reveal).getByTestId('key-secret') as HTMLInputElement;
		expect(shown.value).toBe(SECRET);
		await until(() => expect(document.activeElement).toBe(shown));
		await fireEvent.click(within(reveal).getByTestId('key-secret-copy'));
		expect(writeText).toHaveBeenCalledWith(SECRET);
		expect(await within(reveal).findByText('Copied.')).toBeTruthy();

		await fireEvent.click(within(reveal).getByTestId('key-secret-done'));
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		await until(() => expect(leaked(SECRET)).toEqual([]));
		expect(logs.flatMap((l) => l.mock.calls.flat()).join(' ')).not.toContain(SECRET);
		expect(within(section).getByText('Feeds')).toBeTruthy();
		expect(within(section).getByLabelText(/^Name/)).toHaveProperty('value', '');
	});

	it('regenerate needs the password and replaces the row; Cancel sends nothing', async () => {
		replies['POST /keys/1/regenerate'] = () => ({
			status: 200,
			body: { key: KEY(7, 'Sonarr'), secret: SECRET }
		});
		const section = await keysSection();
		const button = within(section).getByRole('button', { name: 'Regenerate Sonarr…' });
		await fireEvent.click(button);
		let { dialog, input } = await passwordDialog('Regenerate Sonarr?');
		await fireEvent.click(within(dialog).getByTestId('password-dialog-cancel'));
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(writes()).toEqual([]);

		await fireEvent.click(button);
		({ dialog, input } = await passwordDialog('Regenerate Sonarr?'));
		await confirm(dialog, input, 'right-password');
		const reveal = await screen.findByRole('dialog', { name: 'Copy the secret for Sonarr' });
		expect(writes()).toEqual([
			{ method: 'POST', path: '/keys/1/regenerate', body: { password: 'right-password' } }
		]);
		await fireEvent.keyDown(reveal, { key: 'Escape' });
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		await until(() => expect(leaked(SECRET)).toEqual([]));
		expect(within(section).getByText('lsk_kkkkkkk7…', { exact: false })).toBeTruthy();
		expect(screen.getByTestId('security-keys-result').textContent).toContain(
			'The old secret no longer works.'
		);
	});

	it('a session that ended mid-flow reveals nothing', async () => {
		replies['POST /keys'] = () => err(401, 'login_required', 'Sign in again.');
		const section = await keysSection();
		await fireEvent.input(within(section).getByLabelText(/^Name/), { target: { value: 'Radarr' } });
		await fireEvent.click(within(section).getByLabelText('Search API'));
		await fireEvent.click(within(section).getByLabelText('Torznab'));
		await fireEvent.click(within(section).getByTestId('security-key-create'));
		const { dialog, input } = await passwordDialog('Create Radarr?');
		await confirm(dialog, input, 'right-password');
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(writes().map((w) => w.body)).toEqual([
			{ name: 'Radarr', kind: 'api_key', scopes: ['torznab'], password: 'right-password' }
		]);
	});

	it('a refusal that isn’t the password’s closes the dialog and shows on the screen', async () => {
		replies['POST /keys'] = () => err(409, 'too_many_keys', 'There are 500 keys already.');
		replies['POST /keys/1/regenerate'] = () =>
			err(409, 'key_inactive', 'This key is no longer active.');
		const section = await keysSection();
		await fireEvent.input(within(section).getByLabelText(/^Name/), { target: { value: 'Radarr' } });
		await fireEvent.click(within(section).getByTestId('security-key-create'));
		let { dialog, input } = await passwordDialog('Create Radarr?');
		await confirm(dialog, input, 'right-password');
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(screen.getByTestId('security-keys-result').textContent).toContain(
			'Nothing was changed. There are 500 keys already.'
		);

		await fireEvent.click(within(section).getByRole('button', { name: 'Regenerate Sonarr…' }));
		({ dialog, input } = await passwordDialog('Regenerate Sonarr?'));
		await confirm(dialog, input, 'right-password');
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(screen.getByTestId('security-keys-result').textContent).toContain(
			'This key is no longer active.'
		);
		expect(writes()).toHaveLength(2);
	});

	it('a refused name shows at the name field', async () => {
		replies['POST /keys'] = () => err(400, 'invalid', 'That name is taken.', 'name');
		const section = await keysSection();
		const field = within(section).getByLabelText(/^Name/);
		await fireEvent.input(field, { target: { value: 'Sonarr' } });
		await fireEvent.click(within(section).getByTestId('security-key-create'));
		const { dialog, input } = await passwordDialog('Create Sonarr?');
		await confirm(dialog, input, 'right-password');
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(within(section).getByText('That name is taken.')).toBeTruthy();
		expect(field.getAttribute('aria-invalid')).toBe('true');
	});

	it('revoke asks first, without the password (B12); Keep it sends nothing', async () => {
		replies['POST /keys/1/revoke'] = () => ({
			status: 200,
			body: KEY(1, 'Sonarr', { status: 'revoked', revoked_at: 1_790_000_200 })
		});
		const section = await keysSection();
		const button = within(section).getByRole('button', { name: 'Revoke Sonarr…' });
		await fireEvent.click(button);
		let dialog = await screen.findByRole('dialog', { name: 'Revoke Sonarr?' }, { timeout: 10_000 });
		expect(within(dialog).queryByLabelText(/Your password/)).toBeNull();
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Keep it' }));
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(writes()).toEqual([]);

		await fireEvent.click(button);
		dialog = await screen.findByRole('dialog', { name: 'Revoke Sonarr?' }, { timeout: 10_000 });
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Revoke' }));
		await until(() =>
			expect(screen.getByTestId('security-keys-result').textContent).toContain('Revoked Sonarr.')
		);
		expect(writes()).toEqual([{ method: 'POST', path: '/keys/1/revoke', body: {} }]);
		expect(within(section).queryByRole('button', { name: 'Revoke Sonarr…' })).toBeNull();
	});
});

describe('Trusted proxy sign-in', () => {
	beforeEach(() => {
		replies['GET /keys'] = () => ({ status: 200, body: { keys: [] } });
		replies['GET /auth/proxy'] = () => ({ status: 200, body: PROXY() });
		replies['PUT /auth/proxy'] = (body) => {
			const rest = { ...(body as Record<string, unknown>) };
			delete rest.password;
			return { status: 200, body: PROXY(rest) };
		};
	});

	async function proxySection() {
		render(SecurityScreen);
		const form = await screen.findByTestId('security-proxy', {}, { timeout: 10_000 });
		const toggle = (await within(form).findByRole('switch')) as HTMLInputElement;
		return { form, toggle };
	}

	it('explains the choice and recommends leaving it off', async () => {
		const { form } = await proxySection();
		expect(form.textContent).toContain('Recommended: leave this off');
		expect(form.textContent).toContain("Don't publish Lodestar's port");
		expect(form.textContent).toContain('Never trust the Docker bridge gateway');
	});

	it('LODESTAR_PROXY_AUTH=off shows as locked, with its source', async () => {
		replies['GET /auth/proxy'] = () => ({
			status: 200,
			body: PROXY({ enabled: true, forced_off: true, trusted: ['172.20.0.2/32'] })
		});
		const { form, toggle } = await proxySection();
		expect(within(form).getByText(/set by LODESTAR_PROXY_AUTH=off/)).toBeTruthy();
		expect(toggle.disabled).toBe(true);
		expect(toggle.checked).toBe(true);
		expect(toggle.closest('label')!.textContent).toContain('Saved setting');
	});

	it('says so when the settings can’t be read', async () => {
		replies['GET /auth/proxy'] = () => err(500, 'internal', 'boom');
		render(SecurityScreen);
		expect(
			await screen.findByText("Couldn't read the trusted proxy settings.", {}, { timeout: 10_000 })
		).toBeTruthy();
	});

	it('turning it on warns, needs a trusted address and the password; Cancel sends nothing', async () => {
		const { form, toggle } = await proxySection();
		expect(within(form).queryByTestId('security-proxy-warning')).toBeNull();
		await fireEvent.click(toggle);
		expect(within(form).getByTestId('security-proxy-warning').textContent).toContain(
			'without a password'
		);
		await fireEvent.click(within(form).getByTestId('security-proxy-save'));
		expect(within(form).getByText("Add the proxy's address first.")).toBeTruthy();
		expect(screen.queryByRole('dialog')).toBeNull();

		await fireEvent.input(within(form).getByLabelText(/Trusted proxies/), {
			target: { value: ' 172.20.0.2 \n\n' }
		});
		await fireEvent.click(within(form).getByTestId('security-proxy-save'));
		let { dialog, input } = await passwordDialog('Turn on trusted proxy sign-in?');
		expect(dialog.textContent).toContain(
			'Anything at 172.20.0.2 will be able to sign in as any allowed user, without a password.'
		);
		await fireEvent.click(within(dialog).getByTestId('password-dialog-cancel'));
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(writes()).toEqual([]);

		await fireEvent.click(within(form).getByTestId('security-proxy-save'));
		({ dialog, input } = await passwordDialog('Turn on trusted proxy sign-in?'));
		await confirm(dialog, input, 'right-password');
		await until(() =>
			expect(screen.getByTestId('security-proxy-result').textContent).toContain('Saved.')
		);
		expect(writes()).toEqual([
			{
				method: 'PUT',
				path: '/auth/proxy',
				body: {
					enabled: true,
					trusted: ['172.20.0.2'],
					header: 'Remote-User',
					allowed_users: [],
					password: 'right-password'
				}
			}
		]);
		expect(within(form).queryByTestId('security-proxy-warning')).toBeNull();
	});

	it('a new trusted address while on warns as well', async () => {
		replies['GET /auth/proxy'] = () => ({
			status: 200,
			body: PROXY({ enabled: true, trusted: ['172.20.0.2/32'] })
		});
		const { form } = await proxySection();
		const area = within(form).getByLabelText(/Trusted proxies/);
		await fireEvent.input(area, { target: { value: '172.20.0.2/32' } });
		expect(within(form).queryByTestId('security-proxy-warning')).toBeNull();
		await fireEvent.input(area, { target: { value: '172.20.0.2/32\n10.0.0.0/8' } });
		expect(within(form).getByTestId('security-proxy-warning')).toBeTruthy();
		await fireEvent.click(within(form).getByTestId('security-proxy-save'));
		await passwordDialog('Let more people sign in through the proxy?');
	});

	it('turning it off is a plain save, with no warning', async () => {
		replies['GET /auth/proxy'] = () => ({
			status: 200,
			body: PROXY({ enabled: true, trusted: ['172.20.0.2/32'] })
		});
		const { form, toggle } = await proxySection();
		await fireEvent.click(toggle);
		expect(within(form).queryByTestId('security-proxy-warning')).toBeNull();
		await fireEvent.click(within(form).getByTestId('security-proxy-save'));
		const { dialog, input } = await passwordDialog('Save trusted proxy sign-in?');
		expect(dialog.textContent).not.toContain('without a password');
		await confirm(dialog, input, 'right-password');
		await until(() =>
			expect(screen.getByTestId('security-proxy-result').textContent).toContain('Saved.')
		);
		expect((writes()[0]!.body as { enabled: boolean }).enabled).toBe(false);
	});

	it('a new user header while on warns', async () => {
		replies['GET /auth/proxy'] = () => ({
			status: 200,
			body: PROXY({ enabled: true, trusted: ['172.20.0.2/32'] })
		});
		const { form } = await proxySection();
		await fireEvent.input(within(form).getByLabelText(/User header/), {
			target: { value: 'X-Forwarded-User' }
		});
		expect(within(form).getByTestId('security-proxy-warning')).toBeTruthy();
	});

	it('a refused value closes the dialog and shows at its field', async () => {
		replies['PUT /auth/proxy'] = () =>
			err(400, 'invalid', 'A /0 range would trust everyone.', 'trusted');
		const { form, toggle } = await proxySection();
		await fireEvent.click(toggle);
		await fireEvent.input(within(form).getByLabelText(/Trusted proxies/), {
			target: { value: '0.0.0.0/0' }
		});
		await fireEvent.click(within(form).getByTestId('security-proxy-save'));
		const { dialog, input } = await passwordDialog('Turn on trusted proxy sign-in?');
		await confirm(dialog, input, 'right-password');
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(within(form).getByText('A /0 range would trust everyone.')).toBeTruthy();
		expect(document.activeElement).toBe(within(form).getByLabelText(/Trusted proxies/));
	});

	it('leaving with a change asks once, naming every part with something typed', async () => {
		const { form } = await proxySection();
		await fireEvent.input(within(form).getByLabelText(/Allowed users/), {
			target: { value: 'alice' }
		});
		await fireEvent.input(screen.getByLabelText(/Current password/), {
			target: { value: 'half-typed' }
		});
		const cancel = vi.fn();
		for (const g of nav.guards)
			g({
				type: 'link',
				from: { url: new URL(`${location.origin}/settings/security`) },
				to: { url: new URL(`${location.origin}/settings/general`) },
				cancel
			});
		const dialog = await screen.findByRole(
			'dialog',
			{ name: 'Leave without saving?' },
			{ timeout: 10_000 }
		);
		expect(screen.getAllByRole('dialog')).toHaveLength(1);
		expect(dialog.textContent).toContain('What you typed in Change password will be cleared.');
		expect(dialog.textContent).toContain(
			'What you typed in Trusted proxy sign-in will be cleared.'
		);
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Discard' }));
		await until(() => expect(nav.goto).toHaveBeenCalled());
		expect(within(form).getByLabelText(/Allowed users/)).toHaveProperty('value', '');
	});
});
