// GS-4: the detail's Related section on the OpenAPI mock. Four labelled lists with counts, each row
// showing only what differs from the open torrent, "Compare these" pre-filling /compare (2 to 5), a
// withheld torrent showing nothing, and a row opening in the drawer.
import type { Page, Request } from '@playwright/test';
import { response } from '../src/lib/features/compare/testing';
import { expectNoA11yViolations } from './a11y';
import { HASH, detail, group, hit, mockTorrent, searchBody, siblings } from './detail.fixtures';
import { expect, test } from './support/test';

const A = 'a'.repeat(40);
const B = 'b'.repeat(40);
const C = 'c'.repeat(40);
const hashOf = (request: Request) => new URL(request.url()).pathname.split('/')[4] ?? '';

const related = (hash = HASH) => ({
	...siblings(hash),
	other_versions: {
		...group([
			hit({ hash: A, name: 'Show.S01.2160p.WEB-DL-GRP', resolution: '2160p', size: 9_000_000_000 }),
			hit({ hash: B, name: 'Show.S01.720p.HDTV-OTH', resolution: '720p' })
		]),
		truncated: true
	},
	identical_payload: group([hit({ hash: C, name: 'Show.S01.repost', seeders: 3 })]),
	same_file: {
		hits: [],
		reason: 'needs_v2_files' as const,
		status: 'unavailable' as const,
		truncated: false
	},
	similar_payload_partial: true
});

const section = (page: Page) => page.getByTestId('detail-related');
/** The section loads when its placeholder scrolls near (at once on /t), so bring that into view first. */
const reach = (page: Page) =>
	page.getByTestId('detail-related-mount').first().scrollIntoViewIfNeeded();
const list = (page: Page, key: string) => section(page).locator(`[data-list="${key}"]`);

test('/t lists the four groups with counts and only what differs', async ({ page, go, mock }) => {
	await page.setViewportSize({ width: 1366, height: 800 });
	mockTorrent(mock, detail(), hit({ resolution: '1080p' }));
	mock.on('getTorrentSiblings', related());
	await go(`/t/${HASH}`);
	await reach(page);
	await expect(section(page).getByTestId('detail-related-list-h')).toHaveText([
		'Other versions 2+',
		'Identical clones 1',
		'Same file',
		'Similar payloads 0'
	]);
	await expect(list(page, 'same_file')).toContainText(
		'Only v2 and hybrid torrents can be matched by file.'
	);
	await expect(list(page, 'similar_payload')).toContainText('None found.');
	await expect(list(page, 'similar_payload').getByTestId('detail-related-gap')).toBeVisible();
	const rows = section(page).getByTestId('detail-related-differs');
	await expect(rows).toHaveText([/^2160p\s·\s8\.38 GiB$/, /^720p$/, /^3 seeders$/]);
	await expectNoA11yViolations(page);
});

test('Compare these opens /compare with the open torrent and the picks', async ({
	page,
	go,
	mock
}) => {
	await page.setViewportSize({ width: 1366, height: 800 });
	mockTorrent(mock, detail(), hit({ resolution: '1080p' }));
	mock.on('getTorrentSiblings', related());
	mock.on('compareTorrents', (request) => {
		const { hashes } = request.postDataJSON() as { hashes: string[] };
		const body = response(hashes.length);
		body.torrents.forEach((t, i) => (t.hash = hashes[i]!));
		return { status: 200, body };
	});
	await go(`/t/${HASH}`);
	await reach(page);
	const compare = section(page).getByTestId('detail-related-compare');
	await expect(compare).toBeDisabled();
	await expect(section(page).getByTestId('detail-related-why')).toHaveText(
		'Pick 1 to 4 torrents to compare with this one.'
	);
	await section(page).getByRole('checkbox', { name: 'Pick Show.S01.repost to compare' }).check();
	await section(page)
		.getByRole('checkbox', { name: 'Pick Show.S01.2160p.WEB-DL-GRP to compare' })
		.check();
	await expect(compare).toHaveAttribute('href', `/compare?h=${HASH},${C},${A}`);
	await compare.focus();
	await page.keyboard.press('Enter');
	await expect(page).toHaveURL(new RegExp(`/compare\\?h=${HASH},${C},${A}$`));
	await expect(page.getByRole('heading', { level: 1, name: 'Compare' })).toBeVisible();
	await expect
		.poll(() => mock.calls('compareTorrents').map((r) => r.postDataJSON() as { hashes: string[] }))
		.toContainEqual({ hashes: [HASH, C, A] });
});

test('a torrent the API withholds shows no Related section', async ({ page, go, mock }) => {
	await page.setViewportSize({ width: 1366, height: 800 });
	mockTorrent(mock);
	mock.error('getTorrentSiblings', 404, 'torrent_not_found', "There's no torrent with that hash.");
	await go(`/t/${HASH}`);
	await expect(page.getByRole('heading', { level: 1, name: 'Show' })).toBeVisible();
	// The placeholder shows until the section loads, then the whole section goes.
	await reach(page);
	await expect.poll(() => mock.calls('getTorrentSiblings').length).toBeGreaterThan(0);
	await expect(page.getByRole('heading', { name: 'Related' })).toHaveCount(0, { timeout: 10_000 });
	await expect(section(page)).toHaveCount(0);
	await expect(page.getByTestId('detail-related-row')).toHaveCount(0);
});

test('a failed load says so and retries', async ({ page, go, mock }) => {
	await page.setViewportSize({ width: 1366, height: 800 });
	mockTorrent(mock);
	// Parsed asks for the same siblings, so fail by a flag rather than by counting calls.
	let fail = true;
	mock.on('getTorrentSiblings', () =>
		fail ? { status: 500, body: { error: { code: 'internal', message: 'x' } } } : related()
	);
	await go(`/t/${HASH}`);
	await reach(page);
	const failed = page.getByTestId('detail-related-failed');
	await expect(failed).toContainText("Couldn't load related torrents.");
	fail = false;
	await failed.getByRole('button', { name: 'Retry' }).click();
	await expect(section(page).getByTestId('detail-related-row')).toHaveCount(3);
});

test('in the drawer a row opens that torrent in the drawer', async ({ page, go, mock }) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	mock.on('getTorrent', (request) => ({
		status: 200,
		body: detail({ hash: hashOf(request), name: hashOf(request) === A ? 'Show.A' : 'Show.X' })
	}));
	mock.on('searchGet', searchBody([]));
	mock.on('getTorrentSiblings', (request) => related(hashOf(request)));
	mock.on('listTorrentFiles', { files: [], offset: 3, total: 3 });
	await go('/dev/detail');
	await page.getByTestId('push-b').click();
	const drawer = page.getByTestId('drawer');
	await expect(drawer).toBeVisible();
	await reach(page);
	const row = drawer.getByRole('link', { name: 'Show.S01.2160p.WEB-DL-GRP' });
	await row.focus();
	await page.keyboard.press('Enter');
	await expect(page).toHaveURL(new RegExp(`/t/${A}`));
	await expect(drawer.getByTestId('detail-raw-name')).toHaveText('Show.A');
	// The row went with the old torrent; focus stays in the drawer, on its title.
	await expect(drawer.locator('h2[tabindex]')).toBeFocused();
});
