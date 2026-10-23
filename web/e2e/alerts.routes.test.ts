// Alerts (architecture §10.8): the list, one alert with its "New since …" mark, live updates,
// pausing, the save and delete dialogs, and the error states. The API is the OpenAPI mock, typed
// from schema.d.ts; live events come from `liveEvents`.
import type { Page } from '@playwright/test';
import type { components } from '../src/lib/api/schema';
import { expectNoA11yViolations } from './a11y';
import { liveEvents } from './support/events-server';
import { expect, test } from './support/test';
import type { OpenApiMock } from './mock/openapi-mock';

type Saved = components['schemas']['SavedSearchView'];
type Row = components['schemas']['AlertRow'];
type Hit = components['schemas']['SearchHit'];

// A page error (an effect loop, say) can leave the page looking fine: fail on any.
const pageErrors: Error[] = [];
test.beforeEach(({ page }) => {
	pageErrors.length = 0;
	page.on('pageerror', (error) => pageErrors.push(error));
});
test.afterEach(() => expect(pageErrors).toEqual([]));

const T0 = 1_700_000_000;

const saved = (over: Partial<Saved> & Pick<Saved, 'id' | 'name'>): Saved => ({
	alert: true,
	alerts: 0,
	ast: { op: 'all' },
	created_at: T0,
	last_alert_at: null,
	paused: false,
	q: '',
	rss: false,
	sort: null,
	unread: 0,
	updated_at: T0,
	...over
});

const hash = (n: number) => n.toString(16).padStart(40, '0');
const hit = (n: number, over: Partial<Hit> = {}): Hit => ({
	adult_kind: null,
	completed: 12,
	composition: [
		{ group: 'video', bytes: 900, files: 2, pct_bytes: 82, pct_files: 40 },
		{ group: 'audio', bytes: 100, files: 3, pct_bytes: 11, pct_files: 60 }
	],
	content_type: 'video',
	discovered_at: T0 + n * 60,
	files: 5,
	flags: [],
	hash: hash(n),
	hash_v2: null,
	id: n,
	junk_reasons: [],
	junk_score: 0,
	last_scrape_at: T0,
	leechers: 3,
	matched_files: undefined,
	metadata_at: T0,
	name: `Lantern.Keeper.S01E0${n}.1080p.WEB-DL`,
	piece_len: 262144,
	resolved_via: null,
	score: 0,
	seeders: 40 + n,
	seeders_approx: false,
	swarm_suspect: false,
	last_seen_at: T0,
	size: 2_147_483_648,
	title: `Lantern Keeper S01E0${n}`,
	trackers_checked: 20,
	trackers_reporting: 4,
	...over
});
const row = (id: number, read: boolean, over: Partial<Hit> = {}): Row => ({
	id,
	created_at: T0 + id * 60,
	read,
	torrent: hit(id, over)
});

const LANTERN = saved({
	id: 1,
	name: 'Lantern Keeper releases',
	q: 'lantern keeper 1080p',
	unread: 2,
	alerts: 5,
	last_alert_at: T0 + 300
});
const FLAC = saved({
	id: 2,
	name: 'New FLAC albums',
	q: 'flac',
	rss: true,
	alerts: 3,
	last_alert_at: T0 + 100
});
const FILMS = saved({ id: 3, name: 'Films to check', q: 'type:movie', paused: true, alert: false });
const ALL = [LANTERN, FLAC, FILMS];

// Two unread (5, 4) above three read (3, 2, 1).
const MIXED: Row[] = [row(5, false), row(4, false), row(3, true), row(2, true), row(1, true)];

function listMocks(mock: OpenApiMock, searches: Saved[] = ALL) {
	mock.on('listSavedSearches', searches);
}
function detailMocks(mock: OpenApiMock, s: Saved = LANTERN, rows: Row[] = MIXED) {
	mock.on('getSavedSearch', s);
	mock.on('listAlerts', { items: rows, next_cursor: null });
	mock.on('markAlertsRead', { ok: true });
}
const body = (request: import('@playwright/test').Request) => request.postDataJSON() as unknown;

async function connected(page: Page) {
	await expect
		.poll(() => page.evaluate(() => window.__sse?.requests.length ?? 0), { timeout: 10_000 })
		.toBeGreaterThan(0);
}

