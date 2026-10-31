// Security's parts when their code can't load: a section that didn't load offers Try again; the
// reveal dialog failing to load sends nothing (so no secret is made that can't be shown); and the
// leave question failing to load lets the navigation go ahead.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { initLocale } from '$lib/i18n/locale.svelte';
import Later from './Later.svelte';
import Keys from './Keys.svelte';
import ChangePassword from './ChangePassword.svelte';

type Guard = (n: unknown) => void;
const nav = vi.hoisted(() => ({ guards: [] as Guard[] }));
vi.mock('$app/navigation', () => ({
	beforeNavigate: (f: Guard) => nav.guards.push(f),
	goto: vi.fn()
}));
vi.mock('./Reveal.svelte', () => {
	throw new Error('chunk failed');
});
vi.mock('../form/dialogs', () => ({ ask: () => Promise.reject(new Error('chunk failed')) }));
vi.setConfig({ testTimeout: 30_000 });
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });

let calls: string[];
beforeEach(() => {
	initLocale('en');
	nav.guards.length = 0;
	calls = [];
	vi.stubGlobal(
		'fetch',
		vi.fn(async (url: string, init: RequestInit = {}) => {
			calls.push(`${init.method ?? 'GET'} ${new URL(url, 'http://x').pathname}`);
			return new Response(JSON.stringify({ keys: [] }), {
				status: 200,
				headers: { 'Content-Type': 'application/json' }
			});
		})
	);
});
afterEach(() => {
	vi.unstubAllGlobals();
	document.body.innerHTML = '';
});

describe('parts that fail to load', () => {
	it('a section that didn’t load says so and tries again', async () => {
		let fail = true;
		const load = vi.fn(async () => {
			if (fail) throw new Error('chunk failed');
			return { default: Keys };
		});
		render(Later, { id: 's', heading: 'Keys', help: 'Help', show: 'Show', load });
		await fireEvent.click(screen.getByTestId('s-show'));
		expect(await screen.findByText("This part of the screen didn't load.")).toBeTruthy();
		fail = false;
		await fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
		expect(await screen.findByTestId('security-keys')).toBeTruthy();
		await until(() =>
			expect(document.activeElement).toBe(screen.getByRole('heading', { name: 'Keys' }))
		);
	});

	it('a reveal dialog that can’t load sends nothing', async () => {
		render(Keys);
		const name = await screen.findByLabelText(/^Name/);
		await fireEvent.input(name, { target: { value: 'Radarr' } });
		await fireEvent.click(screen.getByTestId('security-key-create'));
		await until(() =>
			expect(screen.getByTestId('security-keys-result').textContent).toContain(
				"That didn't work. Try again."
			)
		);
		expect(screen.queryByRole('dialog')).toBeNull();
		expect(calls.filter((c) => !c.startsWith('GET'))).toEqual([]);
	});

	it('a leave question that can’t load lets the navigation go ahead', async () => {
		const assign = vi.fn();
		vi.stubGlobal('location', { origin: 'http://localhost:3000', assign });
		render(ChangePassword);
		const current = within(screen.getByTestId('security-password')).getByLabelText(
			/Current password/
		);
		await fireEvent.input(current, { target: { value: 'half-typed' } });
		const cancel = vi.fn();
		for (const g of nav.guards)
			g({
				type: 'link',
				from: { url: new URL('http://localhost:3000/settings/security') },
				to: { url: new URL('http://localhost:3000/settings/general') },
				cancel
			});
		expect(cancel).toHaveBeenCalled();
		await until(() =>
			expect(assign).toHaveBeenCalledWith('http://localhost:3000/settings/general')
		);
	});
});
