import { afterEach, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { createRawSnippet } from 'svelte';
import { setLocale } from '$lib/paraglide/runtime.js';
import SettingsShell from './SettingsShell.svelte';
import { screens } from './screens';

const page = vi.hoisted(() => ({ url: new URL('http://x/settings'), params: {} }));
vi.mock('$app/state', () => ({ page }));
vi.mock('$app/paths', () => ({ resolve: (path: string) => path }));

const children = createRawSnippet(() => ({ render: () => '<p>Body</p>' }));
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });
const links = () =>
	screen
		.getAllByRole('link')
		.slice(1)
		.map((link) => link.textContent?.trim());

afterEach(() => {
	setLocale('en', { reload: false });
	vi.restoreAllMocks();
});
const spyAbouts = () =>
	screens.filter((s) => s.about).map((s) => vi.spyOn(s as Required<typeof s>, 'about'));

// The registry's real screens: the titles render on first paint, in §10.10.2 order, with no
// purpose line or icon module needed.
const EN = [
	'Appearance',
	'Keyboard',
	'Language & region',
	'Crawler',
	'Network & VPN',
	'Storage & retention',
	'Search & ranking',
	'Trackers',
	'Filters & Safety',
	'Integrations',
	'Notifications',
	'Insights & digest',
	'General',
	'Logging',
	'Security'
];

it('renders every title at once, in navigation order', () => {
	render(SettingsShell, { children });
	expect(links()).toEqual(EN);
});

it('renders the titles in Chinese, in the same order', () => {
	setLocale('zh', { reload: false });
	render(SettingsShell, { children });
	const zh = links();
	expect(zh).toHaveLength(EN.length);
	expect(zh.slice(3, 7)).toEqual(['爬虫', '网络与 VPN', '存储与保留', '搜索与排序']);
	expect(zh).toContain('语言和地区');
});

it('matches titles at once, and purpose lines once they have loaded', async () => {
	render(SettingsShell, { children });
	const filter = screen.getByRole('searchbox');
	await fireEvent.input(filter, { target: { value: 'crawl' } });
	expect(links()).toEqual(['Crawler']);
	// "peer transport" is only in Crawler's purpose line, which loads with the first query.
	await fireEvent.input(filter, { target: { value: 'peer transport' } });
	await until(() => expect(links()).toEqual(['Crawler']));
	await fireEvent.input(filter, { target: { value: 'first day of the week' } });
	expect(links()).toEqual(['Language & region']);
	await fireEvent.input(filter, { target: { value: 'nothing like this' } });
	expect(screen.getByRole('status')).toBeTruthy();
});

it('loads no purpose line until the filter is used', async () => {
	const spies = spyAbouts();
	expect(spies.length).toBeGreaterThan(0);
	render(SettingsShell, { children });
	await fireEvent.input(screen.getByRole('searchbox'), { target: { value: ' ' } });
	for (const spy of spies) expect(spy).not.toHaveBeenCalled();
	await fireEvent.input(screen.getByRole('searchbox'), { target: { value: 'c' } });
	await until(() => {
		for (const spy of spies) expect(spy).toHaveBeenCalled();
	});
});

it('keeps matching titles when the purpose lines cannot load', async () => {
	const spies = spyAbouts().map((spy) =>
		spy.mockImplementation(() => Promise.reject(new Error('chunk')))
	);
	render(SettingsShell, { children });
	const filter = screen.getByRole('searchbox');
	await fireEvent.input(filter, { target: { value: 'peer transport' } });
	await until(() => {
		for (const spy of spies) expect(spy).toHaveBeenCalled();
	});
	expect(screen.getByRole('status')).toBeTruthy();
	await fireEvent.input(filter, { target: { value: 'crawl' } });
	expect(links()).toEqual(['Crawler']);
});
