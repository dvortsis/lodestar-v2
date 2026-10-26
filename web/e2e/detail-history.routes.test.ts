import { body, expect, hits, row, rowOf, server } from './search-results.fixtures';
import { test } from './support/test';

for (const width of [390, 1366]) {
	test(`detail Back/Forward and explicit close restore the invoker at ${width}`, async ({
		page,
		go,
		mock
	}) => {
		await page.setViewportSize({ width, height: 844 });
		server(mock, () => body(hits(3)));
		await go('/search?q=lantern');
		await row(page, 1).click();
		await expect(page.getByTestId('drawer')).toBeVisible();
		await page.goBack();
		await expect(page.getByTestId('drawer')).toHaveCount(0);
		await expect(rowOf(page, 1)).toBeFocused();
		await page.goForward();
		await expect(page.getByTestId('drawer')).toBeVisible();
		await page.getByTestId('drawer').getByRole('button', { name: 'Close details' }).click();
		await expect(page.getByTestId('drawer')).toHaveCount(0);
		await expect(rowOf(page, 1)).toBeFocused();
	});
}

test('Back falls back to Search if the invoking row is gone', async ({ page, go, mock }) => {
	server(mock, () => body(hits(3)));
	await go('/search?q=lantern');
	await row(page, 1).click();
	await expect(page.getByTestId('drawer')).toBeVisible();
	// Model a virtualized or removed result; do not replace it with another focus target.
	await rowOf(page, 1).evaluate((el) => el.remove());
	await page.goBack();
	await expect(page.getByTestId('drawer')).toHaveCount(0);
	await expect(page.getByTestId('query-editor')).toBeFocused();
});

test('Back preserves focus chosen outside the drawer', async ({ page, go, mock }) => {
	server(mock, () => body(hits(3)));
	await go('/search?q=lantern');
	await row(page, 1).click();
	await expect(page.getByTestId('drawer')).toBeVisible();
	await page.getByTestId('query-editor').focus();
	await page.goBack();
	await expect(page.getByTestId('drawer')).toHaveCount(0);
	await expect(page.getByTestId('query-editor')).toBeFocused();
});

test('leaving Search opens full-page detail and does not restore the old invoker', async ({
	page,
	go,
	mock
}) => {
	server(mock, () => body(hits(3)));
	await go('/search?q=lantern');
	await row(page, 1).click();
	await page.getByTestId('detail-open-full').click();
	await expect(page.getByTestId('detail-view')).toHaveAttribute('data-mode', 'page');
	await expect(page.getByTestId('query-editor')).toHaveCount(0);
	await page.reload();
	await expect(page.getByTestId('detail-view')).toHaveAttribute('data-mode', 'page');
});
