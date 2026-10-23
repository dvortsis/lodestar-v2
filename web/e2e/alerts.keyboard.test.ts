// §10.17's "save alert" flow, keyboard only, and deleting by keyboard with focus restored.
import type { components } from '../src/lib/api/schema';
import { expect, test } from './support/test';

type Saved = components['schemas']['SavedSearchView'];

const T0 = 1_700_000_000;
const saved = (over: Partial<Saved> & Pick<Saved, 'id' | 'name'>): Saved => ({
	alert: true,
	alerts: 0,
	ast: { op: 'all' },
	created_at: T0,
	last_alert_at: null,
	paused: false,
	q: 'a',
	rss: false,
	sort: null,
	unread: 0,
	updated_at: T0,
	...over
});

test.use({ viewport: { width: 1366, height: 800 } });

test('save alert: New alert, name, query, channels, Enter saves and opens it', async ({
	page,
	go,
	kbd,
	mock
}) => {
	mock.on('listSavedSearches', [saved({ id: 1, name: 'Existing' })]);
	mock.on('createSavedSearch', (request) => ({
		status: 201,
		body: saved({ id: 9, ...(request.postDataJSON() as object), name: 'Nature docs' })
	}));
	mock.on('getSavedSearch', saved({ id: 9, name: 'Nature docs', rss: true }));
	mock.on('listAlerts', { items: [], next_cursor: null });

	await go('/alerts');
	await expect(page.getByTestId('alerts-name-1')).toBeVisible();
	await kbd.tabTo(page.getByTestId('alerts-new'));
	await kbd.press('Enter');
	const dialog = page.getByRole('dialog', { name: 'Save search' });
	await expect(dialog).toBeVisible();
	await expect(dialog.getByLabel('Name')).toBeFocused();
	await page.keyboard.type('Nature docs');
	await kbd.press('Tab');
	await expect(dialog.getByLabel('Query')).toBeFocused();
	await page.keyboard.type('nature 4k');
	await kbd.press('Tab');
	await expect(dialog.getByRole('checkbox', { name: 'In the app' })).toBeFocused();
	await kbd.press('Tab');
	await expect(dialog.getByRole('checkbox', { name: 'RSS feed' })).toBeFocused();
	await kbd.press('Space');
	await expect(dialog.getByRole('checkbox', { name: 'RSS feed' })).toBeChecked();
	await kbd.press('Enter');
	await expect(page).toHaveURL(/\/alerts\/9/);
	await expect(page.getByRole('heading', { level: 1, name: 'Nature docs' })).toBeVisible();
	expect(mock.calls('createSavedSearch')).toHaveLength(1);
	expect(mock.calls('createSavedSearch')[0]!.postDataJSON()).toEqual({
		name: 'Nature docs',
		q: 'nature 4k',
		alert: true,
		rss: true
	});
	expect(await kbd.pointerEvents()).toEqual([]);
});

test('Escape closes the save dialog and returns focus to New alert', async ({
	page,
	go,
	kbd,
	mock
}) => {
	mock.on('listSavedSearches', [saved({ id: 1, name: 'Existing' })]);
	await go('/alerts');
	await expect(page.getByTestId('alerts-name-1')).toBeVisible();
	await kbd.tabTo(page.getByTestId('alerts-new'));
	await kbd.press('Enter');
	await expect(page.getByRole('dialog', { name: 'Save search' })).toBeVisible();
	await kbd.press('Escape');
	await expect(page.getByRole('dialog')).toHaveCount(0);
	await expect(page.getByTestId('alerts-new')).toBeFocused();
});

test('delete: Cancel has focus, Escape restores it, Enter on Delete removes the row', async ({
	page,
	go,
	kbd,
	mock
}) => {
	mock.on('listSavedSearches', [
		saved({ id: 1, name: 'First' }),
		saved({ id: 2, name: 'Second' }),
		saved({ id: 3, name: 'Third' })
	]);
	mock.on('deleteSavedSearch', { ok: true });
	await go('/alerts');
	const del = page.getByTestId('alerts-delete-2');
	await kbd.tabTo(del);
	await kbd.press('Enter');
	const dialog = page.getByRole('dialog', { name: "Delete 'Second' and its history?" });
	await expect(dialog).toBeVisible();
	await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused();
	await kbd.press('Escape');
	await expect(page.getByRole('dialog')).toHaveCount(0);
	await expect(del).toBeFocused();

	await kbd.press('Enter');
	await expect(dialog).toBeVisible();
	mock.on('listSavedSearches', [saved({ id: 1, name: 'First' }), saved({ id: 3, name: 'Third' })]);
	await kbd.press('Tab');
	await expect(dialog.getByRole('button', { name: 'Delete' })).toBeFocused();
	await kbd.press('Enter');
	await expect(page.getByTestId('alerts-row-2')).toHaveCount(0);
	await expect(page.getByTestId('alerts-name-3')).toBeFocused();
	expect(await kbd.pointerEvents()).toEqual([]);
});

test('j and k move through an alert’s rows', async ({ page, go, kbd, mock }) => {
	const hit = (n: number) => ({
		adult_kind: null,
		completed: null,
		composition: [],
		content_type: null,
		discovered_at: T0,
		files: 1,
		flags: [],
		hash: n.toString(16).padStart(40, '0'),
		hash_v2: null,
		id: n,
		junk_reasons: [],
		junk_score: null,
		last_scrape_at: null,
		leechers: null,
		metadata_at: T0,
		name: `Row ${n}`,
		piece_len: 16384,
		resolved_via: null,
		score: 0,
		seeders: null,
		seeders_approx: false,
		swarm_suspect: false,
		last_seen_at: T0,
		size: 1024,
		title: null,
		trackers_checked: null,
		trackers_reporting: null
	});
	mock.on('getSavedSearch', saved({ id: 1, name: 'Rows' }));
	mock.on('listAlerts', {
		items: [3, 2, 1].map((n) => ({ id: n, created_at: T0 + n, read: true, torrent: hit(n) })),
		next_cursor: null
	});
	await go('/alerts/1');
	await expect(page.getByTestId('alert-row-3')).toBeVisible();
	await kbd.press('Tab'); // skip link
	await kbd.tabTo(page.getByRole('row').first());
	await kbd.press('j');
	await expect(page.getByRole('row').nth(1)).toBeFocused();
	await kbd.press('k');
	await expect(page.getByRole('row').nth(0)).toBeFocused();
});