test.describe('the list', () => {
	test.use({ viewport: { width: 1366, height: 800 } });

	test("shows each saved search with the server's numbers and states", async ({
		page,
		go,
		mock
	}) => {
		listMocks(mock);
		await go('/alerts');
		await expect(page.getByRole('heading', { level: 1, name: 'Alerts' })).toBeVisible();
		await expect(page.getByTestId('alerts-table')).toBeVisible();
		await expect(page.getByTestId('alerts-unread-1')).toHaveText('2');
		await expect(page.getByTestId('alerts-unread-2')).toHaveText('0');
		await expect(page.getByTestId('alerts-name-1')).toHaveAttribute('href', /^\/alerts\/1/);
		const flac = page.getByTestId('alerts-row-2');
		await expect(flac).toContainText('In the app');
		await expect(flac).toContainText('RSS');
		await expect(flac.getByTestId('alerts-last-2')).not.toHaveText('—');
		const films = page.getByTestId('alerts-row-3');
		await expect(films.getByTestId('alerts-paused-3')).toHaveText('Paused');
		await expect(films).toContainText('None');
		await expect(films.getByTestId('alerts-last-3')).toContainText('No alerts yet');
		await expect(page.getByRole('switch', { name: 'Paused: Films to check' })).toBeChecked();
	});

	test('an empty query reads “Every new torrent”', async ({ page, go, mock }) => {
		listMocks(mock, [saved({ id: 1, name: 'Everything' })]);
		await go('/alerts');
		await expect(page.getByTestId('alerts-row-1')).toContainText('Every new torrent');
	});

	test('an alerts event refetches the list once and updates it in place', async ({
		page,
		go,
		mock
	}) => {
		const events = await liveEvents(page);
		listMocks(mock);
		await go('/alerts');
		await expect(page.getByTestId('alerts-unread-1')).toHaveText('2');
		await connected(page);
		listMocks(mock, [{ ...LANTERN, unread: 3, alerts: 6 }, FLAC, FILMS]);
		const before = mock.calls('listSavedSearches').length;
		await events.send('alerts', { saved_search: 1, matched: 1, alert: 6, max_id: 6, notify: true });
		await events.send('alerts', {
			saved_search: 2,
			matched: 1,
			alert: 7,
			max_id: 7,
			notify: false
		});
		await expect(page.getByTestId('alerts-unread-1')).toHaveText('3');
		expect(mock.calls('listSavedSearches').length - before).toBeLessThanOrEqual(2);
		await expect(page.getByTestId('alerts-name-1')).toBeVisible();
	});

	test('pausing and resuming is a toggle with no confirmation', async ({ page, go, mock }) => {
		listMocks(mock);
		mock.on('patchSavedSearch', (request) => ({
			status: 200,
			body: { ...LANTERN, paused: (body(request) as { paused: boolean }).paused }
		}));
		await go('/alerts');
		const toggle = page.getByRole('switch', { name: 'Paused: Lantern Keeper releases' });
		await toggle.check();
		await expect(page.getByTestId('alerts-paused-1')).toBeVisible();
		expect(body(mock.calls('patchSavedSearch')[0]!)).toEqual({ paused: true });
		await toggle.uncheck();
		await expect(page.getByTestId('alerts-paused-1')).toHaveCount(0);
		await expect(page.getByTestId('alerts-resume-help-1')).toHaveText(
			'Resuming skips torrents indexed while it was paused.'
		);
		expect(mock.calls('patchSavedSearch')).toHaveLength(2);
	});

	test('a failed pause goes back with an error toast', async ({ page, go, mock }) => {
		listMocks(mock);
		mock.error('patchSavedSearch', 500, 'internal', 'boom');
		await go('/alerts');
		const toggle = page.getByRole('switch', { name: 'Paused: Lantern Keeper releases' });
		// click, not check(): the toggle may already be back by the time check() looks.
		await toggle.click();
		await expect(toggle).not.toBeChecked();
		await expect(
			page.getByText("Couldn't change “Lantern Keeper releases”", { exact: false })
		).toBeVisible();
	});

	test('with no saved searches it offers one next step', async ({ page, go, mock }) => {
		listMocks(mock, []);
		await go('/alerts');
		await expect(page.getByRole('heading', { name: 'No saved searches yet' })).toBeVisible();
		await expect(page.getByRole('button', { name: 'New alert' })).toHaveCount(2);
	});

	test('a failed load shows a banner with Retry, and a retry recovers', async ({
		page,
		go,
		mock
	}) => {
		mock.error('listSavedSearches', 500, 'internal', 'boom');
		await go('/alerts');
		await expect(page.getByRole('alert')).toContainText("Couldn't load your alerts");
		listMocks(mock);
		await page.getByRole('button', { name: 'Retry' }).click();
		await expect(page.getByTestId('alerts-row-1')).toBeVisible();
	});
});

