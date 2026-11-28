// X-row-rail (UI review U10, U11, I6, U15, 02 §4c; architecture §10.4): the two-tier result row at
// four widths, and a click on a badge that filters. Name ends in `routes.test.ts` so the `core`
// project runs it. Waits are conditions with the shared 10 s deadline, never sleeps.
import type { Page } from '@playwright/test';
import { body, expect, hashOf, hits, row, server } from './search-results.fixtures';
import { test } from './support/test';

const withFacts = (n: number) =>
	hits(n).map((h) => ({
		...h,
		content_type: 'movie',
		resolution: '1080p' as const,
		flags: ['private'],
		seeders: 512,
		leechers: 48,
		trackers_reporting: 50,
		trackers_checked: 56
	}));

const q = (page: Page) => new URL(page.url()).searchParams.get('q');

for (const width of [390, 1024, 1440, 2560]) {
	test(`the two-tier row at ${width} px: choice facts only, in words`, async ({
		page,
		go,
		mock
	}) => {
		await page.setViewportSize({ width, height: 900 });
		server(mock, () => body(withFacts(4)));
		await go('/search?q=lantern');
		await expect(row(page, 4)).toBeVisible();
		const part = (p: string) => page.getByTestId(`result-${hashOf(1)}-${p}`);
		// Availability is one fact in words and number; size and discovered stay at every width.
		await expect(part('seeders')).toHaveText('512 seeders');
		await expect(part('size')).toBeVisible();
		await expect(part('discovered')).toBeVisible();
		// The technical facts are the drawer's, not the row's.
		for (const gone of ['leechers', 'trackers', 'backfill'])
			await expect(part(gone)).toHaveCount(0);
		if (width >= 1024) await expect(part('badges')).toBeVisible();
		// Nothing overflows sideways.
		const overflow = await page.evaluate(
			() => document.documentElement.scrollWidth - document.documentElement.clientWidth
		);
		expect(overflow).toBeLessThanOrEqual(0);
	});
}

test('clicking a badge filters by it and does not open the row', async ({ page, go, mock }) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	server(mock, () => body(withFacts(3)));
	await go('/search?q=lantern');
	await expect(row(page, 1)).toBeVisible();
	const badge = page.getByTestId(`result-${hashOf(1)}-badge-res`);
	await expect(badge).toHaveAccessibleName('Filter by 1080p');
	await badge.click();
	await expect.poll(() => q(page)).toBe('lantern res:1080p');
	await expect(page.getByTestId('drawer')).toHaveCount(0);
	// "Filter by 1080p" adds only: a second click keeps the filter, it doesn't widen the results.
	await page.getByTestId(`result-${hashOf(1)}-badge-res`).click();
	await page.waitForLoadState('networkidle');
	expect(q(page)).toBe('lantern res:1080p');
});

test('the rail keeps its decided order, and Seeders offers plain presets', async ({
	page,
	go,
	mock
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	server(mock, () => body(withFacts(3)));
	await go('/search?q=lantern');
	const rail = page.getByTestId('facet-rail');
	await expect(rail).toBeVisible();
	const seeders = page.getByTestId('facet-seeders-5');
	await expect(seeders).toBeVisible();
	await seeders.click();
	await expect.poll(() => q(page)).toBe('lantern seeders:>=5');
});
