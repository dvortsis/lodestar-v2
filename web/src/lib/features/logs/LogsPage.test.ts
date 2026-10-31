import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { stubLayout } from '$lib/ui/lists/testing/layout';

const get = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (orig) => ({
	...(await orig<typeof import('$lib/api/client')>()),
	api: { get }
}));
vi.mock('$app/paths', () => ({ resolve: (p: string) => p.replace('[section]', 'logging') }));

import LogsPage from './LogsPage.svelte';

configure({ defaultHidden: true });
beforeAll(() => stubLayout(600, 1000));
beforeEach(() => {
	get.mockReset();
	get.mockImplementation((path: string) =>
		path === '/logs/catalog'
			? Promise.reject(new Error('no'))
			: Promise.resolve({
					lines: [
						{
							component: 'api',
							event: null,
							fields: [{ kind: 'untyped', name: 'name', value: '<img src=x onerror=alert(1)>' }],
							id: 'aa-1',
							level: 'warn',
							msg: '<script>window.__pwned = 1</script>',
							opt_in: null,
							repeated: null,
							target: null,
							truncated: false,
							ts: '2026-10-31T10:00:00.000Z'
						}
					],
					more: false,
					next_cursor: null,
					unreadable: 0
				})
	);
	vi.stubGlobal(
		'fetch',
		vi.fn(() => new Promise(() => {}))
	);
});
afterEach(() => vi.unstubAllGlobals());

describe('LogsPage', () => {
	it('renders a script in a message and a field as literal text, never as markup', async () => {
		const { container } = render(LogsPage);
		await waitFor(() => expect(screen.getByText(/window\.__pwned/)).toBeTruthy());
		expect(container.querySelector('script')).toBeNull();
		expect((window as unknown as { __pwned?: number }).__pwned).toBeUndefined();
		await fireEvent.click(screen.getByText(/window\.__pwned/));
		await waitFor(() => expect(screen.getByText('<img src=x onerror=alert(1)>')).toBeTruthy());
		expect(container.querySelector('img')).toBeNull();
	});

	it('links to the full log in Settings and sends the level to the server', async () => {
		render(LogsPage);
		await waitFor(() => expect(screen.getByText(/window\.__pwned/)).toBeTruthy());
		expect(screen.getByRole('link', { name: 'Open full log' }).getAttribute('href')).toBe(
			'/settings/logging'
		);
		await fireEvent.change(screen.getByLabelText('Minimum level'), { target: { value: 'error' } });
		await waitFor(
			() =>
				expect(get).toHaveBeenCalledWith(
					'/logs',
					expect.objectContaining({
						query: expect.objectContaining({ level: 'error' })
					})
				),
			{ timeout: 10_000 }
		);
	});

	it('toggles pause with Space and focuses the filter with /', async () => {
		render(LogsPage);
		await waitFor(() => expect(screen.getByText(/window\.__pwned/)).toBeTruthy());
		await fireEvent.keyDown(document.body, { key: ' ' });
		expect(screen.getByRole('button', { name: 'Resume' }).getAttribute('aria-pressed')).toBe(
			'true'
		);
		await fireEvent.keyDown(document.body, { key: '/' });
		expect(document.activeElement).toBe(screen.getByLabelText('Filter text'));
	});
});
