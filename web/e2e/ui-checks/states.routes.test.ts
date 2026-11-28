// UI check (review 03 "Not done": index rebuild, paused crawler): the status pill in the real
// shell reads "Paused" when the server says so, and "Index rebuild failed" when a rebuild fails,
// with the torrent count the paused state shows and none for the failed one. The rebuilding and idle states are in shell.routes.test.ts
// and the Search banner in search-recover-ui.routes.test.ts; this fills the other two, on a
// desktop and a phone width. Waits are conditions with a 10 s deadline; no fixed sleeps.
import type { Page } from '@playwright/test';
import { crawlerEvent, liveEvents } from '../support/events-server';
import { expect as baseExpect, test } from '../support/test';

const expect = baseExpect.configure({ timeout: 10_000 });

/**
 * What a person sees: the pill is visible and inside the viewport, and its accessible name says
 * what it shows. On a phone only the icon is visible (the words are screen-reader text).
 */
async function expectPillFits(page: Page, width: number) {
	const pill = page.getByTestId('status-pill');
	await expect(pill).toBeVisible();
	await expect(pill).toHaveAccessibleName(/Crawler status\. Open Insights\./);
	const box = (await pill.boundingBox())!;
	expect(box.x).toBeGreaterThanOrEqual(0);
	expect(box.x + box.width).toBeLessThanOrEqual(width);
}

for (const [name, viewport] of [
	['desktop', { width: 1280, height: 800 }],
	['phone', { width: 390, height: 800 }]
] as const) {
	test.describe(`the status pill, ${name}`, () => {
		test.use({ viewport });

		test('a paused crawler reads Paused, with its torrent count', async ({ page, go }) => {
			const events = await liveEvents(page);
			await go('/search');
			const pill = page.getByTestId('status-pill');
			await expect(pill).toHaveAttribute('data-state', 'crawling');
			events.send('crawler', crawlerEvent({ paused: true }));
			await expect(pill).toHaveAttribute('data-state', 'paused');
			await expect(page.getByTestId('status-pill-text')).toHaveText('Paused');
			await expect(page.getByTestId('status-pill-count')).toHaveText('· 1.2M searchable');
			await expectPillFits(page, viewport.width);
			events.send('crawler', crawlerEvent({ paused: false }));
			await expect(pill).toHaveAttribute('data-state', 'crawling');
		});

		test('a failed rebuild says so', async ({ page, go }) => {
			const events = await liveEvents(page);
			await go('/search');
			const pill = page.getByTestId('status-pill');
			await expect(pill).toHaveAttribute('data-state', 'crawling');
			events.send('rebuild', {
				done: 1500,
				error: 'disk full',
				from: 'v3',
				phase: 'failed',
				to: 'v4',
				total: 20000,
				unavailable: []
			});
			await expect(pill).toHaveAttribute('data-state', 'failed');
			await expect(page.getByTestId('status-pill-text')).toHaveText('Index rebuild failed');
			await expect(page.getByTestId('status-pill-count')).toHaveCount(0);
			await expectPillFits(page, viewport.width);
		});
	});
}
