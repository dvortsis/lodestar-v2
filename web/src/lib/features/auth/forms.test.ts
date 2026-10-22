// Component tests: errors land on the right field or in the form alert, the 429 countdown runs
// and unlocks, the submit can't fire twice, and nothing typed is echoed back.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';

const nav = vi.hoisted(() => ({
	toSetup: vi.fn(),
	toLogin: vi.fn(),
	toNext: vi.fn(),
	toWelcome: vi.fn(),
	toLoginFrom: vi.fn()
}));
vi.mock('./nav', () => nav);

import LoginForm from './LoginForm.svelte';
import SetupForm from './SetupForm.svelte';

type Reply = { status: number; body: unknown; headers?: Record<string, string> };
const ok: Reply = { status: 200, body: { ok: true } };
const fail = (status: number, code: string, headers?: Record<string, string>): Reply => ({
	status,
	body: { error: { code, message: `server says ${code}` } },
	headers
});

let replies: Reply[];
let fetchMock: ReturnType<typeof vi.fn>;
beforeEach(() => {
	vi.clearAllMocks();
	replies = [];
	fetchMock = vi.fn(async () => {
		const r = replies.shift() ?? ok;
		return new Response(JSON.stringify(r.body), {
			status: r.status,
			headers: { 'Content-Type': 'application/json', ...r.headers }
		});
	});
	vi.stubGlobal('fetch', fetchMock);
});
afterEach(() => {
	vi.unstubAllGlobals();
	vi.useRealTimers();
});

const sentBody = (i = 0) =>
	JSON.parse((fetchMock.mock.calls[i] as [string, RequestInit])[1].body as string);

