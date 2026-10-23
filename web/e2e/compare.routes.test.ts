// S-compare: the Compare page (§10.6) against the OpenAPI mock. Columns at 1366, the
// attribute-first list at 390 and 768, Remove with history, every error state, a 2,000-row
// file list, and axe in Riverstone light and dark for the populated, error and narrow views.
// No snapshot baselines (the route is not a key screen).
import type { components } from '../src/lib/api/schema';
import { fileList, HASHES, response } from '../src/lib/features/compare/testing';
import { expectNoA11yViolations } from './a11y';
import type { OpenApiMock } from './mock/openapi-mock';
import { expect, test } from './support/test';

type CompareResponse = components['schemas']['CompareResponse'];
const url = (n: number) => `/compare?h=${HASHES.slice(0, n).join(',')}`;
const COLUMNS = { width: 1366, height: 900 };

/** Answers for whatever hashes were asked, so Remove gets a comparison of the rest. */
function serve(mock: OpenApiMock, patch?: (r: CompareResponse) => void) {
	// Hover and focus on a column's actions prefetch its magnet.
	mock.on('getTorrent', { status: 200, body: { magnet: 'magnet:?xt=urn:btih:a' } as never });
	mock.on('compareTorrents', (request) => {
		const { hashes } = request.postDataJSON() as { hashes: string[] };
		const body = response(hashes.length);
		body.torrents.forEach((t, i) => (t.hash = hashes[i]!));
		patch?.(body);
		return { status: 200, body };
	});
}

for (const mode of ['light', 'dark'] as const) {
	test.describe(`Riverstone ${mode}`, () => {
		test.use({ themeId: `riverstone-${mode}`, colorMode: mode });

		test('two columns at 1366: a table, differences marked, axe clean', async ({
			page,
			go,
			mock
		}) => {
			serve(mock);
			await page.setViewportSize(COLUMNS);
			await go(url(2));
			await expect(page.getByRole('heading', { level: 1, name: 'Compare' })).toBeVisible();
			const table = page.getByTestId('compare-table');
			await expect(table).toBeVisible();
			await expect(table.getByRole('columnheader')).toHaveCount(3);
			await expect(page.getByTestId('compare-differs')).toHaveCount(2);
			await expect(page.getByTestId('compare-row-attr-res')).toContainText('differs');
			await expect(page.getByTestId('compare-row-size')).not.toContainText('differs');
			await expect(page.getByTestId('compare-composition-1-bar')).toHaveAttribute(
				'aria-label',
				'80% Video, 20% Audio'
			);
			await expectNoA11yViolations(page);
		});

		test('five columns at 1366 stay a table', async ({ page, go, mock }) => {
			serve(mock);
			await page.setViewportSize(COLUMNS);
			await go(url(5));
			await expect(page.getByTestId('compare-table').getByRole('columnheader')).toHaveCount(6);
			await expectNoA11yViolations(page);
		});

		for (const [width, n] of [
			[390, 2],
			[390, 5],
			[768, 3],
			[768, 5]
		] as const) {
			test(`at ${width} with ${n} torrents the table becomes an attribute-first list`, async ({
				page,
				go,
				mock
			}) => {
				serve(mock);
				await page.setViewportSize({ width, height: 900 });
				await go(url(n));
				await expect(page.getByTestId('compare-list')).toBeVisible();
				await expect(page.getByTestId('compare-table')).toHaveCount(0);
				await expect(
					page.getByTestId('compare-row-attr-res').getByRole('heading', { level: 2 })
				).toContainText('differs');
				await expect(page.getByTestId('compare-row-attr-year')).not.toContainText('differs');
				const overflow = await page.evaluate(
					() => document.documentElement.scrollWidth > document.documentElement.clientWidth
				);
				expect(overflow, 'no horizontal page scroll').toBe(false);
				await expectNoA11yViolations(page);
			});
		}

		test('error states are readable and axe clean', async ({ page, go, mock }) => {
			await page.setViewportSize(COLUMNS);
			mock.error('compareTorrents', 400, 'duplicate_hash', 'x', { field: 'hashes' });
			await go(url(2));
			const panel = page.getByTestId('compare-error-duplicate_hash');
			await expect(panel).toContainText('more than once');
			await expect(panel.getByRole('button', { name: /^Remove / })).toHaveCount(2);
			await expectNoA11yViolations(page);
		});
	});
}

