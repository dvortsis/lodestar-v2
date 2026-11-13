import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';

vi.mock('$app/state', () => import('./error.page-state.svelte'));
vi.mock('$app/paths', () => ({ resolve: (r: string) => r }));

import ErrorPage from './+error.svelte';
import { setPage } from './error.page-state.svelte';

const WAIT = { timeout: 10_000 };
const reload = vi.fn();
let alive = false;
let urls: string[] = [];
beforeEach(() => {
	alive = false;
	urls = [];
	setPage(500, '/alerts');
	reload.mockClear();
	vi.stubGlobal('location', { ...window.location, reload });
	vi.stubGlobal(
		'fetch',
		vi.fn(async (url: string) => {
			urls.push(url);
			if (!alive) throw new TypeError('Failed to fetch');
			return new Response('{}');
		})
	);
});
afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

const unreachable = () => screen.findByRole('heading', { name: 'Can’t reach Lodestar' }, WAIT);

describe('+error.svelte', () => {
	it('shows the connection view, not Error 500, when nothing answers', async () => {
		render(ErrorPage);
		await unreachable();
		expect(screen.queryByText('Error 500')).toBeNull();
		expect(screen.getByTestId('offline-status').getAttribute('aria-live')).toBe('polite');
		expect(screen.getByTestId('offline-status').textContent).toBe('Trying again in 1 sec.');
		expect(urls).toEqual(['/_app/version.json']);
	});

	it('keeps the error view when the server answers, and shows nothing before it has', async () => {
		alive = true;
		render(ErrorPage);
		expect(screen.queryByRole('heading')).toBeNull();
		await screen.findByRole('heading', { name: 'Something went wrong' }, WAIT);
		expect(screen.queryByRole('heading', { name: 'Can’t reach Lodestar' })).toBeNull();
		expect(reload).not.toHaveBeenCalled();
	});

	it('does not probe for a 404', async () => {
		setPage(404, '/nope');
		render(ErrorPage);
		await screen.findByRole('heading', { name: 'Page not found' }, WAIT);
		expect(urls).toEqual([]);
	});

	it('probes again when a later error is a 5xx, and drops the offline state for a 404', async () => {
		render(ErrorPage);
		await unreachable();
		setPage(404, '/nope');
		await tick();
		await screen.findByRole('heading', { name: 'Page not found' }, WAIT);
		expect(screen.queryByRole('heading', { name: 'Can’t reach Lodestar' })).toBeNull();
		const before = urls.length;
		setPage(500, '/alerts');
		await tick();
		await waitFor(() => expect(urls.length).toBe(before + 1), WAIT);
		await unreachable();
	});

	it('Try again probes now: no timer has been advanced when the page loads again', async () => {
		vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
		render(ErrorPage);
		const button = await screen.findByRole('button', { name: 'Try again' }, WAIT);
		expect(urls.length).toBe(1);
		expect(reload).not.toHaveBeenCalled();
		alive = true;
		await fireEvent.click(button);
		await waitFor(() => expect(reload).toHaveBeenCalledOnce(), WAIT);
	});

	it('probes at once when the browser comes back online', async () => {
		vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
		render(ErrorPage);
		await screen.findByRole('button', { name: 'Try again' }, WAIT);
		alive = true;
		window.dispatchEvent(new Event('online'));
		await tick();
		await waitFor(() => expect(reload).toHaveBeenCalledOnce(), WAIT);
	});
});