describe('LoginForm', () => {
	const password = () => screen.getByLabelText(/^Password/) as HTMLInputElement;
	const submit = () => screen.getByRole('button', { name: 'Sign in' });

	it('signs in and returns to next', async () => {
		render(LoginForm, { next: '/search?q=a' });
		expect(password().autocomplete).toBe('current-password');
		expect(password().type).toBe('password');
		await fireEvent.input(password(), { target: { value: 'correct horse' } });
		await fireEvent.click(submit());
		await waitFor(() => expect(nav.toNext).toHaveBeenCalledWith('/search?q=a'));
		expect(fetchMock.mock.calls[0]![0]).toBe('/api/v1/login');
		expect(sentBody()).toEqual({ password: 'correct horse' });
	});

	it('asks for a password before sending anything', async () => {
		render(LoginForm, { next: '/' });
		await fireEvent.click(submit());
		expect(await screen.findByText('Enter your password.')).toBeTruthy();
		expect(password().getAttribute('aria-invalid')).toBe('true');
		expect(fetchMock).not.toHaveBeenCalled();
	});

	it('shows a wrong password on the field, focused, with no server text', async () => {
		replies = [fail(401, 'bad_password')];
		render(LoginForm, { next: '/' });
		await fireEvent.input(password(), { target: { value: 'nope nope nope' } });
		await fireEvent.click(submit());
		expect(await screen.findByText('That password is wrong. Check it and try again.')).toBeTruthy();
		await waitFor(() => expect(document.activeElement).toBe(password()));
		expect(document.body.textContent).not.toContain('server says');
		expect(document.body.textContent).not.toContain('nope nope nope');
		// Typing again clears the error.
		await fireEvent.input(password(), { target: { value: 'nope nope nopes' } });
		expect(screen.queryByText(/That password is wrong/)).toBeNull();
	});

	it('cannot double-submit: the button is aria-disabled and a second submit is dropped', async () => {
		let release!: (r: Response) => void;
		fetchMock.mockImplementationOnce(() => new Promise<Response>((r) => (release = r)));
		render(LoginForm, { next: '/' });
		await fireEvent.input(password(), { target: { value: 'correct horse' } });
		await fireEvent.click(submit());
		await waitFor(() => expect(submit().getAttribute('aria-disabled')).toBe('true'));
		expect(submit().getAttribute('aria-busy')).toBe('true');
		await fireEvent.click(submit());
		await fireEvent.submit(submit().closest('form')!);
		expect(fetchMock).toHaveBeenCalledTimes(1);
		release(new Response('{"ok":true}', { status: 200 }));
		await waitFor(() => expect(nav.toNext).toHaveBeenCalledOnce());
	});

	it('counts down a 429 from Retry-After, then unlocks', async () => {
		vi.useFakeTimers({ now: 0, toFake: ['setInterval', 'clearInterval', 'Date'] });
		replies = [fail(429, 'rate_limited', { 'Retry-After': '3' })];
		render(LoginForm, { next: '/' });
		await fireEvent.input(password(), { target: { value: 'correct horse' } });
		await fireEvent.click(submit());
		const countdown = await screen.findByTestId('auth-countdown');
		expect(countdown.textContent).toBe('Try again in 3 s.');
		expect(countdown.getAttribute('aria-hidden')).toBe('true');
		expect(screen.getByRole('alert').textContent).toContain('Too many attempts');
		expect(submit().getAttribute('aria-disabled')).toBe('true');
		await fireEvent.click(submit());
		expect(fetchMock).toHaveBeenCalledTimes(1);
		await vi.advanceTimersByTimeAsync(1000);
		expect(screen.getByTestId('auth-countdown').textContent).toBe('Try again in 2 s.');
		await vi.advanceTimersByTimeAsync(2000);
		expect(screen.queryByTestId('auth-countdown')).toBeNull();
		expect(screen.getByText('You can try again now.')).toBeTruthy();
		expect(submit().getAttribute('aria-disabled')).toBeNull();
	});

	it('uses the server refill interval when a 429 has no Retry-After', async () => {
		vi.useFakeTimers({ now: 0, toFake: ['setInterval', 'clearInterval', 'Date'] });
		replies = [fail(429, 'rate_limited')];
		render(LoginForm, { next: '/' });
		await fireEvent.input(password(), { target: { value: 'correct horse' } });
		await fireEvent.click(submit());
		expect((await screen.findByTestId('auth-countdown')).textContent).toBe('Try again in 12 s.');
	});

	it('goes to setup when the instance is not set up', async () => {
		replies = [fail(409, 'not_set_up')];
		render(LoginForm, { next: '/' });
		await fireEvent.input(password(), { target: { value: 'correct horse' } });
		await fireEvent.click(submit());
		await waitFor(() => expect(nav.toSetup).toHaveBeenCalledOnce());
	});

	it('says when the server is unreachable', async () => {
		fetchMock.mockRejectedValueOnce(new TypeError('Failed to fetch'));
		render(LoginForm, { next: '/' });
		await fireEvent.input(password(), { target: { value: 'correct horse' } });
		await fireEvent.click(submit());
		expect((await screen.findByRole('alert')).textContent).toContain("Lodestar isn't responding");
	});
});

