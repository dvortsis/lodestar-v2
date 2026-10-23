import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { clearIndexCache } from '../form/schema';
import { sv } from '../form/testing/values';
import { goldenValues, settingsApi, systemInfo } from './testing';
import ScreenHarness from './testing/ScreenHarness.svelte';

const nav = vi.hoisted(() => ({ goto: vi.fn() }));
const route = vi.hoisted(() => ({
	page: {
		params: {} as Record<string, string | undefined>,
		url: new URL('http://x/settings'),
		state: {}
	}
}));
vi.mock('$app/state', () => ({ page: route.page }));
vi.mock('$app/navigation', () => ({
	beforeNavigate: () => {},
	afterNavigate: () => {},
	goto: nav.goto,
	replaceState: vi.fn()
}));

const box = () => screen.getByRole('combobox', { name: 'Search settings' }) as HTMLInputElement;
const results = () => screen.queryAllByTestId('settings-search-result');

async function open() {
	const initial = goldenValues();
	const network = initial.find((s) => s.section === 'network')!;
	network.fields.dht_port = sv(7000, 'db');
	network.fields.http_bind = sv('0.0.0.0:8090', 'env', {
		env: 'LODESTAR_HTTP_BIND',
		read_only: true
	});
	settingsApi({ 'GET /system': () => ({ status: 200, body: systemInfo() }) }, initial);
	render(ScreenHarness, {});
	await screen.findByRole('navigation', { name: 'Settings sections' });
	await fireEvent.focus(box());
	await waitFor(() => expect(box().getAttribute('aria-expanded')).toBe('false'));
}
async function type(q: string) {
	await fireEvent.input(box(), { target: { value: q } });
}

beforeEach(() => {
	vi.spyOn(console, 'warn').mockImplementation(() => {});
	vi.stubGlobal('matchMedia', () => ({
		matches: true,
		addEventListener: () => {},
		removeEventListener: () => {}
	}));
	clearIndexCache();
	nav.goto.mockClear();
	route.page.params = {};
	route.page.url = new URL('http://x/settings');
});
afterEach(() => {
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe('settings search', () => {
	it('is a combobox over a listbox, and finds a setting by a synonym with breadcrumb, value and source', async () => {
		await open();
		await type('port');
		const rows = await screen.findAllByTestId('settings-search-result');
		expect(box().getAttribute('aria-expanded')).toBe('true');
		expect(screen.getByRole('listbox', { name: 'Search results' })).toBeTruthy();
		const port = rows.find((r) => r.textContent?.includes('DHT port'))!;
		expect(within(port).getByTestId('settings-search-crumb').textContent).toBe(
			'Crawling › Network & VPN'
		);
		await waitFor(() => expect(port.textContent).toContain('7,000'));
		expect(port.textContent).toContain('Saved in app');
		const bind = rows.find((r) => r.textContent?.includes('Web address'))!;
		await waitFor(() => expect(bind.textContent).toContain('Set by LODESTAR_HTTP_BIND'));
		expect(bind.textContent).toContain('0.0.0.0:8090');
		expect(bind.querySelector('.font-mono, [class*="mono"], code')).not.toBeNull();
	});

	it('says Advanced in the breadcrumb of an advanced setting', async () => {
		await open();
		await type('fetch timeout');
		const rows = await screen.findAllByTestId('settings-search-result');
		expect(within(rows[0]!).getByTestId('settings-search-crumb').textContent).toBe(
			'Crawling › Crawler › Advanced'
		);
	});

	it('moves through the results with the arrows and opens one with Enter, hash included', async () => {
		await open();
		await type('timeout');
		await screen.findAllByTestId('settings-search-result');
		const first = results()[0]!;
		expect(box().getAttribute('aria-activedescendant')).toBe(first.id);
		expect(first.getAttribute('aria-selected')).toBe('true');
		await fireEvent.keyDown(box(), { key: 'ArrowDown' });
		const second = results()[1];
		if (second) expect(box().getAttribute('aria-activedescendant')).toBe(second.id);
		await fireEvent.keyDown(box(), { key: 'ArrowUp' });
		expect(box().getAttribute('aria-activedescendant')).toBe(first.id);
		await fireEvent.keyDown(box(), { key: 'Enter' });
		expect(nav.goto).toHaveBeenCalledWith('/settings/crawler#crawler.timeout_secs');
		expect(box().value).toBe('');
	});

	it('opens a screen found by its title without a hash', async () => {
		await open();
		await type('Storage');
		const rows = await screen.findAllByTestId('settings-search-result');
		await fireEvent.click(rows.find((r) => r.textContent?.includes('Storage & retention'))!);
		expect(nav.goto).toHaveBeenCalledWith('/settings/storage');
	});

	it('says no setting matches, and links to the user guide in a new tab', async () => {
		await open();
		await type('zzzz');
		const none = await screen.findByTestId('settings-search-none');
		expect(none.textContent).toContain("No setting matches 'zzzz'");
		const link = within(none).getByRole('link');
		expect(link.getAttribute('href')).toBe(
			'https://github.com/dvortsis/lodestar-v2/tree/main/docs/user'
		);
		expect(link.getAttribute('target')).toBe('_blank');
		expect(link.getAttribute('rel')).toContain('noopener');
	});

	it('clears on Escape and closes the list', async () => {
		await open();
		await type('port');
		await screen.findAllByTestId('settings-search-result');
		await fireEvent.keyDown(box(), { key: 'Escape' });
		expect(box().value).toBe('');
		expect(results()).toHaveLength(0);
		expect(box().getAttribute('aria-expanded')).toBe('false');
	});

	it('asks for each section’s values once while open', async () => {
		const api = await (async () => {
			const initial = goldenValues();
			return settingsApi({ 'GET /system': () => ({ status: 200, body: systemInfo() }) }, initial);
		})();
		render(ScreenHarness, {});
		await screen.findByRole('navigation', { name: 'Settings sections' });
		await fireEvent.focus(box());
		await waitFor(() => expect(box().getAttribute('aria-expanded')).toBe('false'));
		await type('port');
		await screen.findAllByTestId('settings-search-result');
		await waitFor(() => expect(api.calls.some((c) => c.path === '/settings/network')).toBe(true));
		await type('port forward');
		await type('port');
		expect(api.calls.filter((c) => c.path === '/settings/network')).toHaveLength(1);
	});
});
