// S-detail on the OpenAPI mock: /t/{hash} as a page (summary, contents, parsed, the 3,000-file tree
// in a worker, the error states) and the drawer on /dev/detail (focus, Esc, resize, widths).
import type { Page, Request } from '@playwright/test';
import { expectNoA11yViolations } from './a11y';
import {
	BIG,
	HASH,
	detail,
	group,
	hit,
	manyFiles,
	mockTorrent,
	searchBody,
	siblings
} from './detail.fixtures';
import { expect, test } from './support/test';

// Finite animations only: a loading skeleton pulses forever.
const settled = (page: Page) =>
	page.evaluate(() =>
		Promise.all(
			document
				.getAnimations()
				.filter((a) => a.effect?.getComputedTiming().iterations !== Infinity)
				.map((a) => a.finished)
		)
	);
const hashOf = (request: Request) => new URL(request.url()).pathname.split('/')[4] ?? '';
const queryHash = (request: Request) =>
	new URL(request.url()).searchParams.get('q')?.replace('hash:', '') ?? '';

for (const [themeId, colorMode] of [
	['riverstone-light', 'light'],
	['riverstone-dark', 'dark']
] as const) {
	test.describe(`/t/{hash}, ${themeId}`, () => {
		test.use({ themeId, colorMode });

		for (const [width, height] of [
			[1366, 800],
			[390, 844]
		]) {
			test(`renders every section and is axe-clean at ${width}`, async ({ page, go, mock }) => {
				await page.setViewportSize({ width, height });
				mockTorrent(mock);
				await go(`/t/${HASH}`);
				await expect(page.getByRole('heading', { level: 1, name: 'Show' })).toBeVisible();
				await expect(page.getByTestId('detail-size')).toHaveText('2.70 GiB');
				await expect(page.getByTestId('detail-trackers')).toHaveText(
					'Reported by 4 of 20 trackers checked'
				);
				await expect(page.getByTestId('detail-compbar-dominant')).toHaveText('99% video');
				await expect(page.getByRole('tree', { name: 'Files in this torrent' })).toBeVisible();
				// Parsed loads when its section scrolls near
				await page.locator('[data-section="parsed"]').scrollIntoViewIfNeeded();
				await expect(page.getByTestId('detail-parsed-years')).toHaveText('2019');
				// the page is the results-column width; its header keeps the manuscript width
				const box = await page.locator('.ls-detail-page').boundingBox();
				const head = await page.locator('.ls-detail-page > header').boundingBox();
				expect(head!.width).toBeLessThanOrEqual(720 + 1);
				const want = Math.min(page.viewportSize()!.width, 1120);
				expect(Math.abs(box!.width - want)).toBeLessThanOrEqual(1);
				await settled(page);
				await expectNoA11yViolations(page);
			});
		}
	});
}

test('the loading state is axe-clean, and so is the page once it loads', async ({
	page,
	go,
	mock
}) => {
	// Hold the torrent until axe has seen the loading state (smoke runs could catch it by chance).
	let release!: () => void;
	const held = new Promise<void>((r) => (release = r));
	mockTorrent(mock);
	// The mock awaits a handler; its type only allows sync ones.
	mock.on('getTorrent', (async () => {
		await held;
		return detail();
	}) as never);
	await go(`/t/${HASH}`);
	const loading = page.getByRole('status', { name: 'Loading torrent…' });
	await expect(loading).toHaveAttribute('aria-busy', 'true');
	await expectNoA11yViolations(page);
	release();
	await expect(page.getByRole('heading', { level: 1, name: 'Show' })).toBeVisible();
	await expect(loading).toHaveCount(0);
	await settled(page);
	await expectNoA11yViolations(page);
});

