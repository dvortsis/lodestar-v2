import { afterEach, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { createRawSnippet } from 'svelte';
import { setLocale } from '$lib/paraglide/runtime.js';
import SettingsShell from './SettingsShell.svelte';

const page = vi.hoisted(() => ({ url: new URL('http://x/settings'), params: {} }));
vi.mock('$app/state', () => ({ page }));
vi.mock('$app/navigation', () => ({ goto: vi.fn(() => Promise.resolve()) }));
vi.mock('./find', () => {
	const entry = (label: string, key: string) => ({
		screen: 'crawler',
		screenTitle: 'Crawler',
		label,
		trail: 'Crawling › Crawler',
		href: `/settings/crawler#${key}`,
		hay: label.toLowerCase(),
		head: label.toLowerCase()
	});
	const entries = [entry('Crawl intensity', 'crawler.intensity')];
	return {
		loadFindIndex: () => Promise.resolve(entries),
		search: (all: typeof entries, q: string) => all.filter((e) => e.hay.includes(q.trim())),
		describe: () => Promise.resolve(undefined),
		forgetValues: () => {},
		highlight: () => () => {}
	};
});
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

// The registry's real screens: the titles render on first paint, in §10.10.2 order, with no
// purpose line or icon module needed.
const EN = [
	'Appearance',
	'Keyboard',
	'Language & region',
	'Crawler',
	'Network & VPN',
	'Sources',
	'Jobs',
	'Companions',
	'Add torrent files',
	'Storage & retention',
	'Search & ranking',
	'Trackers',
	'Filters & Safety',
	'Blocklist subscriptions',
	'Integrations',
	'Outputs',
	'Notifications',
	'Insights & digest',
	'General',
	'Updates',
	'Logging',
	'Security',
	'About'
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
	expect(zh.slice(3, 10)).toEqual([
		'爬虫',
		'网络与 VPN',
		'来源',
		'任务',
		'配套组件',
		'添加种子文件',
		'存储与保留'
	]);
	expect(zh).toContain('语言和地区');
});

it('loads the search only when the box is focused, then shows matching settings in place of the list', async () => {
	render(SettingsShell, { children });
	const box = screen.getByRole('searchbox');
	// Until it is focused the box is a plain input.
	expect(box.getAttribute('role')).toBeNull();
	await fireEvent.focus(box);
	await until(() => expect(screen.getByRole('combobox')).toBeTruthy());
	await fireEvent.input(box, { target: { value: 'intensity' } });
	await until(() => expect(screen.getAllByRole('option').length).toBeGreaterThan(0));
	expect(screen.queryByRole('navigation')).toBeNull();
	await fireEvent.input(box, { target: { value: '' } });
	await until(() => expect(screen.getByRole('navigation')).toBeTruthy());
	expect(links()).toEqual(EN);
});

it('says so when no setting matches', async () => {
	render(SettingsShell, { children });
	const box = screen.getByRole('searchbox');
	await fireEvent.focus(box);
	await until(() => expect(screen.getByRole('combobox')).toBeTruthy());
	await fireEvent.input(box, { target: { value: 'nothing like this' } });
	await until(() => expect(screen.getByRole('status').textContent).toContain('nothing like this'));
});
