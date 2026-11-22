import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { onLoginRequired } from '$lib/api/client';
import { clearIndexCache } from '../form/schema';
import { clearFindIndex, loadFindIndex } from './find';
import { settingsRows } from './find-palette';

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('$app/paths', () => ({ resolve: (path: string) => path }));

const signedOut = () =>
	new Response(JSON.stringify({ error: { code: 'login_required', message: 'Log in first.' } }), {
		status: 401,
		headers: { 'content-type': 'application/json' }
	});
let fetchMock: ReturnType<typeof vi.fn>;
let login: ReturnType<typeof vi.fn<Parameters<typeof onLoginRequired>[0] & object>>;

beforeEach(() => {
	clearIndexCache();
	clearFindIndex();
	fetchMock = vi.fn(() => Promise.resolve(signedOut()));
	vi.stubGlobal('fetch', fetchMock);
	login = vi.fn();
	onLoginRequired(login);
});
afterEach(() => {
	onLoginRequired(undefined);
	vi.unstubAllGlobals();
});

it('a signed-out first search takes the app’s sign-in path once and shows no settings', async () => {
	await expect(settingsRows('pause')).resolves.toEqual([]);
	expect(login).toHaveBeenCalledTimes(1);
});

it('keeps no half-built index after a 401, so the next load asks again', async () => {
	await expect(loadFindIndex()).rejects.toMatchObject({ status: 401 });
	await expect(loadFindIndex()).rejects.toMatchObject({ status: 401 });
	expect(fetchMock).toHaveBeenCalledTimes(2);
});

it('does not ask again on every key after a failure', async () => {
	await settingsRows('p');
	const calls = fetchMock.mock.calls.length;
	await settingsRows('pa');
	await settingsRows('pau');
	expect(fetchMock.mock.calls.length).toBe(calls);
});
