// UI check (review 04 §6c): the flow a moderate user will run from a phone, never walked end to
// end: open the Alerts list, open an alert, open one of its torrents and send it to the download
// client. At 390 px, tapping (touch), with nothing clipped sideways and the confirmation visible.
// Waits are conditions with a 10 s deadline; no fixed sleeps. Named `.routes.test.ts` so the
// `core` project picks it up.
import type { components } from '../../src/lib/api/schema';
import { body, hashOf, hits, indexState, row, server } from '../search-results.fixtures';
import { expect as baseExpect, test } from '../support/test';

const expect = baseExpect.configure({ timeout: 10_000 });

test.use({ viewport: { width: 390, height: 800 }, hasTouch: true, isMobile: true });

type Saved = components['schemas']['SavedSearchView'];
type AlertRow = components['schemas']['AlertRow'];

const T0 = 1_700_000_000;
const LANTERN: Saved = {
	alert: true,
	alerts: 2,
	ast: { op: 'all' },
	created_at: T0,
	id: 1,
	last_alert_at: T0,
	name: 'Lantern Keeper releases',
	paused: false,
	q: 'lantern keeper',
	rss: false,
	sort: null,
	unread: 1,
	updated_at: T0
};
/** One alert row for result `n`, its torrent taken from the Search fixtures. */
const alertRow = (n: number, read: boolean): AlertRow => ({
	id: n,
	created_at: T0 + n,
	read,
	torrent: hits(3)[n]!
});

test('phone: open an alert, open it in Search, open a torrent, send it to the client', async ({
	page,
	go,
	mock
}) => {
	const sent: string[][] = [];
	server(mock, () => body(hits(3)));
	mock.on('searchIndexState', indexState(5_000_000));
	mock.on('listSavedSearches', [LANTERN]);
	mock.on('getSavedSearch', LANTERN);
	mock.on('listAlerts', { items: [alertRow(2, false), alertRow(1, true)], next_cursor: null });
	mock.on('markAlertsRead', { ok: true });
	mock.on('getSettings', {
		section: 'integrations',
		revision: 1,
		restart_required: [],
		fields: {
			clients: { value: [{ id: 'q', name: 'qBittorrent' }], source: 'db', read_only: false },
			default_client: { value: '', source: 'default', read_only: false }
		}
	} as never);
	mock.on('sendToClient', (request) => {
		const { hashes } = request.postDataJSON() as { hashes: string[] };
		sent.push(hashes);
		return {
			client: 'q',
			sent: hashes.length,
			results: hashes.map((hash) => ({ hash, outcome: 'sent' }))
		} as never;
	});

	// Nothing on the way pushes the page wider than the phone.
	const fits = async () =>
		expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(
			390
		);
	await go('/alerts');
	await fits();
	await page.getByTestId('alerts-name-1').tap();
	await expect(page).toHaveURL(/\/alerts\/1/);
	await fits();
	await page.getByTestId('alert-open-search').tap();
	await expect(page).toHaveURL(/\/search\?q=lantern/);
	await fits();
	await row(page, 1).tap();
	const send = page.getByTestId('detail-send');
	await expect(send).toHaveText('Send to qBittorrent');
	await fits();
	await send.tap();
	await expect(page.getByText('Sent to qBittorrent')).toBeVisible();
	expect(sent).toEqual([[hashOf(1)]]);
});
