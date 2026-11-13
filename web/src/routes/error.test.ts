import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';

const state = vi.hoisted(() => ({ page: { status: 500, url: new URL('http://x/alerts') } }));
vi.mock('$app/state', () => state);
vi.mock('$app/paths', () => ({ resolve: (r: string) => r }));

import ErrorPage from './+error.svelte';

const reload = vi.fn();
let alive = false;
let probes = 0;
beforeEach(() => {
	alive = false;
	probes = 0;
	state.page.status = 500;
	reload.mockClear();
	vi.stubGlobal('location', { ...window.location, reload });
	vi.stubGlobal(
		'fetch',
		vi.fn(async () => {
			probes++;
			if (!alive) throw new TypeError('Failed to fetch');
			return new Response('boom', { status: 500 });
		})
	);
});
afterEach(() => vi.unstubAllGlobals());

describe('+error.svelte', () => {
	it('shows the connection view, not Error 500, when nothing answers', async () => {
		render(ErrorPage);
		await screen.findByRole('heading', { name: 'Couldn’t reach Lodestar' });
		expect(screen.queryByText('Error 500')).toBeNull();
		expect(screen.getByTestId('offline-status').getAttribute('aria-live')).toBe('polite');
	});

	it('keeps the error view when the server answers with a 500', async () => {
		alive = true;
		render(ErrorPage);
		await screen.findByRole('heading', { name: 'Something went wrong' });
		expect(screen.queryByRole('heading', { name: 'Couldn’t reach Lodestar' })).toBeNull();
		expect(reload).not.toHaveBeenCalled();
	});

	it('does not probe for a 404', async () => {
		state.page.status = 404;
		render(ErrorPage);
		await screen.findByRole('heading', { name: 'Page not found' });
		expect(probes).toBe(0);
	});

	it('Try again probes now, and loads the page again once the server answers', async () => {
		render(ErrorPage);
		const button = await screen.findByRole('button', { name: 'Try again' });
		await waitFor(() => expect(probes).toBe(1));
		alive = true;
		await fireEvent.click(button);
		await waitFor(() => expect(reload).toHaveBeenCalledOnce());
	});
});