test('Remove asks again with the rest, pushes history, and Back restores it', async ({
	page,
	go,
	mock
}) => {
	serve(mock);
	await page.setViewportSize(COLUMNS);
	await go(`${url(3)}`);
	await expect(page.getByTestId('compare-table')).toBeVisible();
	await page
		.getByRole('button', { name: /^Remove .* from comparison$/ })
		.nth(1)
		.click();
	await expect(page).toHaveURL(new RegExp(`h=${HASHES[0]},${HASHES[2]}(&|$)`));
	await expect(page.getByTestId('compare-table').getByRole('columnheader')).toHaveCount(3);
	const asked = mock
		.calls('compareTorrents')
		.map((r) => (r.postDataJSON() as { hashes: string[] }).hashes);
	expect(asked.at(-1)).toEqual([HASHES[0], HASHES[2]]);
	await page.goBack();
	// `go()` wrote the commas percent-encoded; the page writes them plain. Both mean the same list.
	await expect
		.poll(() => decodeURIComponent(page.url()))
		.toContain(`h=${HASHES[0]},${HASHES[1]},${HASHES[2]}`);
	await expect(page.getByTestId('compare-table').getByRole('columnheader')).toHaveCount(4);
});

test.describe('error codes', () => {
	test.beforeEach(async ({ page }) => page.setViewportSize(COLUMNS));
	const cases: [string, number, string][] = [
		['compare_count', 400, 'Compare needs 2 to 5 torrents'],
		['bad_hash', 400, "One of these torrents isn't valid"]
	];
	for (const [code, status, text] of cases) {
		test(code, async ({ page, go, mock }) => {
			mock.error('compareTorrents', status, code, 'x', { field: 'hashes' });
			await go(url(2));
			await expect(page.getByTestId(`compare-error-${code}`)).toContainText(text);
		});
	}

	test('torrent_not_found names nothing and offers Remove and Back to Search', async ({
		page,
		go,
		mock
	}) => {
		mock.error('compareTorrents', 404, 'torrent_not_found', 'x');
		await go(url(3));
		const panel = page.getByTestId('compare-error-not-found');
		await expect(panel).toContainText("One of these torrents isn't available");
		await expect(panel.getByRole('button', { name: /^Remove / })).toHaveCount(3);
		await expect(panel.getByRole('link', { name: 'Back to Search' })).toHaveAttribute(
			'href',
			'/search'
		);
	});

	test('a network failure is a banner with Retry', async ({ page, go }) => {
		// Registered after the mock's own route, so it answers first.
		await page.route('**/api/v1/compare', (route) => route.abort());
		await go(url(2));
		await expect(page.getByRole('alert')).toBeVisible();
	});

	test('a real duplicate in the URL is listed and Remove drops one copy', async ({
		page,
		go,
		mock
	}) => {
		mock.error('compareTorrents', 400, 'duplicate_hash', 'x', { field: 'hashes' });
		await go(`/compare?h=${HASHES[0]},${HASHES[0]},${HASHES[1]}`);
		const panel = page.getByTestId('compare-error-duplicate_hash');
		await expect(panel.getByRole('button', { name: /^Remove / })).toHaveCount(3);
		await panel
			.getByRole('button', { name: /^Remove / })
			.first()
			.click();
		await expect(page).toHaveURL(new RegExp(`h=${HASHES[0]},${HASHES[1]}(&|$)`));
	});

	test('a server error is a banner with Retry', async ({ page, go, mock }) => {
		mock.error('compareTorrents', 503, 'unavailable', 'x');
		await go(url(2));
		await expect(page.getByRole('alert')).toContainText("couldn't load");
		serve(mock);
		await page.getByRole('button', { name: 'Try again' }).click();
		await expect(page.getByTestId('compare-table')).toBeVisible();
	});

	test('fewer than two torrents asks for no request and offers Search', async ({
		page,
		go,
		mock
	}) => {
		await go(`/compare?h=${HASHES[0]}`);
		await expect(page.getByText(/Select 2 to 5 torrents in Search/)).toBeVisible();
		expect(mock.calls('compareTorrents')).toHaveLength(0);
	});
});

test('a 2,000-row truncated list opens, scrolls to its last row and links to the torrent', async ({
	page,
	go,
	mock
}) => {
	serve(mock, (r) => {
		r.file_diff.only[0] = fileList(12345, 2000, true);
	});
	await page.setViewportSize(COLUMNS);
	await go(url(2));
	const section = page.getByTestId('compare-diff-only-0');
	await expect(page.getByTestId('compare-diff-only-0-count')).toHaveText('12,345 files');
	await section.locator('summary').click();
	const rows = section.getByRole('listitem');
	await expect(rows).toHaveCount(2000);
	await rows.last().scrollIntoViewIfNeeded();
	await expect(rows.last()).toBeInViewport();
	await expect(page.getByTestId('compare-diff-only-0-truncated').getByRole('link')).toHaveAttribute(
		'href',
		`/t/${HASHES[0]}`
	);
});
