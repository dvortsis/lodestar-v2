// A saved language and time zone apply on a fresh load, with no visit to Language & region.
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
