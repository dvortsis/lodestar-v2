// The live banner (S-search-6, S-search-live-ui; §10.4 "Live"): the query shown is registered with
// `POST /search/live`, and "N new results · Show" comes from `search_live` events on /events. The
// list never reorders by itself, and the announcement is throttled. Registration failing (429)
// falls back to the old 30 s probe, with the clock installed so it runs without waiting. Every
// wait is a condition.
import type { Request } from '@playwright/test';
import { expectNoA11yViolations } from './a11y';
import { body, expect, hits, row, rowOf, server } from './search-results.fixtures';
import { liveEvents } from './support/events-server';
import { test } from './support/test';

type Mock = Parameters<typeof server>[0];

/** Search answers `total` hits; `/search/live` hands out `a1`, `a2`, ... or fails when `fail`. */
function live(mock: Mock, state: { total: number; approx: boolean; fail?: boolean }) {
	server(mock, () => body(hits(10), { total: 10 }));
	mock.on('searchPost', (async (request: Request) => {
		const sent = request.postDataJSON() as { limit: number };
		if (sent.limit === 1) return body([], { total: state.total, total_approx: state.approx });
		return body(hits(state.total), { total: state.total });
	}) as never);
	let n = 0;
	mock.on('registerSearchLive', (() =>
		state.fail
			? { status: 429, body: { error: { code: 'too_many_live_queries', message: 'too many' } } }
			: { status: 200, body: { id: `a${++n}`, since: 100 } }) as never);
}

test('new matches show a banner; the list holds until Show reloads it and registers again', async ({
	page,
	go,
	mock
}) => {
	const events = await liveEvents(page);
	live(mock, { total: 10, approx: false });
	await go('/search?q=lantern');
	await expect(row(page, 1)).toBeVisible();
	await expect.poll(() => mock.calls('registerSearchLive').length).toBe(1);
	const banner = page.getByTestId('results-live');
	// An event for some other registration counts for nothing.
	await events.send('search_live', { id: 'zzz', matched: 9, through: 5 });
	await events.send('search_live', { id: 'a1', matched: 4, through: 105 });
	await expect(banner).toHaveText('4 new results');
	// Nothing reordered under the user, and the announcement reached the polite region.
	await expect(row(page, 1)).toBeVisible();
	await expect(page.getByTestId('results-announce')).toContainText('4 new results');
	await expectNoA11yViolations(page);
	const calls = mock.calls('searchPost').length;
	await page.getByTestId('results-live-show').focus();
	await page.keyboard.press('Enter');
	await expect(banner).toHaveCount(0);
	await expect.poll(() => mock.calls('searchPost').length).toBeGreaterThan(calls);
	// Show registers again (the count starts from 0) and releases the old id.
	await expect.poll(() => mock.calls('registerSearchLive').length).toBe(2);
	await expect.poll(() => mock.calls('releaseSearchLive').length).toBeGreaterThan(0);
	// The list owns its active row after Show: focus is on the first row, and j goes to the second.
	await expect(rowOf(page, 1)).toBeFocused();
	await page.keyboard.press('j');
	await expect(rowOf(page, 2)).toBeFocused();
	// The old id is dead: its late event does nothing; the new one counts.
	await events.send('search_live', { id: 'a1', matched: 7, through: 120 });
	await events.send('search_live', { id: 'a2', matched: 1, through: 121 });
	await expect(banner).toHaveText('1 new result');
});

test('a full registry (429) falls back to the 30 s probe', async ({ page, go, mock }) => {
	const state = { total: 10, approx: false, fail: true };
	live(mock, state);
	await page.clock.install();
	await go('/search?q=lantern');
	await expect(row(page, 1)).toBeVisible();
	state.total = 14;
	const banner = page.getByTestId('results-live');
	await expect(async () => {
		await page.clock.runFor(30_000);
		await expect(banner).toHaveText('4 new results', { timeout: 1_000 });
	}).toPass({ timeout: 10_000 });
});
