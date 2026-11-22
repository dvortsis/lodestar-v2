// X-detail-depth: the Swarm section, Technical details, the actions menu, Open in torrent client and
// the back link, on /t/{hash} at phone, tablet and desktop widths. Axe on each.
import { expectNoA11yViolations } from './a11y';
import { HASH, detail, hit, mockTorrent } from './detail.fixtures';
import { expect, test } from './support/test';

const DAY = 86_400;
const T0 = 1_700_000_000;
const swarm = detail({
	dht_peers: 90,
	dht_seeds: 40,
	swarm_history: {
		daily: [
			{ at: T0, seeders: 5, leechers: 1 },
			{ at: T0 + DAY, seeders: 6, leechers: 2 },
			{ at: T0 + 2 * DAY, seeders: null, leechers: null },
			{ at: T0 + 3 * DAY, seeders: 7, leechers: 2 }
		],
		weekly: []
	},
	swarm_trackers: [
		{
			url: 'https://tracker.example.org/PASSKEY123456/announce?pk=SECRETQUERY',
			at: T0 + 3 * DAY,
			seeders: 7,
			leechers: 2,
			completed: 30
		}
	]
});

for (const width of [390, 1024, 1440]) {
	test(`Swarm, Technical details and the menu at ${width}, axe-clean`, async ({
		page,
		go,
		mock
	}) => {
		await page.setViewportSize({ width, height: 900 });
		mockTorrent(mock, swarm, hit());
		await go(`/t/${HASH}`);
		await expect(page.getByTestId('detail-swarm-chart')).toBeVisible();
		await expect(page.getByTestId('detail-swarm-max')).toHaveText('7');
		await expect(page.getByTestId('detail-swarm-host')).toHaveText('tracker.example.org');
		await expect(page.locator('body')).not.toContainText('PASSKEY123456');
		await expect(page.locator('body')).not.toContainText('SECRETQUERY');
		await expect(page.getByTestId('detail-swarm-dht')).toContainText('estimate');
		// Technical details: closed, then open
		const tech = page.getByTestId('detail-technical');
		await expect(tech).not.toHaveAttribute('open', '');
		await tech.getByText('Technical details').click();
		await expect(page.getByTestId('detail-piece-size')).toHaveText('4.00 MiB');
		await expect(page.getByTestId('detail-hash')).toHaveText(HASH);
		// Open in torrent client beside Copy magnet
		await expect(page.getByTestId('detail-open-magnet')).toHaveText('Open in torrent client');
		await expect(page.getByTestId('detail-copy-magnet')).toBeVisible();
		// The file filter has a visible label
		await expect(page.getByLabel('Filter files')).toBeVisible();
		await expectNoA11yViolations(page);
	});
}

test('the actions menu opens from the keyboard and holds Block this…', async ({
	page,
	go,
	mock,
	kbd
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	mockTorrent(mock, swarm, hit());
	await go(`/t/${HASH}`);
	const trigger = page.getByTestId('detail-actions-menu');
	await expect(trigger).toBeVisible();
	await kbd.tabTo(trigger);
	await kbd.press('Enter');
	const item = page.getByRole('menuitem', { name: 'Block this…' });
	await expect(item).toBeFocused();
	// Finite animations only: the menu's fade would otherwise be measured mid-transition.
	await page.evaluate(() =>
		Promise.all(
			document
				.getAnimations()
				.filter((a) => a.effect?.getComputedTiming().iterations !== Infinity)
				.map((a) => a.finished)
		)
	);
	await expectNoA11yViolations(page);
	await kbd.press('Escape');
	await expect(item).toHaveCount(0);
	await expect(trigger).toBeFocused();
});

test('at 390 the actions sit under the title, in the DOM and on screen', async ({
	page,
	go,
	mock
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	mockTorrent(mock, swarm, hit());
	await go(`/t/${HASH}`);
	await expect(page.getByTestId('detail-copy-magnet')).toBeVisible();
	await expect
		.poll(
			async () => {
				const title = await page.getByRole('heading', { level: 1 }).boundingBox();
				const copy = await page.getByTestId('detail-copy-magnet').boundingBox();
				return copy!.y - (title!.y + title!.height);
			},
			{ timeout: 10_000 }
		)
		.toBeGreaterThanOrEqual(-1);
	const order = await page.evaluate(() => {
		const h1 = document.querySelector('h1')!;
		const copy = document.querySelector('[data-testid="detail-copy-magnet"]')!;
		return h1.compareDocumentPosition(copy) & Node.DOCUMENT_POSITION_FOLLOWING;
	});
	expect(order).toBeTruthy();
});

test('Back to Search links to Search on a direct load', async ({ page, go, mock }) => {
	mockTorrent(mock, swarm, hit());
	await go(`/t/${HASH}`);
	await expect(page.getByTestId('detail-back-search')).toHaveAttribute('href', '/search');
});