test('looks the hit up by hash on a direct load', async ({ page, go, mock }) => {
	mockTorrent(mock);
	await go(`/t/${HASH}`);
	await expect(page.getByRole('heading', { level: 1, name: 'Show' })).toBeVisible();
	const calls = mock.calls('searchGet');
	expect(calls).toHaveLength(1);
	const q = new URL(calls[0]!.url()).searchParams;
	expect(q.get('q')).toBe(`hash:${HASH}`);
	expect(q.get('limit')).toBe('1');
	expect(q.get('hidden')).toBe('true');
});

test('a never-scraped torrent says so', async ({ page, go, mock }) => {
	mockTorrent(
		mock,
		detail({
			seeders: null,
			leechers: null,
			last_scrape_at: null,
			trackers_checked: null,
			trackers_reporting: null
		}),
		hit({ seeders: null, leechers: null })
	);
	await go(`/t/${HASH}`);
	await expect(page.getByTestId('detail-scrape-age')).toHaveText('Never scraped');
	await expect(page.getByTestId('detail-seeders')).toContainText('Never scraped');
});

test('3,000 files page in and build in the worker; the filter runs there too', async ({
	page,
	go,
	mock
}) => {
	const all = manyFiles(3000);
	mock.on(
		'getTorrent',
		detail({ hash: BIG, file_list: all.slice(0, 500), file_list_total: 3000, files: 3000 })
	);
	mock.on('searchGet', searchBody([hit({ hash: BIG, files: 3000 })]));
	mock.on('getTorrentSiblings', siblings(BIG));
	mock.on('listTorrentFiles', (request) => {
		const u = new URL(request.url());
		const offset = Number(u.searchParams.get('offset') ?? 0);
		const limit = Number(u.searchParams.get('limit') ?? 1000);
		return { files: all.slice(offset, offset + limit), offset, total: 3000 };
	});
	await go(`/t/${BIG}`);
	const files = page.getByTestId('detail-file-tree');
	await expect(files).toHaveAttribute('data-tree-worker', 'true');
	await expect.poll(() => mock.calls('listTorrentFiles').length, { timeout: 15_000 }).toBe(3);
	await expect(files).toHaveAttribute('aria-busy', 'false', { timeout: 15_000 });
	const pages = mock
		.calls('listTorrentFiles')
		.map((r) => new URL(r.url()).searchParams.get('offset'));
	expect(pages.sort()).toEqual(['1500', '2500', '500']);
	// the worker was a same-origin module, never a blob: URL
	expect(page.workers().length).toBeGreaterThan(0);
	for (const w of page.workers()) expect(w.url().startsWith('blob:')).toBe(false);
	const tree = page.getByRole('tree', { name: 'Files in this torrent' });
	await page.getByRole('searchbox', { name: 'Filter files' }).fill('recording-02999');
	await expect(tree.getByRole('treeitem')).toHaveCount(3); // Archive, part4, the file
	await expect(tree.getByRole('treeitem').last()).toContainText('recording-02999.flac');
});

test('torrent_not_found: says so and goes back to Search', async ({ page, go, mock }) => {
	mock.error('getTorrent', 404, 'torrent_not_found', 'No such torrent');
	mock.on('searchGet', searchBody([]));
	await go(`/t/${HASH}`);
	const box = page.getByTestId('detail-not-found');
	await expect(box).toContainText("This torrent isn't in the index");
	await expect(box.getByRole('link', { name: 'Back to Search' })).toHaveAttribute(
		'href',
		'/search'
	);
	await expectNoA11yViolations(page);
});

test('adult_hidden: says why and links to appearance settings', async ({ page, go, mock }) => {
	mock.error('getTorrent', 404, 'adult_hidden', 'Hidden');
	mock.on('searchGet', searchBody([]));
	await go(`/t/${HASH}`);
	const box = page.getByTestId('detail-adult-hidden');
	await expect(box).toContainText('This torrent is Adult, and Show adult content is off');
	await expect(box.getByRole('link', { name: 'Open appearance settings' })).toHaveAttribute(
		'href',
		'/settings/appearance'
	);
	await expectNoA11yViolations(page);
});

