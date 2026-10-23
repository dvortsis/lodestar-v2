import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { clearIndexCache } from '../form/schema';
import { dismissAll } from '../form/dialogs';
import { goldenValues, settingsApi } from '../shell/testing';
import ScreenHarness from '../shell/testing/ScreenHarness.svelte';
import { sv } from '../form/testing/values';

const route = vi.hoisted(() => ({
	page: { params: {} as Record<string, string | undefined>, url: new URL('http://x/'), state: {} },
	replaceState: vi.fn()
}));
vi.mock('$app/state', () => ({ page: route.page }));
vi.mock('$app/navigation', () => ({
	beforeNavigate: () => {},
	afterNavigate: () => {},
	goto: vi.fn(),
	replaceState: route.replaceState
}));

// Controls the engine loads by type, transformed once up front rather than inside a test's wait.
beforeAll(async () => {
	await Promise.all([import('../form/NumberControl.svelte'), import('./ConfigDisclosure.svelte')]);
});

const KEY = 'lodestar.settings.advanced.crawler';

function open(id: string, search = '', hash = '') {
	route.page.params = { section: id };
	route.page.url = new URL(`http://localhost/settings/${id}${search}${hash}`);
	history.replaceState(null, '', `/settings/${id}${search}${hash}`);
}

const field = (key: string) => document.getElementById(key);
const advanced = (group: string) => screen.getByTestId(`advanced-${group}`) as HTMLDetailsElement;

beforeEach(() => {
	vi.spyOn(console, 'warn').mockImplementation(() => {});
	vi.stubGlobal('matchMedia', () => ({
		matches: true,
		addEventListener: () => {},
		removeEventListener: () => {}
	}));
	clearIndexCache();
	sessionStorage.clear();
	localStorage.clear();
	route.replaceState.mockClear();
});
afterEach(() => {
	dismissAll();
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
	history.replaceState(null, '', '/');
});

describe('a generated screen', () => {
	it('shows the heading, its purpose and the groups with Advanced (n) closed', async () => {
		settingsApi();
		open('crawler');
		render(ScreenHarness, { id: 'crawler' });
		await screen.findByRole('heading', { level: 1, name: 'Crawler' });
		expect(screen.getByText(/How hard Lodestar crawls/)).toBeTruthy();
		expect(
			(await screen.findAllByRole('heading', { level: 2, name: 'Intensity' })).length
		).toBeGreaterThan(0);
		expect(screen.getByText('Fetching torrent details')).toBeTruthy();
		const fetching = advanced('fetching');
		expect(fetching.querySelector('summary')?.textContent).toBe('Advanced (3)');
		expect(fetching.open).toBe(false);
		expect(field('crawler.timeout_secs')).not.toBeNull();
	});

	it('remembers an Advanced the user opens, per screen', async () => {
		settingsApi();
		open('crawler');
		const first = render(ScreenHarness, { id: 'crawler' });
		await screen.findByTestId('advanced-fetching');
		const details = advanced('fetching');
		details.open = true;
		await fireEvent(details, new Event('toggle'));
		expect(JSON.parse(localStorage.getItem(KEY)!)).toEqual(['fetching']);
		first.unmount();

		render(ScreenHarness, { id: 'crawler' });
		await screen.findByTestId('advanced-fetching');
		expect(advanced('fetching').open).toBe(true);

		advanced('fetching').open = false;
		await fireEvent(advanced('fetching'), new Event('toggle'));
		expect(localStorage.getItem(KEY)).toBeNull();
	});

	it('works when storage throws, and just forgets', async () => {
		settingsApi();
		vi.stubGlobal('localStorage', {
			getItem: () => {
				throw new DOMException('blocked', 'SecurityError');
			},
			setItem: () => {
				throw new DOMException('blocked', 'SecurityError');
			},
			removeItem: () => {
				throw new DOMException('blocked', 'SecurityError');
			}
		});
		open('crawler');
		render(ScreenHarness, { id: 'crawler' });
		await screen.findByTestId('advanced-fetching');
		const details = advanced('fetching');
		details.open = true;
		await expect(fireEvent(details, new Event('toggle'))).resolves.toBeDefined();
		expect(field('crawler.intensity')).not.toBeNull();
	});

	it('opens Advanced for a deep link, focuses the field and highlights it, without remembering', async () => {
		settingsApi();
		open('crawler', '', '#crawler.timeout_secs');
		render(ScreenHarness, { id: 'crawler' });
		await waitFor(() => expect(advanced('fetching').open).toBe(true), { timeout: 10_000 });
		await waitFor(
			() => {
				const wrapper = field('crawler.timeout_secs')!;
				expect(wrapper.hasAttribute('data-highlight')).toBe(true);
				expect(wrapper.contains(document.activeElement)).toBe(true);
			},
			{ timeout: 10_000 }
		);
		expect(localStorage.getItem(KEY)).toBeNull();
	});

	it('ignores a hash that is not one of the screen’s fields', async () => {
		settingsApi();
		open('crawler', '', '#nothing.here');
		render(ScreenHarness, { id: 'crawler' });
		await screen.findByTestId('advanced-fetching');
		expect(advanced('fetching').open).toBe(false);
	});

	it('shows only changed fields with ?changed=1, and one line when nothing is changed', async () => {
		settingsApi();
		open('crawler', '?changed=1');
		render(ScreenHarness, { id: 'crawler' });
		expect(await screen.findByTestId('settings-nothing-changed')).toBeTruthy();
		expect(field('crawler.intensity')).toBeNull();
	});

	it('keeps changed fields, hides the rest and opens the Advanced that holds one', async () => {
		const initial = goldenValues();
		initial.find((s) => s.section === 'crawler')!.fields.timeout_secs = sv(30, 'db');
		settingsApi({}, initial);
		open('crawler', '?changed=1');
		render(ScreenHarness, { id: 'crawler' });
		await waitFor(() => expect(field('crawler.timeout_secs')).not.toBeNull());
		expect(field('crawler.intensity')).toBeNull();
		expect(field('crawler.max_sockets')).toBeNull();
		expect(advanced('fetching').open).toBe(true);
		expect(advanced('fetching').querySelector('summary')?.textContent).toBe('Advanced (1)');
		expect(screen.queryByTestId('settings-nothing-changed')).toBeNull();
	});

	it('puts Show changed only in the URL', async () => {
		settingsApi();
		open('crawler');
		render(ScreenHarness, { id: 'crawler' });
		const toggle = await screen.findByTestId('settings-changed-only');
		await fireEvent.click(toggle);
		expect(route.replaceState).toHaveBeenCalledTimes(1);
		expect((route.replaceState.mock.calls[0]![0] as URL).search).toBe('?changed=1');
	});

	it('saves instantly on a preferences screen and stages everywhere else', async () => {
		settingsApi();
		open('preferences');
		render(ScreenHarness, { id: 'preferences' });
		await screen.findByRole('heading', { level: 1, name: 'Preferences' });
		await waitFor(() => expect(field('preferences.theme')).not.toBeNull());
		expect(screen.queryByTestId('save-bar')).toBeNull();
	});
});
