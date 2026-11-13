// The results and the facet rail against the real server (`lodestar serve --fixture`, bench/README.md
// "Fixture backend"), not the OpenAPI mock. Local only until S-e2e wires a fixture job: it skips
// unless E2E_FIXTURE_URL is set, and imports @playwright/test directly so no mock is installed. Run
// it as search-query.fixture.test.ts says.
import { expect as base, test, type Page } from '@playwright/test';
import { FIXTURE_STATE } from './support/fixture-session';

const expect = base.configure({ timeout: 10_000 });
const BASE = process.env.E2E_FIXTURE_URL;

test.skip(!BASE, 'E2E_FIXTURE_URL is not set (fixture backend specs are local only)');

test.describe.configure({ mode: 'serial' });
let page: Page;
test.beforeAll(async ({ browser }) => {
	const context = await browser.newContext({
		baseURL: BASE,
		viewport: { width: 1366, height: 1000 },
		storageState: FIXTURE_STATE
	});
	page = await context.newPage();
});
test.afterAll(() => page?.context().close());

const q = () => new URL(page.url()).searchParams.get('q');

test('lantern lists rows; type:movie is checked in Type with the server’s count; unchecking drops it', async () => {
	await page.goto('/search?q=lantern');
	await expect(
		page.getByRole('grid', { name: 'Matching torrents' }).getByRole('row').first()
	).toBeVisible();
	const movie = page.getByTestId('facet-type-movie');
	await expect(movie).toBeVisible();
	await expect(movie).not.toBeChecked();

	// The count the rail shows for Movie is the one /facets sends for this query.
	const facets = await page.request.post('/api/v1/facets', {
		data: { q: 'lantern', limits: {} }
	});
	const counts = (await facets.json()) as {
		facets: Record<string, { values: { value: string; count: number }[] }>;
	};
	const want = counts.facets.type!.values.find((v) => v.value === 'movie')!.count;
	await expect(page.getByTestId('facet-type-movie-count')).toContainText(
		new Intl.NumberFormat('en').format(want)
	);

	// Checking changes the query, and the rail re-renders from the /facets answer for the new one. Unchecking
	// before that lands can hit the old checkbox, which the re-render then resets ("did not change its
	// state"), so wait for that answer, then for the rail to show its settled state.
	const settled = page.waitForResponse(
		(r) =>
			new URL(r.url()).pathname === '/api/v1/facets' &&
			r.request().method() === 'POST' &&
			(r.request().postData() ?? '').includes('"key":"type"')
	);
	await movie.check();
	await expect.poll(q).toBe('lantern type:movie');
	await settled;
	await expect(page.getByTestId('facet-type-movie')).toBeChecked();
	// The Clear button renders only once the server's data marks a Type value selected: the rail has re-rendered.
	await expect(page.getByTestId('facet-type-clear')).toBeVisible();
	await page.getByTestId('facet-type-movie').uncheck();
	await expect.poll(q).toBe('lantern');
	await expect(page.getByTestId('facet-type-movie')).not.toBeChecked();
});
