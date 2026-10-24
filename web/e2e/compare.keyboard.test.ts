// S-compare, keyboard only: Tab through a column's actions, Remove by keyboard, and where
// focus lands afterwards (§10.13).
import { HASHES, response } from '../src/lib/features/compare/testing';
import { expect, test } from './support/test';

const COLUMNS = { width: 1366, height: 900 };

test.beforeEach(async ({ page, mock }) => {
	await page.setViewportSize(COLUMNS);
	mock.on('getTorrent', { status: 200, body: { magnet: 'magnet:?xt=urn:btih:a' } as never });
	mock.on('compareTorrents', (request) => {
		const { hashes } = request.postDataJSON() as { hashes: string[] };
		const body = response(hashes.length);
		body.torrents.forEach((t, i) => (t.hash = hashes[i]!));
		return { status: 200, body };
	});
});

test('a column’s actions are reachable in order: Open, Copy magnet, Remove', async ({
	page,
	go,
	kbd
}) => {
	await go(`/compare?h=${HASHES[0]},${HASHES[1]},${HASHES[2]}`);
	await expect(page.getByTestId('compare-table')).toBeVisible();
	const first = page.locator(`[data-hash="${HASHES[0]}"]`);
	await kbd.tabTo(first.getByRole('link', { name: /^Open / }));
	await expect(first.getByRole('link', { name: /^Open / })).toBeFocused();
	await kbd.press('Tab');
	await expect(first.getByRole('button', { name: /^Copy magnet for/ })).toBeFocused();
	await kbd.press('Tab');
	await expect(first.getByRole('button', { name: /^Remove .* from comparison$/ })).toBeFocused();
	expect(await kbd.pointerEvents()).toEqual([]);
});

test('Remove by keyboard moves focus to the next column’s first action', async ({
	page,
	go,
	kbd
}) => {
	await go(`/compare?h=${HASHES[0]},${HASHES[1]},${HASHES[2]}`);
	await expect(page.getByTestId('compare-table')).toBeVisible();
	const second = page.locator(`[data-hash="${HASHES[1]}"]`);
	const remove = second.getByRole('button', { name: /^Remove .* from comparison$/ });
	await kbd.tabTo(remove);
	await kbd.press('Enter');
	await expect(page).toHaveURL(new RegExp(`h=${HASHES[0]},${HASHES[2]}(&|$)`));
	await expect(
		page.locator(`[data-hash="${HASHES[2]}"]`).getByRole('link', { name: /^Open / })
	).toBeFocused();
});

test('removing the last column of two lands on the page heading', async ({ page, go, kbd }) => {
	await go(`/compare?h=${HASHES[0]},${HASHES[1]}`);
	await expect(page.getByTestId('compare-table')).toBeVisible();
	const remove = page
		.locator(`[data-hash="${HASHES[0]}"]`)
		.getByRole('button', { name: /^Remove .* from comparison$/ });
	await kbd.tabTo(remove);
	await kbd.press('Space');
	await expect(page.getByRole('heading', { level: 1, name: 'Compare' })).toBeFocused();
});