test('a server error shows a banner with Retry, and Retry recovers', async ({ page, go, mock }) => {
	let up = false;
	mock.on('getTorrent', () =>
		up
			? { status: 200, body: detail() }
			: { status: 503, body: { error: { code: 'unavailable', message: 'down' } } }
	);
	mock.on('searchGet', searchBody([hit()]));
	mock.on('getTorrentSiblings', siblings());
	mock.on('listTorrentFiles', { files: [], offset: 3, total: 3 });
	await go(`/t/${HASH}`);
	const alert = page.getByRole('alert');
	await expect(alert).toContainText("Couldn't load this torrent");
	await expectNoA11yViolations(page);
	up = true;
	await alert.getByRole('button', { name: 'Retry' }).click();
	await expect(page.getByTestId('detail-size')).toHaveText('2.70 GiB');
	await expect(page.getByRole('alert')).toHaveCount(0);
});

test('a malformed hash is the shell 404', async ({ page, go }) => {
	await go('/t/not-a-hash');
	await expect(page.getByRole('heading', { name: /not found|404/i }).first()).toBeVisible();
});

test('a hash typed in capitals opens the torrent', async ({ page, go, mock }) => {
	mockTorrent(mock);
	await go(`/t/${HASH.toUpperCase()}`);
	await expect(page.getByRole('heading', { level: 1, name: 'Show' })).toBeVisible();
	expect(hashOf(mock.calls('getTorrent')[0]!)).toBe(HASH);
});

