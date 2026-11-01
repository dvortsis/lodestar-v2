// Documentation screenshots (slice S-doc-shots; .claude/skills/user-docs/SKILL.md §4).
// Run with `npm run docs:shots`: the opt-in `docs` project, over the mock fixture data, Riverstone,
// 1366 wide, the clock frozen. Each shot lands in `docs/user/images/<page>-<step>.png`, optimised by
// `./capture.ts`. Excluded from the default run and from CI.
import type { Page } from '@playwright/test';
import type { components } from '../../src/lib/api/schema';
import { discoverResponse, showAdultSettings } from '../../src/lib/features/discover/testing';
import { crawlerFixture, overviewFixture } from '../../src/lib/features/insights/overview/fixtures';
import { body, expect, hits, server } from '../search-results.fixtures';
import { test } from '../support/test';
import { capture, freeze } from './capture';

type Facets = components['schemas']['FacetsResponse'];
type Term = components['schemas']['TermFacet'];
const GiB = 2 ** 30;
const DAY = 86_400;
// Matches the frozen clock in `freeze` (2026-01-15 12:00 UTC).
const NOW = 1_768_478_400;

const term = (values: [string, number][], selected?: string): Term => ({
	approx: false,
	distinct: values.length,
	other: 0,
	values: values.map(([value, count]) => ({
		value,
		count,
		selected: value === selected,
		excluded: false
	}))
});

const facets = (): Facets => ({
	backfill_before: NOW - 20 * DAY,
	buckets: { seeders: null, trackers: null },
	canonical: '',
	facets: {
		type: term([
			['movie', 812],
			['tv', 301]
		]),
		res: term(
			[
				['1080p', 640],
				['2160p', 210],
				['480p', 33]
			],
			'1080p'
		),
		ext: term([
			['mkv', 700],
			['mp4', 220]
		]),
		lang: term([['en', 900]])
	},
	histograms: {
		size: {
			approx: false,
			missing: 0,
			unit: 'bytes',
			bins: [
				{ lo: 0, hi: GiB, count: 300, backfill: false },
				{ lo: GiB, hi: 4 * GiB, count: 700, backfill: false },
				{ lo: 4 * GiB, hi: 16 * GiB, count: 120, backfill: false },
				{ lo: 16 * GiB, hi: null, count: 84, backfill: false }
			]
		},
		discovered: {
			approx: false,
			missing: 0,
			unit: 'week',
			bins: [
				{ lo: NOW - 28 * DAY, hi: NOW - 21 * DAY, count: 500, backfill: true },
				{ lo: NOW - 21 * DAY, hi: NOW - 14 * DAY, count: 400, backfill: false },
				{ lo: NOW - 14 * DAY, hi: NOW - 7 * DAY, count: 200, backfill: false },
				{ lo: NOW - 7 * DAY, hi: NOW, count: 184, backfill: false }
			]
		}
	},
	sample_rate: null,
	sampled: false,
	took_ms: 1,
	total: 640,
	total_approx: false,
	unavailable: ['seeders', 'trackers']
});

function serveSearch(mock: Parameters<typeof server>[0]) {
	server(mock, () => body(hits(8), { total: 640 }));
	mock.on('facetsPost', () => facets());
}

const ready = async (page: Page) => {
	await page.evaluate(() => document.fonts.ready);
};

test.use({ viewport: { width: 1366, height: 900 } });

// The guide's light/dark `<picture>` pairs need both modes; the single-theme shots are light only.
for (const mode of ['light', 'dark'] as const) {
	test.describe(mode, () => {
		test.use({ themeId: `riverstone-${mode}`, colorMode: mode });
		test.beforeEach(({ page }) => freeze(page));

		test(`search-${mode}`, async ({ page, go, mock }) => {
			serveSearch(mock);
			await go('/search?q=lantern+res%3A1080p');
			await expect(page.getByTestId('facet-rail')).toBeVisible();
			await expect(page.getByTestId('facet-size-histogram')).toBeVisible();
			await ready(page);
			await capture(page, `search-${mode}`);
		});

		test(`detail-${mode}`, async ({ page, go, mock }) => {
			serveSearch(mock);
			await go('/search?q=lantern+res%3A1080p');
			await page.getByTestId('result-' + 'a'.repeat(39) + '1').click();
			await expect(page.getByTestId('drawer')).toBeVisible();
			await ready(page);
			await capture(page, `detail-${mode}`);
		});

		test(`discover-${mode}`, async ({ page, go, mock }) => {
			mock.on('getDiscover', discoverResponse());
			mock.on('getSettings', showAdultSettings(false));
			await go('/discover');
			await expect(page.getByRole('heading', { name: 'Discover' })).toBeVisible();
			await ready(page);
			await capture(page, `discover-${mode}`);
		});

		test(`insights-${mode}`, async ({ page, go, mock }) => {
			mock.on('getInsightsOverview', overviewFixture());
			mock.on('getCrawler', crawlerFixture());
			await go('/insights');
			await expect(page.getByRole('heading', { name: 'Insights' })).toBeVisible();
			await ready(page);
			await capture(page, `insights-${mode}`, undefined, { fullPage: false });
		});
	});
}

test.describe('setup', () => {
	test.beforeEach(({ page }) => freeze(page));

	test('setup-screen', async ({ page, go, mock }) => {
		mock.on('getAuthStatus', { logged_in: false, setup_required: true });
		await go('/setup');
		await expect(page.getByRole('heading', { name: 'Set up Lodestar' })).toBeVisible();
		await ready(page);
		await capture(page, 'setup-screen', page.getByLabel(/setup token/i).first());
	});
});