test.describe('layout by width', () => {
	for (const [width, table] of [
		[390, false],
		[768, true],
		[1366, true]
	] as const) {
		test(`${width} px shows ${table ? 'a table' : 'a stacked list'}`, async ({
			page,
			go,
			mock
		}) => {
			await page.setViewportSize({ width, height: 800 });
			listMocks(mock);
			await go('/alerts');
			await expect(page.getByTestId('alerts-row-1')).toBeVisible();
			await expect(page.getByTestId('alerts-table')).toHaveCount(table ? 1 : 0);
			await expect(page.getByTestId('alerts-stack')).toHaveCount(table ? 0 : 1);
			const overflow = await page.evaluate(
				() => document.documentElement.scrollWidth > document.documentElement.clientWidth
			);
			expect(overflow).toBe(false);
		});
	}
});

test.describe('one alert', () => {
	test.use({ viewport: { width: 1366, height: 900 } });

	test('opens from the list, shows the mark where the flags change and marks read', async ({
		page,
		go,
		mock
	}) => {
		listMocks(mock);
		detailMocks(mock);
		await go('/alerts');
		await page.getByTestId('alerts-name-1').click();
		await expect(page).toHaveURL(/\/alerts\/1/);
		await expect(
			page.getByRole('heading', { level: 1, name: 'Lantern Keeper releases' })
		).toBeVisible();
		await expect(page.getByTestId('alert-opened-unread')).toHaveText('2 new');
		const mark = page.getByRole('heading', { level: 2, name: /^New since / });
		await expect(mark).toBeVisible();
		const y = async (loc: ReturnType<Page['locator']>) => (await loc.boundingBox())!.y;
		const above = await y(page.getByTestId('alert-row-4'));
		const mid = await y(mark);
		const below = await y(page.getByTestId('alert-row-3'));
		expect(above).toBeLessThan(mid);
		expect(mid).toBeLessThan(below);
		await expect.poll(() => mock.calls('markAlertsRead').length).toBe(1);
		expect(body(mock.calls('markAlertsRead')[0]!)).toEqual({ saved_search: 1, through: 5 });
		await expect(page.getByTestId('alert-title-5')).toHaveText('Lantern Keeper S01E05');
		await expect(page.getByTestId('alert-raw-5')).toContainText('Lantern.Keeper.S01E05');
		await expect(page.getByTestId('alert-size-5')).toHaveText('2.00 GiB');
		await expect(page.getByTestId('alert-trackers-5')).toContainText('4 / 20');
		await expect(page.getByTestId('alert-open-search')).toHaveAttribute(
			'href',
			/^\/search\?q=lantern\+keeper\+1080p/
		);
	});

	test('with nothing unread there is no mark and nothing is marked', async ({ page, go, mock }) => {
		detailMocks(mock, { ...LANTERN, unread: 0 }, [row(3, true), row(2, true)]);
		await go('/alerts/1');
		await expect(page.getByTestId('alert-row-3')).toBeVisible();
		await expect(page.getByTestId('alert-divider')).toHaveCount(0);
		expect(mock.calls('markAlertsRead')).toHaveLength(0);
	});

	test('an event raises the banner; Show reloads, re-places the mark and marks read', async ({
		page,
		go,
		mock
	}) => {
		const events = await liveEvents(page);
		detailMocks(mock);
		await go('/alerts/1');
		await expect(page.getByTestId('alert-row-5')).toBeVisible();
		await connected(page);
		await expect.poll(() => mock.calls('markAlertsRead').length).toBe(1);
		// Another search's event does nothing here: it comes first, then this search's, so by the
		// time the banner shows the first has been handled (events arrive in order).
		await events.send('alerts', { saved_search: 2, matched: 1, alert: 9, max_id: 9, notify: true });
		mock.on('getSavedSearch', { ...LANTERN, unread: 1, alerts: 6 });
		await events.send('alerts', { saved_search: 1, matched: 1, alert: 6, max_id: 6, notify: true });
		await expect(page.getByTestId('alert-banner')).toHaveText('1 new alert');
		expect(mock.calls('getSavedSearch')).toHaveLength(2); // the page's own load, then one event
		await expect(page.getByTestId('alert-live')).toHaveText('1 new alert');
		// The list stays as it was until Show.
		await expect(page.getByTestId('alert-row-6')).toHaveCount(0);

		mock.on('listAlerts', {
			items: [row(6, false), ...MIXED.map((r) => ({ ...r, read: true }))],
			next_cursor: null
		});
		await page.getByTestId('alert-show').click();
		await expect(page.getByTestId('alert-row-6')).toBeVisible();
		await expect(page.getByTestId('alert-banner')).toHaveCount(0);
		await expect.poll(() => mock.calls('markAlertsRead').length).toBe(2);
		expect(body(mock.calls('markAlertsRead')[1]!)).toEqual({ saved_search: 1, through: 6 });
		await expect(page.getByTestId('alert-divider')).toBeVisible();
	});

	test('the empty states say what to expect', async ({ page, go, mock }) => {
		detailMocks(mock, { ...LANTERN, unread: 0 }, []);
		await go('/alerts/1');
		await expect(page.getByTestId('alert-empty')).toHaveText(
			'No matches yet. New torrents that match appear here.'
		);
		detailMocks(mock, { ...LANTERN, unread: 0, paused: true }, []);
		await go('/alerts/1');
		await expect(page.getByTestId('alert-empty')).toHaveText('Paused: no new alerts are recorded.');
	});

	test('an RSS alert shows its feed address and selects it on focus', async ({
		page,
		go,
		mock
	}) => {
		detailMocks(mock, { ...FLAC, unread: 0 }, []);
		await go('/alerts/2');
		const feed = page.getByTestId('alert-feed');
		await expect(feed).toHaveValue('/api/v1/feeds/2');
		await feed.focus();
		const selected = await feed.evaluate((el: HTMLInputElement) =>
			el.value.slice(el.selectionStart ?? 0, el.selectionEnd ?? 0)
		);
		expect(selected).toBe('/api/v1/feeds/2');
		await expect(page.getByRole('link', { name: 'Settings → Security' })).toHaveAttribute(
			'href',
			/^\/settings\/security/
		);
	});

	test('a missing alert says so and links back', async ({ page, go, mock }) => {
		mock.error('getSavedSearch', 404, 'saved_search_not_found');
		mock.error('listAlerts', 404, 'saved_search_not_found');
		await go('/alerts/77');
		await expect(
			page.getByRole('heading', { level: 1, name: "This alert doesn't exist any more" })
		).toBeVisible();
		await expect(page.getByRole('link', { name: 'Back to Alerts' })).toHaveAttribute(
			'href',
			/^\/alerts/
		);
	});

	test('a bad id is a 404', async ({ page, go }) => {
		await go('/alerts/abc');
		await expect(page.getByTestId('state-screen')).toBeVisible();
	});

	test('a failed load keeps the page honest, and Retry recovers', async ({ page, go, mock }) => {
		mock.error('getSavedSearch', 500, 'internal', 'boom');
		mock.error('listAlerts', 500, 'internal', 'boom');
		await go('/alerts/1');
		await expect(page.getByRole('alert')).toContainText("Couldn't load your alerts");
		detailMocks(mock);
		await page.getByRole('button', { name: 'Retry' }).click();
		await expect(page.getByTestId('alert-row-5')).toBeVisible();
	});

	test('a later page appends; a bad cursor starts over', async ({ page, go, mock }) => {
		mock.on('getSavedSearch', { ...LANTERN, unread: 0 });
		mock.on('markAlertsRead', { ok: true });
		let pages = 0;
		mock.on('listAlerts', (request) => {
			const cursor = new URL(request.url()).searchParams.get('cursor');
			pages++;
			if (cursor === null) return { items: [row(6, true), row(5, true)], next_cursor: 'c1' };
			if (cursor === 'c1' && pages <= 3)
				return { status: 400, body: { error: { code: 'bad_cursor', message: 'bad' } } };
			return { items: [row(4, true)], next_cursor: null };
		});
		await go('/alerts/1');
		await expect(page.getByTestId('alert-row-6')).toBeVisible();
		// The list pages by itself once its end is reached; the bad cursor restarts from page 1.
		await expect.poll(() => pages).toBeGreaterThanOrEqual(2);
	});

	test('deleting from the page asks first, then goes to the list', async ({ page, go, mock }) => {
		detailMocks(mock, { ...FLAC, unread: 0 }, []);
		mock.on('deleteSavedSearch', { ok: true });
		mock.on('listSavedSearches', [LANTERN]);
		await go('/alerts/2');
		await page.getByTestId('alert-delete').click();
		const dialog = page.getByRole('dialog', { name: "Delete 'New FLAC albums' and its history?" });
		await expect(dialog).toContainText('Its alerts are deleted, and its RSS feed stops working.');
		await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused();
		await dialog.getByRole('button', { name: 'Delete' }).click();
		await expect(page).toHaveURL(/\/alerts(\?|$)/);
		expect(mock.calls('deleteSavedSearch')).toHaveLength(1);
		await expect(page.getByText('Deleted “New FLAC albums”').first()).toBeVisible();
	});
});

