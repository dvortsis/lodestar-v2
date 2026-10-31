// The live banner (S-search-6; §10.4 "Live"): "N new results · Show", the list never reordering by
// itself, the throttled polite announcement, and the pauses. The clock is installed so the 30 s
// poll runs without waiting; every wait is a condition.
import type { Request } from '@playwright/test';
import { expectNoA11yViolations } from './a11y';
import { body, expect, hits, row, server } from './search-results.fixtures';
import { test } from './support/test';

function live(mock: Parameters<typeof server>[0], state: { total: number; approx: boolean }) {
	server(mock, () => body(hits(10), { total: 10 }));
	mock.on('searchPost', (async (request: Request) => {
		const sent = request.postDataJSON() as { limit: number };
		if (sent.limit === 1) return body([], { total: state.total, total_approx: state.approx });
		return body(hits(10), { total: state.total });
	}) as never);
}

test('new results show a banner; the list holds until Show reloads it', async ({
	page,
	go,
	mock
}) => {
	const state = { total: 10, approx: false };
	live(mock, state);
	await page.clock.install();
	await go('/search?q=lantern');
	await expect(row(page, 1)).toBeVisible();
	state.total = 14;
	const banner = page.getByTestId('results-live');
	// The poller loads after the first rows: advance the clock until it has run once.
	await expect(async () => {
		await page.clock.runFor(30_000);
		await expect(banner).toHaveText('4 new results', { timeout: 1_000 });
	}).toPass({ timeout: 10_000 });
	// Nothing reordered under the user, and the announcement reached the polite region.
	await expect(row(page, 1)).toBeVisible();
	await expect(page.getByTestId('results-announce')).toContainText('4 new results');
	await expectNoA11yViolations(page);
	const calls = mock.calls('searchPost').length;
	await page.getByTestId('results-live-show').focus();
	await page.keyboard.press('Enter');
	await expect(banner).toHaveCount(0);
	await expect.poll(() => mock.calls('searchPost').length).toBeGreaterThan(calls);
});

test('an approximate total says nothing', async ({ page, go, mock }) => {
	const state = { total: 10, approx: true };
	live(mock, state);
	await page.clock.install();
	await go('/search?q=lantern');
	await expect(row(page, 1)).toBeVisible();
	state.total = 14;
	const before = mock.calls('searchPost').length;
	await expect(async () => {
		await page.clock.runFor(30_000);
		expect(mock.calls('searchPost').length).toBeGreaterThan(before);
	}).toPass({ timeout: 10_000 });
	await expect(page.getByTestId('results-live')).toHaveCount(0);
});
