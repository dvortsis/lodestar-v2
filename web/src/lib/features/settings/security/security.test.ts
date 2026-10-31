// Settings → Security (M-security-settings) against a stubbed fetch: the password confirmation
// (Cancel sends nothing, a wrong password shows the server's words, the lock-out, the field
// cleared), Change password's rules and result, and the read-only secure cookies status.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { initLocale } from '$lib/i18n/locale.svelte';
import { screens } from '../shell/screens';
import SecurityScreen from './SecurityScreen.svelte';

vi.mock('$app/navigation', () => ({ beforeNavigate: () => {}, goto: vi.fn() }));
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
			'Every other session was signed out.'
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
		expect(within(dialog).getByText('Too many wrong passwords.')).toBeTruthy();
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
				'Every other session was signed out'
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

describe('Secure cookies', () => {
	it('shows the status read-only, and the variable that set it', async () => {
		replies['GET /settings/network'] = () => ({
			status: 200,
			body: NETWORK('LODESTAR_SECURE_COOKIES', true)
		});
		render(SecurityScreen);
		await until(() => expect(screen.getByTestId('security-cookies').textContent).toContain('On'));
		expect(screen.getByText(/Set by LODESTAR_SECURE_COOKIES/)).toBeTruthy();
		expect(screen.queryByRole('switch')).toBeNull();
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
