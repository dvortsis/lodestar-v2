import { afterEach, expect, it, vi } from 'vitest';
import { onLoginRequired } from '$lib/api/client';
import { requestSettingsExport } from './export';

afterEach(() => {
	vi.unstubAllGlobals();
	onLoginRequired(undefined);
});

it('requests the backend file with explicit preferences and preserves its bytes', async () => {
	const body = '# fixture\r\nlodestar_settings = 1\r\n[crawler]\r\npaused = true\r\n';
	const fetch = vi.fn(
		async () =>
			new Response(body, { headers: { 'Content-Type': 'application/toml; charset=utf-8' } })
	);
	vi.stubGlobal('fetch', fetch);
	const signal = new AbortController().signal;
	expect(await (await requestSettingsExport(false, signal)).text()).toBe(body);
	expect(fetch).toHaveBeenCalledWith('/api/v1/settings/export?preferences=false', {
		credentials: 'same-origin',
		headers: { Accept: 'application/toml' },
		signal
	});
	await requestSettingsExport(true, signal);
	expect(fetch).toHaveBeenNthCalledWith(2, '/api/v1/settings/export?preferences=true', {
		credentials: 'same-origin',
		headers: { Accept: 'application/toml' },
		signal
	});
});

it.each(['application/json', 'text/html', ''])(
	'rejects a successful response of type %s',
	async (type) => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => new Response('not an export', { headers: { 'Content-Type': type } }))
		);
		await expect(requestSettingsExport(false, new AbortController().signal)).rejects.toMatchObject({
			code: 'bad_response'
		});
	}
);

it('uses the shared login-required handler for expired sessions', async () => {
	const login = vi.fn();
	onLoginRequired(login);
	vi.stubGlobal(
		'fetch',
		vi.fn(
			async () =>
				new Response(JSON.stringify({ error: { code: 'login_required', message: 'expired' } }), {
					status: 401
				})
		)
	);
	await expect(requestSettingsExport(false, new AbortController().signal)).rejects.toMatchObject({
		status: 401
	});
	expect(login).toHaveBeenCalledTimes(1);
});

it('does not start an already-cancelled request', async () => {
	const fetch = vi.fn();
	vi.stubGlobal('fetch', fetch);
	const controller = new AbortController();
	controller.abort();
	await expect(requestSettingsExport(false, controller.signal)).rejects.toMatchObject({
		name: 'AbortError'
	});
	expect(fetch).not.toHaveBeenCalled();
});

it('drops a late response after cancellation even if fetch ignores the signal', async () => {
	const controller = new AbortController();
	vi.stubGlobal(
		'fetch',
		vi.fn(async () => {
			controller.abort();
			return new Response('lodestar_settings = 1', {
				headers: { 'Content-Type': 'application/toml' }
			});
		})
	);
	await expect(requestSettingsExport(false, controller.signal)).rejects.toMatchObject({
		name: 'AbortError'
	});
});
