import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { setSession } from './session.svelte';

const nav = vi.hoisted(() => ({ toSetup: vi.fn(), toLoginFrom: vi.fn() }));
vi.mock('./nav', () => nav);
vi.mock('$app/paths', () => ({ resolve: (path: string) => path }));
vi.mock('$app/navigation', () => ({ beforeNavigate: () => {}, goto: vi.fn() }));
import WelcomePage from './WelcomePage.svelte';

vi.setConfig({ testTimeout: 20_000 });
configure({ asyncUtilTimeout: 10_000 });

const reply = (setup_required = false, logged_in = true) =>
	new Response(JSON.stringify({ setup_required, logged_in }), {
		status: 200,
		headers: { 'Content-Type': 'application/json' }
	});
beforeEach(() => {
	vi.clearAllMocks();
	setSession('unknown');
});
afterEach(() => vi.unstubAllGlobals());

it('checks authentication before showing the wizard and focuses its heading', async () => {
	let finish!: (value: Response) => void;
	const fetch = vi.fn<typeof globalThis.fetch>(
		() =>
			new Promise<Response>((resolve) => {
				finish = resolve;
			})
	);
	vi.stubGlobal('fetch', fetch);
	render(WelcomePage);
	expect(screen.queryByText('Finish setting up')).toBeNull();
	finish(reply());
	const heading = await screen.findByRole('heading', { level: 1, name: 'Finish setting up' });
	await waitFor(() => expect(document.activeElement).toBe(heading));
	await screen.findByRole('heading', { level: 2, name: 'Network check' });
	expect(fetch.mock.calls[0]![0]).toBe('/api/v1/auth');
	expect(fetch.mock.calls.filter(([url]) => url === '/api/v1/auth')).toHaveLength(1);
});

it.each([true, false])(
	'redirects a direct signed-out visit (setup required: %s)',
	async (setup) => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => reply(setup, false))
		);
		render(WelcomePage);
		await waitFor(() => expect(setup ? nav.toSetup : nav.toLoginFrom).toHaveBeenCalledOnce());
		if (!setup) expect(nav.toLoginFrom).toHaveBeenCalledWith('/welcome');
		expect(screen.queryByText('Finish setting up')).toBeNull();
	}
);

it('offers retry after an auth failure without claiming success', async () => {
	const fetch = vi.fn().mockRejectedValueOnce(new Error('offline')).mockResolvedValue(reply());
	vi.stubGlobal('fetch', fetch);
	render(WelcomePage);
	await screen.findByRole('alert');
	expect(screen.getByRole('heading', { level: 1, name: 'Checking this instance…' })).toBeTruthy();
	await waitFor(() =>
		expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Retry' }))
	);
	await fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
	await screen.findByRole('heading', { name: 'Finish setting up' });
});

it('does not redirect or focus after leaving during the auth check', async () => {
	let finish!: (value: Response) => void;
	vi.stubGlobal(
		'fetch',
		vi.fn(
			() =>
				new Promise<Response>((resolve) => {
					finish = resolve;
				})
		)
	);
	const { unmount } = render(WelcomePage);
	unmount();
	finish(reply(true, false));
	for (let i = 0; i < 20; i++) await Promise.resolve();
	expect(nav.toSetup).not.toHaveBeenCalled();
});