test.describe('the drawer on /dev/detail', () => {
	const open = async (page: Page) => {
		await page.getByTestId('open-a').focus();
		await page.keyboard.press('Enter');
		await expect(page.getByTestId('drawer')).toBeVisible();
	};
	const A = 'a'.repeat(40);
	test.beforeEach(async ({ mock }) => {
		mock.on('getTorrent', (request) => ({
			status: 200,
			body: detail({ hash: hashOf(request), name: hashOf(request) === A ? 'Show.A' : 'Show.B' })
		}));
		mock.on('searchGet', (request) => {
			const h = queryHash(request);
			return {
				status: 200,
				body: searchBody([
					hit({
						hash: h,
						title: h === A ? 'Alpha show' : 'Beta show',
						name: 'raw.' + h.slice(0, 4)
					})
				])
			};
		});
		mock.on('getTorrentSiblings', { ...siblings(), other_versions: group() });
		mock.on('listTorrentFiles', { files: [], offset: 3, total: 3 });
	});

	test('opens with focus on the title; Esc returns focus to the invoker and goes Back', async ({
		page,
		go
	}) => {
		await page.setViewportSize({ width: 1440, height: 900 });
		await go('/dev/detail');
		await expect(page.locator('.ls-detail-host')).toHaveAttribute('data-mode', 'inline');
		await open(page);
		await expect(page).toHaveURL(new RegExp(`/t/${A}`));
		await expect(page.getByRole('heading', { level: 2, name: 'Alpha show' })).toBeFocused();
		await expect(page.getByTestId('detail-size')).toBeVisible();
		await page.keyboard.press('Escape');
		await expect(page.getByTestId('drawer')).toHaveCount(0);
		await expect(page.getByTestId('open-a')).toBeFocused();
		await expect(page).toHaveURL(/\/dev\/detail/);
	});

	test('the browser Back closes it', async ({ page, go }) => {
		await page.setViewportSize({ width: 1440, height: 900 });
		await go('/dev/detail');
		await open(page);
		await page.goBack();
		await expect(page.getByTestId('drawer')).toHaveCount(0);
	});

	test('opening a second torrent keeps one history entry: Esc closes, and it opens again', async ({
		page,
		go
	}) => {
		await page.setViewportSize({ width: 1440, height: 900 });
		await go('/dev/detail');
		await open(page);
		await page.getByTestId('push-b').focus();
		await page.keyboard.press('Enter');
		const drawer = page.getByTestId('drawer');
		await expect(drawer.getByRole('heading', { level: 2 })).toHaveText('Beta show');
		await drawer.getByRole('heading', { level: 2 }).focus();
		await page.keyboard.press('Escape');
		await expect(drawer).toHaveCount(0);
		await expect(page).toHaveURL(/\/dev\/detail/);
		await open(page);
		await expect(drawer.getByRole('heading', { level: 2 })).toHaveText('Alpha show');
	});

	test('swapping torrents keeps focus and announces the new title', async ({ page, go }) => {
		await page.setViewportSize({ width: 1440, height: 900 });
		await go('/dev/detail');
		await open(page);
		const title = page.getByTestId('drawer').getByRole('heading', { level: 2 });
		await expect(title).toHaveText('Alpha show');
		await page.getByTestId('open-b').focus();
		await page.keyboard.press('Enter');
		await expect(title).toHaveText('Beta show');
		await expect(page.getByTestId('open-b')).toBeFocused();
		await expect(page.getByTestId('detail-announce')).toHaveText('Showing Beta show');
		// a replace, not a push: one Back leaves the drawer
		await page.goBack();
		await expect(page.getByTestId('drawer')).toHaveCount(0);
	});

	test('resizes by keyboard and keeps the width across a reload', async ({ page, go }) => {
		await page.setViewportSize({ width: 1440, height: 900 });
		await go('/dev/detail');
		await open(page);
		const handle = page.getByRole('separator', { name: 'Resize details' });
		const before = Number(await handle.getAttribute('aria-valuenow'));
		await handle.focus();
		await page.keyboard.press('ArrowLeft');
		const after = Number(await handle.getAttribute('aria-valuenow'));
		expect(after).toBe(Math.min(640, before + 16));
		expect(await page.evaluate(() => localStorage.getItem('lodestar.drawer.width'))).toBe(
			String(after)
		);
		await page.keyboard.press('Escape');
		await expect(page).toHaveURL(/\/dev\/detail/);
		await page.reload();
		await open(page);
		await expect(page.getByRole('separator', { name: 'Resize details' })).toHaveAttribute(
			'aria-valuenow',
			String(after)
		);
	});

	test('an overlay at 1024 keeps the list visible', async ({ page, go }) => {
		await page.setViewportSize({ width: 1024, height: 800 });
		await go('/dev/detail');
		await expect(page.locator('.ls-detail-host')).toHaveAttribute('data-mode', 'overlay');
		await open(page);
		await settled(page); // the slide-in is a transform
		const drawer = await page.getByTestId('drawer').boundingBox();
		expect(Math.round(drawer!.width)).toBe(560);
		expect(Math.round(drawer!.x + drawer!.width)).toBe(1024);
		await expect(page.getByTestId('open-a')).toBeVisible();
		const opener = await page.getByTestId('open-a').boundingBox();
		expect(opener!.x + opener!.width).toBeLessThan(drawer!.x);
		await settled(page);
		await expectNoA11yViolations(page);
	});

	test('a narrow overlay at 800', async ({ page, go }) => {
		await page.setViewportSize({ width: 800, height: 800 });
		await go('/dev/detail');
		await expect(page.locator('.ls-detail-host')).toHaveAttribute('data-mode', 'narrow');
		await open(page);
		expect(Math.round((await page.getByTestId('drawer').boundingBox())!.width)).toBe(480);
	});

	test('a full-screen view with Back at 390', async ({ page, go }) => {
		await page.setViewportSize({ width: 390, height: 844 });
		await go('/dev/detail');
		await expect(page.locator('.ls-detail-host')).toHaveAttribute('data-mode', 'full');
		await open(page);
		const drawer = await page.getByTestId('drawer').boundingBox();
		expect(Math.round(drawer!.width)).toBe(390);
		await settled(page);
		await expectNoA11yViolations(page);
		await page.getByRole('button', { name: 'Back to results' }).click();
		await expect(page.getByTestId('drawer')).toHaveCount(0);
		await expect(page.getByTestId('open-a')).toBeFocused();
	});
});