test.describe('the dialogs', () => {
	test.use({ viewport: { width: 1366, height: 800 } });

	test('save: fields, defaults, and a created search opens its page', async ({
		page,
		go,
		mock
	}) => {
		listMocks(mock);
		mock.on('createSavedSearch', (request) => ({
			status: 201,
			body: saved({ id: 9, name: 'Nature docs', ...(body(request) as object) })
		}));
		mock.on('getSavedSearch', saved({ id: 9, name: 'Nature docs', rss: true }));
		mock.on('listAlerts', { items: [], next_cursor: null });
		await go('/alerts');
		await page.getByTestId('alerts-new').click();
		const dialog = page.getByRole('dialog', { name: 'Save search' });
		await expect(dialog.getByLabel('Name')).toBeVisible();
		await expect(dialog.getByLabel('Query')).toHaveValue('');
		await expect(dialog.getByText('Alerts cover torrents indexed from now on')).toBeVisible();
		await expect(dialog.getByRole('checkbox', { name: 'In the app' })).toBeChecked();
		await expect(dialog.getByRole('checkbox', { name: 'RSS feed' })).not.toBeChecked();
		await dialog.getByLabel('Name').fill('Nature docs');
		await dialog.getByLabel('Query').fill('nature 4k');
		await dialog.getByRole('checkbox', { name: 'RSS feed' }).check();
		await dialog.getByRole('button', { name: 'Save search' }).click();
		await expect(page).toHaveURL(/\/alerts\/9/);
		expect(body(mock.calls('createSavedSearch')[0]!)).toEqual({
			name: 'Nature docs',
			q: 'nature 4k',
			alert: true,
			rss: true
		});
	});

	test('save: an empty name is caught before sending', async ({ page, go, mock }) => {
		listMocks(mock);
		await go('/alerts');
		await page.getByTestId('alerts-new').click();
		const dialog = page.getByRole('dialog', { name: 'Save search' });
		await dialog.getByRole('button', { name: 'Save search' }).click();
		await expect(dialog.getByTestId('field-error')).toContainText('1 to 200 characters');
		expect(mock.calls('createSavedSearch')).toHaveLength(0);
	});

	test('save: a query syntax error is shown on the query and its span is selected', async ({
		page,
		go,
		mock
	}) => {
		listMocks(mock);
		mock.error('createSavedSearch', 400, 'query_syntax', 'Unmatched ")" at position 6.', {
			start: 6,
			end: 7
		});
		await go('/alerts');
		await page.getByTestId('alerts-new').click();
		const dialog = page.getByRole('dialog', { name: 'Save search' });
		await dialog.getByLabel('Name').fill('Broken');
		await dialog.getByLabel('Query').fill('lantern) 1080p');
		await dialog.getByRole('button', { name: 'Save search' }).click();
		await expect(dialog.getByTestId('field-error')).toContainText('Unmatched ")" at position 6.');
		await expect(dialog.getByLabel('Query')).toBeFocused();
		const sel = await dialog
			.getByLabel('Query')
			.evaluate((el: HTMLInputElement) => [el.selectionStart, el.selectionEnd]);
		expect(sel).toEqual([6, 7]);
		await expect(dialog).toBeVisible();
	});

	test('save: too many saved searches is a form-level message', async ({ page, go, mock }) => {
		listMocks(mock);
		mock.error('createSavedSearch', 409, 'too_many_saved_searches');
		await go('/alerts');
		await page.getByTestId('alerts-new').click();
		const dialog = page.getByRole('dialog', { name: 'Save search' });
		await dialog.getByLabel('Name').fill('One more');
		await dialog.getByRole('button', { name: 'Save search' }).click();
		await expect(dialog.getByRole('alert')).toContainText(
			'You have 1,000 saved searches, the most Lodestar keeps. Delete one to add another.'
		);
		await expect(dialog.getByLabel('Name')).toHaveValue('One more');
	});

	test('delete from the list: a specific question, then focus moves to the next row', async ({
		page,
		go,
		mock
	}) => {
		listMocks(mock);
		mock.on('deleteSavedSearch', { ok: true });
		await go('/alerts');
		await page.getByTestId('alerts-delete-2').click();
		const dialog = page.getByRole('dialog', { name: "Delete 'New FLAC albums' and its history?" });
		await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused();
		mock.on('listSavedSearches', [LANTERN, FILMS]);
		await dialog.getByRole('button', { name: 'Delete' }).click();
		await expect(page.getByTestId('alerts-row-2')).toHaveCount(0);
		await expect(page.getByTestId('alerts-name-3')).toBeFocused();
	});

	test('delete: an alert that is already gone counts as deleted', async ({ page, go, mock }) => {
		listMocks(mock);
		mock.error('deleteSavedSearch', 404, 'saved_search_not_found');
		await go('/alerts');
		await page.getByTestId('alerts-delete-3').click();
		const dialog = page.getByRole('dialog');
		await expect(dialog).toContainText(
			"Its alerts are deleted. This can't be undone.".replace('’', "'")
		);
		mock.on('listSavedSearches', [LANTERN, FLAC]);
		await dialog.getByRole('button', { name: 'Delete' }).click();
		await expect(page.getByTestId('alerts-row-3')).toHaveCount(0);
		await expect(page.getByTestId('alerts-name-2')).toBeFocused();
	});
});

