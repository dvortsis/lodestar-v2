// The Overview's privacy banner (S-privacy-banner, §10.10.12): shown from the overview's `privacy`
// summary, reachable by keyboard, and absent (with none of its code loaded) while nothing is on.
import { readFileSync } from 'node:fs';
import { expectNoA11yViolations } from './a11y';
import {
	NOW,
	crawlerFixture,
	overviewFixture
} from '../src/lib/features/insights/overview/fixtures';
import { expect, test } from './support/test';

type Mock = Parameters<Parameters<typeof test>[2]>[0]['mock'];

function serve(
	mock: Mock,
	privacy: { identifying_until: number | null; query_logging: boolean } | null
) {
	mock.on('getInsightsOverview', overviewFixture({ privacy, updated_at: NOW }));
	mock.on('getCrawler', crawlerFixture());
}

test('names what is recorded, with Turn off now and the Insights link, by keyboard', async ({
	page,
	mock,
	go
}) => {
	serve(mock, { identifying_until: NOW + 52 * 60, query_logging: true });
	await go('/insights');
	const banners = page.getByTestId('privacy-banners');
	await expect(banners.getByText('Log lines may identify torrents or people')).toBeVisible();
	await expect(page.getByTestId('privacy-identifying')).toContainText('about 52 min more');
	await expect(banners.getByText('Search queries are being saved')).toBeVisible();
	await expect(banners.getByRole('link', { name: /Settings → Insights/ })).toHaveAttribute(
		'href',
		'/settings/insights'
	);
	await expect(banners.getByRole('status')).toHaveCount(2);
	// Reached with Tab alone, no dismiss control.
	const button = banners.getByRole('button', { name: 'Turn off now' });
	for (let i = 0; i < 40; i++) {
		if (await button.evaluate((el) => el === document.activeElement)) break;
		await page.keyboard.press('Tab');
	}
	await expect(button).toBeFocused();
	await expect(banners.getByRole('button')).toHaveCount(1);
	await expectNoA11yViolations(page);
});

test('shows nothing and loads no banner code while nothing is on', async ({ page, mock, go }) => {
	// The banner's chunk, by its hashed name in the build the server serves.
	const manifest = JSON.parse(
		readFileSync('.svelte-kit/output/client/.vite/manifest.json', 'utf8')
	) as Record<string, { file: string }>;
	const chunk = manifest['src/lib/features/shell/privacy/PrivacyBanner.svelte'].file;
	const scripts: string[] = [];
	page.on('request', (r) => r.resourceType() === 'script' && scripts.push(r.url()));
	serve(mock, null);
	await go('/insights');
	await expect(page.getByTestId('insights-kpi-searchable-value')).toHaveText('1.2M');
	await expect(page.getByTestId('privacy-banners')).toHaveCount(0);
	expect(scripts.length).toBeGreaterThan(0);
	expect(scripts.filter((u) => u.endsWith(chunk))).toEqual([]);
});
