// Insights overview with no pointer: the bar, range arrows, Compare, a strip link, a table and a
// drill-through, all from the keyboard.
import { crawlerFixture, overviewFixture } from '../src/lib/features/insights/overview/fixtures';
import { expect, test } from './support/test';

test('keyboard-only: bar, range, compare, strip, table and drill-through', async ({
	page,
	mock,
	go,
	kbd
}) => {
	mock.on('getInsightsOverview', overviewFixture());
	mock.on('getCrawler', crawlerFixture());
	await go('/insights');
	await expect(page.getByTestId('insights-kpis')).toBeVisible();

	// Range: one tab stop on the checked radio; arrows move and select.
	await kbd.tabTo(page.getByRole('radio', { name: '7 d' }));
	await kbd.press('ArrowRight');
	await expect(page).toHaveURL(/range=30d/);
	await expect(page.getByRole('radio', { name: '30 d' })).toBeFocused();

	// Compare toggle.
	await kbd.tabTo(page.getByTestId('insights-compare'));
	await kbd.press('Space');
	await expect(page).toHaveURL(/compare=1/);

	// A strip link moves focus to its level's heading.
	await kbd.tabTo(page.getByTestId('insights-stage-fetch'));
	await kbd.press('Enter');
	await expect(page.locator('#insights-funnel-heading')).toBeFocused();

	// View as table.
	const toggle = page.getByTestId('insights-funnel-panel-toggle');
	await kbd.tabTo(toggle);
	await kbd.press('Enter');
	await expect(toggle).toHaveAttribute('aria-expanded', 'true');

	// A drill-through is a real link.
	const link = page.getByTestId('insights-funnel-stored').locator('a');
	await kbd.tabTo(link);
	await kbd.press('Enter');
	await expect(page).toHaveURL(/\/insights\/understand\?range=30d$/);
	expect(await kbd.pointerEvents()).toEqual([]);
});