describe('SetupForm', () => {
	const tokenInput = () => screen.getByLabelText(/^Setup token/) as HTMLInputElement;
	const newPw = () => screen.getByLabelText(/^New admin password/) as HTMLInputElement;
	const confirmPw = () => screen.getByLabelText(/^Confirm password/) as HTMLInputElement;
	const create = () => screen.getByRole('button', { name: 'Create password and sign in' });

	async function toPasswordStep(token = 'first run: enter setup token ab12-cd34-ef56-7890 now') {
		render(SetupForm);
		await fireEvent.input(tokenInput(), { target: { value: token } });
		await fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
		await waitFor(() => expect(document.activeElement).toBe(newPw()));
	}

	async function fill(pw: string, confirm = pw) {
		await fireEvent.input(newPw(), { target: { value: pw } });
		await fireEvent.input(confirmPw(), { target: { value: confirm } });
		await fireEvent.click(create());
	}

	it('runs the happy path: token, password, then /welcome', async () => {
		await toPasswordStep();
		expect(newPw().autocomplete).toBe('new-password');
		expect(confirmPw().autocomplete).toBe('new-password');
		expect(screen.getByRole('listitem', { current: 'step' }).textContent).toContain(
			'Admin password'
		);
		await fill('correct horse battery');
		await waitFor(() => expect(nav.toWelcome).toHaveBeenCalledOnce());
		expect(fetchMock.mock.calls[0]![0]).toBe('/api/v1/setup');
		expect(sentBody()).toEqual({ token: 'AB12-CD34-EF56-7890', password: 'correct horse battery' });
	});

	it('refuses a malformed token without sending it', async () => {
		render(SetupForm);
		await fireEvent.input(tokenInput(), { target: { value: 'AB12-CD34' } });
		await fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
		expect(await screen.findByText(/doesn't look like a setup token/)).toBeTruthy();
		expect(tokenInput().getAttribute('aria-invalid')).toBe('true');
		expect(document.body.textContent).not.toContain('AB12-CD34');
		expect(fetchMock).not.toHaveBeenCalled();
	});

	it('returns to the token step on a wrong or spent token, without echoing it', async () => {
		replies = [fail(401, 'bad_token')];
		await toPasswordStep('AB12CD34EF567890');
		await fill('correct horse battery');
		expect(await screen.findByText(/That setup token doesn't match/)).toBeTruthy();
		await waitFor(() => expect(document.activeElement).toBe(tokenInput()));
		expect(screen.getByRole('listitem', { current: 'step' }).textContent).toContain('Setup token');
		expect(document.body.textContent).not.toContain('server says');
		// The token is in the field for correcting, never in the message.
		const message = screen.getByText(/That setup token doesn't match/).textContent!;
		expect(message).not.toContain('AB12');
	});

	it('blocks a short password and a mismatch on the right field', async () => {
		await toPasswordStep();
		await fill('short');
		expect(await screen.findByText('Use at least 10 characters.')).toBeTruthy();
		expect(newPw().getAttribute('aria-invalid')).toBe('true');
		await fill('correct horse battery', 'correct horse batterz');
		expect(await screen.findByText(/The passwords don't match/)).toBeTruthy();
		expect(confirmPw().getAttribute('aria-invalid')).toBe('true');
		expect(fetchMock).not.toHaveBeenCalled();
	});

	it('hints at strength without blocking', async () => {
		await toPasswordStep();
		await fireEvent.input(newPw(), { target: { value: 'aaaaaaaaaaaa' } });
		const describedBy = newPw().getAttribute('aria-describedby')!;
		expect(document.getElementById(describedBy.split(' ')[0]!)!.textContent).toContain(
			'Strength: weak'
		);
		await fill('aaaaaaaaaaaa');
		await waitFor(() => expect(nav.toWelcome).toHaveBeenCalledOnce());
	});

	it('goes to sign in when setup is already done', async () => {
		replies = [fail(409, 'setup_done')];
		await toPasswordStep();
		await fill('correct horse battery');
		await waitFor(() => expect(nav.toLogin).toHaveBeenCalledOnce());
	});

	it('locks on 429 with a countdown', async () => {
		vi.useFakeTimers({ now: 0, toFake: ['setInterval', 'clearInterval', 'Date'] });
		replies = [fail(429, 'rate_limited', { 'Retry-After': '5' })];
		await toPasswordStep();
		await fill('correct horse battery');
		expect((await screen.findByTestId('auth-countdown')).textContent).toBe('Try again in 5 s.');
		expect(create().getAttribute('aria-disabled')).toBe('true');
	});

	it('goes back to the token step with Back', async () => {
		await toPasswordStep();
		await fireEvent.click(screen.getByRole('button', { name: 'Back' }));
		await waitFor(() => expect(document.activeElement).toBe(tokenInput()));
		expect(tokenInput().value).toBe('AB12-CD34-EF56-7890');
	});
});
