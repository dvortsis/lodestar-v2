// The overflow menu's Sign out (X-wizard-labels-signout, review B2): an action after the routes
// that ends the session and lands on sign-in; a failure says so in a toast and stays put.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { toast, toasts } from '$lib/ui/feedback/toasts.svelte';
import { currentSession, setSession } from '$lib/features/auth/session.svelte';

const nav = vi.hoisted(() => ({ goto: vi.fn(async () => {}) }));
vi.mock('$app/navigation', () => ({ goto: nav.goto, beforeNavigate: () => {} }));
vi.mock('$app/paths', () => ({ resolve: (path: string) => path }));
import OverflowMenu from './OverflowMenu.svelte';

vi.setConfig({ testTimeout: 30_000 });
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });
const json = (status: number, body: unknown) =>
	new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });

let fetch: ReturnType<typeof vi.fn>;
beforeEach(() => {
	nav.goto.mockClear();
	toast.clear();
	setSession('signed_in');
	fetch = vi.fn(async () => json(200, { ok: true }));
	vi.stubGlobal('fetch', fetch);
});
afterEach(() => {
	cleanup();
	toast.clear();
	vi.unstubAllGlobals();
});

const open = () => render(OverflowMenu, { hrefFor: (href: string) => href, openNow: true });
const signOutItem = () => screen.findByRole('menuitem', { name: 'Sign out' });

describe('overflow menu', () => {
	it('lists the routes, then Sign out', async () => {
		open();
		await signOutItem();
		expect(screen.getAllByRole('menuitem').map((i) => i.textContent?.trim())).toEqual([
			'Logs',
			'About',
			'Sign out'
		]);
	});

	it('Sign out posts /logout once and lands on sign-in', async () => {
		open();
		await fireEvent.click(await signOutItem());
		await until(() => expect(nav.goto).toHaveBeenCalledWith('/login', { replaceState: true }));
		expect(fetch).toHaveBeenCalledOnce();
		expect(fetch.mock.calls[0]![0]).toBe('/api/v1/logout');
		expect((fetch.mock.calls[0]![1] as RequestInit).method).toBe('POST');
		expect(currentSession()).toBe('signed_out');
		expect(toasts.items).toEqual([]);
	});

	it('a failed sign-out shows the error toast and stays signed in', async () => {
		fetch.mockImplementation(async () => Promise.reject(new TypeError('offline')));
		open();
		await fireEvent.click(await signOutItem());
		await until(() => expect(toasts.items.map((t) => t.kind)).toEqual(['error']));
		expect(toasts.items[0]!.text).toContain("Couldn't sign out");
		expect(nav.goto).not.toHaveBeenCalled();
		expect(currentSession()).toBe('signed_in');
	});

	it('a route still navigates', async () => {
		open();
		await fireEvent.click(await screen.findByRole('menuitem', { name: 'Logs' }));
		await until(() => expect(nav.goto).toHaveBeenCalledWith('/logs'));
		expect(fetch).not.toHaveBeenCalled();
	});
});