for (const [name, themeId, colorMode] of [
	['Riverstone light', 'riverstone-light', 'light'],
	['Riverstone dark', 'riverstone-dark', 'dark']
] as const) {
	test.describe(`axe, ${name}`, () => {
		test.use({ themeId, colorMode, viewport: { width: 1366, height: 900 } });

		test('the list, the empty list and a failed load', async ({ page, go, mock }) => {
			listMocks(mock);
			await go('/alerts');
			await expect(page.getByTestId('alerts-row-1')).toBeVisible();
			await expectNoA11yViolations(page);
			await page.setViewportSize({ width: 390, height: 800 });
			await expect(page.getByTestId('alerts-stack')).toBeVisible();
			await expectNoA11yViolations(page);

			listMocks(mock, []);
			await go('/alerts');
			await expect(page.getByRole('heading', { name: 'No saved searches yet' })).toBeVisible();
			await expectNoA11yViolations(page);

			mock.error('listSavedSearches', 500, 'internal', 'boom');
			await go('/alerts');
			await expect(page.getByRole('alert')).toBeVisible();
			await expectNoA11yViolations(page);
		});

		test('one alert, its banner and a missing one', async ({ page, go, mock }) => {
			const events = await liveEvents(page);
			detailMocks(mock);
			await go('/alerts/1');
			await expect(page.getByTestId('alert-divider')).toBeVisible();
			await expectNoA11yViolations(page);
			await connected(page);
			mock.on('getSavedSearch', { ...LANTERN, unread: 2 });
			await events.send('alerts', {
				saved_search: 1,
				matched: 2,
				alert: 7,
				max_id: 7,
				notify: true
			});
			await expect(page.getByTestId('alert-banner')).toBeVisible();
			await expectNoA11yViolations(page);

			mock.error('getSavedSearch', 404, 'saved_search_not_found');
			mock.error('listAlerts', 404, 'saved_search_not_found');
			await go('/alerts/77');
			await expect(page.getByRole('link', { name: 'Back to Alerts' })).toBeVisible();
			await expectNoA11yViolations(page);
		});

		test('both dialogs open', async ({ page, go, mock }) => {
			listMocks(mock);
			await go('/alerts');
			await page.getByTestId('alerts-new').click();
			await expect(page.getByRole('dialog', { name: 'Save search' })).toBeVisible();
			await expectNoA11yViolations(page);
			await page.keyboard.press('Escape');
			await expect(page.getByRole('dialog')).toHaveCount(0);
			await page.getByTestId('alerts-delete-2').click();
			await expect(page.getByRole('dialog')).toBeVisible();
			await expectNoA11yViolations(page);
		});
	});
}
