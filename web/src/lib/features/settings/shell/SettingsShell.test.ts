import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/svelte';
import { clearIndexCache } from '../form/schema';
import { settingsApi } from './testing';
import ScreenHarness from './testing/ScreenHarness.svelte';

const nav = vi.hoisted(() => ({ callbacks: [] as (() => void)[] }));
const route = vi.hoisted(() => ({
	page: {
		params: {} as Record<string, string | undefined>,
		url: new URL('http://localhost/settings'),
		state: {}
	}
}));
vi.mock('$app/state', () => ({ page: route.page }));
vi.mock('$app/navigation', () => ({
	beforeNavigate: () => {},
	afterNavigate: (f: () => void) => nav.callbacks.push(f),
	goto: vi.fn(),
	replaceState: vi.fn()
}));

function viewport(wide: boolean) {
	vi.stubGlobal(
		'matchMedia',
		vi.fn((query: string) => ({
			matches: wide,
			media: query,
			addEventListener: () => {},
			removeEventListener: () => {}
		}))
	);
}

function open(path: string, section?: string) {
	route.page.params = section ? { section } : {};
	route.page.url = new URL(`http://localhost${path}`);
	nav.callbacks.length = 0;
}

beforeEach(() => {
	vi.spyOn(console, 'warn').mockImplementation(() => {});
	clearIndexCache();
	sessionStorage.clear();
	localStorage.clear();
	settingsApi();
});
afterEach(() => {
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe('the Settings shell', () => {
	it('shows two panes from x-large: the navigation beside the screen', async () => {
		viewport(true);
		open('/settings/crawler', 'crawler');
		render(ScreenHarness, { id: 'crawler' });
		const nav = await screen.findByRole('navigation', { name: 'Settings sections' });
		expect(within(nav).getByRole('link', { name: 'Crawler' }).getAttribute('aria-current')).toBe(
			'page'
		);
		expect(
			within(nav).getByRole('link', { name: 'Network & VPN' }).getAttribute('aria-current')
		).toBeNull();
		expect(screen.queryByTestId('settings-back')).toBeNull();
		expect(await screen.findByRole('heading', { level: 1, name: 'Crawler' })).toBeTruthy();
	});

	it('groups the navigation and lists interim screens for unclaimed sections', async () => {
		viewport(true);
		open('/settings/crawler', 'crawler');
		render(ScreenHarness, { id: 'crawler' });
		const nav = await screen.findByRole('navigation', { name: 'Settings sections' });
		await within(nav).findByRole('link', { name: 'Logging' });
		const headings = within(nav)
			.getAllByRole('heading', { level: 2 })
			.map((h) => h.textContent);
		expect(headings).toEqual(['Preferences', 'Crawling', 'Library', 'System']);
		for (const name of ['Keyboard', 'Preferences', 'Content', 'Crawler', 'General', 'Logging']) {
			expect(within(nav).getAllByRole('link', { name }).length).toBeGreaterThan(0);
		}
	});

	it('shows only the list at /settings below x-large, and only the screen with a Back link at /settings/<id>', async () => {
		viewport(false);
		open('/settings');
		const list = render(ScreenHarness, {});
		expect(await screen.findByRole('heading', { level: 1, name: 'Settings' })).toBeTruthy();
		expect(screen.getAllByRole('navigation', { name: 'Settings sections' })).toHaveLength(1);
		expect(screen.queryByTestId('settings-back')).toBeNull();
		list.unmount();

		open('/settings/network', 'network');
		render(ScreenHarness, { id: 'network' });
		expect(await screen.findByRole('heading', { level: 1, name: 'Network & VPN' })).toBeTruthy();
		expect(screen.queryByRole('navigation', { name: 'Settings sections' })).toBeNull();
		expect(screen.getByTestId('settings-back').getAttribute('href')).toBe('/settings');
	});

	it('has one h1 per page', async () => {
		viewport(true);
		open('/settings/storage', 'storage');
		render(ScreenHarness, { id: 'storage' });
		await screen.findByRole('heading', { level: 1, name: 'Storage & retention' });
		expect(screen.getAllByRole('heading', { level: 1 })).toHaveLength(1);
	});

	it('moves focus to the heading after opening a screen', async () => {
		viewport(true);
		open('/settings/search', 'search');
		render(ScreenHarness, { id: 'search' });
		const h1 = await screen.findByRole('heading', { level: 1, name: 'Search & ranking' });
		expect(h1.getAttribute('tabindex')).toBe('-1');
		nav.callbacks.forEach((f) => f());
		await waitFor(() => expect(document.activeElement).toBe(h1));
	});

	it('leaves focus for the screen when the URL hash names where to go', async () => {
		viewport(true);
		open('/settings/search#search.heap_mib', 'search');
		history.replaceState(null, '', '/settings/search#search.heap_mib');
		render(ScreenHarness, { id: 'search' });
		const h1 = await screen.findByRole('heading', { level: 1, name: 'Search & ranking' });
		nav.callbacks.forEach((f) => f());
		await waitFor(() => expect(document.getElementById('search.heap_mib')).not.toBeNull());
		expect(document.activeElement).not.toBe(h1);
		history.replaceState(null, '', '/');
	});

	it('says so, with Retry, when the schema cannot be loaded; the screens it knows still list', async () => {
		viewport(true);
		settingsApi({
			'GET /settings': () => ({ status: 500, body: { error: { code: 'x', message: 'x' } } })
		});
		open('/settings');
		render(ScreenHarness, {});
		expect(await screen.findByText(/Couldn.t load the settings/)).toBeTruthy();
		expect(screen.getByRole('button', { name: 'Try again' })).toBeTruthy();
		const nav = screen.getByRole('navigation', { name: 'Settings sections' });
		expect(within(nav).getByRole('link', { name: 'Crawler' })).toBeTruthy();
	});
});
