// A saved language and time zone apply on a fresh load, with no visit to Language & region.
import { body, hashOf, hits, server } from './search-results.fixtures';
import { liveEvents } from './support/events-server';
import { expect as baseExpect, test } from './support/test';

const expect = baseExpect.configure({ timeout: 10_000 });
const field = (value: unknown) => ({
	value,
	env: null,
	read_only: false,
	restart_required: false,
	source: 'db' as const
});

test('a saved zh language shows zh after a reload', async ({ page, go, mock }) => {
	await liveEvents(page);
	mock.on('getSettings', {
		section: 'preferences',
		revision: 1,
		restart_required: [],
		fields: {
			language: field('zh'),
			time_zone: field('Asia/Tokyo'),
			first_day_of_week: field('sunday')
		}
	});
	await go('/search');
	await expect(page.locator('html')).toHaveAttribute('lang', 'zh');
	await expect
		.poll(() => page.evaluate(() => JSON.parse(localStorage.getItem('lodestar.region') ?? '{}')))
		.toMatchObject({ language: 'zh', timeZone: 'Asia/Tokyo', firstDay: 'sunday' });
	await page.reload();
	await expect(page.locator('html')).toHaveAttribute('lang', 'zh');
});

// 2026-10-22 23:30 UTC is already the 23rd in Tokyo and still the 22nd in Los Angeles.
const AT = Date.UTC(2026, 9, 22, 23, 30) / 1000;
for (const [zone, day] of [
	['Asia/Tokyo', 'Oct 23, 2026, 08:30'],
	['America/Los_Angeles', 'Oct 22, 2026, 16:30']
] as const) {
	test(`a saved time zone (${zone}) sets a visible timestamp`, async ({ page, go, mock }) => {
		await liveEvents(page);
		mock.on('getSettings', {
			section: 'preferences',
			revision: 1,
			restart_required: [],
			fields: { language: field(''), time_zone: field(zone), first_day_of_week: field('auto') }
		});
		server(mock, () =>
			body(
				hits(1).map((h) => ({ ...h, discovered_at: AT })),
				{ total: 1 }
			)
		);
		await go('/search?q=lantern');
		const when = page.getByTestId(`result-${hashOf(1)}-discovered`).locator('time');
		await expect(when).toHaveAttribute('title', day);
	});
}
