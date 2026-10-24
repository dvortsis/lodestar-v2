// The facet rail (S-search-2b; architecture §10.4, §10.15): the server's counts in decision order,
// clicks as AST edits through W5 (one history entry each), the brush by pointer and keys, "Show
// more", the swarm groups before M4, the sheets below x-large (the bottom one staged), and axe.
// Name ends in `routes.test.ts` so the `core` project runs it.
import type { Page, Request } from '@playwright/test';
import type { components } from '../src/lib/api/schema';
import { expectNoA11yViolations } from './a11y';
import type { OpenApiMock } from './mock/openapi-mock';
import { body, expect, hits, print, row, server, type Ast } from './search-results.fixtures';
import { test } from './support/test';

type Facets = components['schemas']['FacetsResponse'];
type Term = components['schemas']['TermFacet'];
const GiB = 2 ** 30;
const DAY = 86_400;
const NOW = Math.floor(Date.now() / 1000);

const tops = (ast: Ast): Ast[] => (ast.op === 'and' ? ast.of : ast.op === 'all' ? [] : [ast]);
/** The values a top-level clause on `key` selects, and the ones a negated one excludes. */
function chosen(ast: Ast, key: string) {
	const pick = (n: Ast): string[] =>
		(n.op === 'tag' && n.key === key) || (n.op === 'ext' && key === 'ext') ? n.any : [];
	return {
		inc: tops(ast).flatMap(pick),
		exc: tops(ast).flatMap((n) => (n.op === 'not' ? pick(n.of) : []))
	};
}
const term = (ast: Ast, key: string, values: [string, number][], over: Partial<Term> = {}) => {
	const { inc, exc } = chosen(ast, key);
	return {
		approx: false,
		distinct: values.length,
		other: 0,
		values: values.map(([value, count]) => ({
			value,
			count,
			selected: inc.includes(value),
			excluded: exc.includes(value)
		})),
		...over
	};
};

/** The server stand-in: counts that depend on the query, so a staged query shows its own total. */
function facetsFor(ast: Ast, limits: Record<string, number>): Facets {
	const filters = tops(ast).filter((n) => n.op !== 'word').length;
	const ext = Array.from({ length: limits.ext ?? 8 }, (_, i): [string, number] => [
		`e${i}`,
		100 - i
	]);
	return {
		backfill_before: NOW - 20 * DAY,
		buckets: { seeders: null, trackers: null },
		canonical: '',
		facets: {
			type: term(ast, 'type', [
				['movie', 812],
				['tv', 301]
			]),
			res: term(ast, 'res', [
				['1080p', 640],
				['2160p', 210],
				['480p', 33]
			]),
			ext: term(ast, 'ext', ext, { distinct: 30, other: limits.ext ? 0 : 220 }),
			lang: term(ast, 'lang', [['en', 900]])
		},
		histograms: {
			size: {
				approx: false,
				missing: 0,
				unit: 'bytes',
				bins: [
					{ lo: 0, hi: GiB, count: 300, backfill: false },
					{ lo: GiB, hi: 4 * GiB, count: 700, backfill: false },
					{ lo: 4 * GiB, hi: 16 * GiB, count: 0, backfill: false },
					{ lo: 16 * GiB, hi: null, count: 284, backfill: false }
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
		total: 1284 - 400 * filters,
		total_approx: false,
		unavailable: ['seeders', 'trackers']
	};
}

function serve(mock: OpenApiMock) {
	server(mock, () => body(hits(3), { next_cursor: 'lease-1' }));
	mock.on('facetsPost', (request: Request) => {
		const sent = request.postDataJSON() as { ast: Ast; limits: Record<string, number> };
		return facetsFor(sent.ast, sent.limits);
	});
}
const facetCalls = (mock: OpenApiMock) =>
	mock
		.calls('facetsPost')
		.map(
			(r) => r.postDataJSON() as { ast: Ast; lease: string | null; limits: Record<string, number> }
		);
const q = (page: Page) => new URL(page.url()).searchParams.get('q');
const rail = (page: Page) => page.getByTestId('facet-rail');

test.describe('the rail at x-large', () => {
	test.use({ viewport: { width: 1366, height: 1000 } });

	test('shows the groups in order with the server’s counts, on the results’ snapshot', async ({
		page,
		go,
		mock
	}) => {
		serve(mock);
		await go('/search?q=lantern');
		await expect(row(page, 1)).toBeVisible();
		await expect(rail(page)).toBeVisible();
		const order = await rail(page)
			.locator(':scope > section, :scope > div > section')
			.evaluateAll((els) => els.map((e) => e.getAttribute('data-testid')));
		expect(order).toEqual([
			'facet-type',
			'facet-discovered',
			'facet-size',
			'facet-ext',
			'facet-res',
			'facet-seeders',
			'facet-trackers'
		]);
		await expect(page.getByTestId('facet-type-movie-count')).toContainText('812');
		await expect(page.getByTestId('facet-type-movie').locator('..')).toContainText('Movie');
		expect(facetCalls(mock).at(-1)!.lease).toBe('lease-1');
		// Before M4 the swarm groups say so, never zeros.
		await expect(page.getByTestId('facet-seeders-unavailable')).toContainText('Not available yet');
		await expect(page.getByTestId('facet-discovered-backfill')).toBeVisible();
	});

	test('a click edits the AST through W5: select, add, exclude, and Back undoes', async ({
		page,
		go,
		mock
	}) => {
		serve(mock);
		await go('/search?q=lantern');
		await expect(rail(page)).toBeVisible();
		await page.getByTestId('facet-res-2160p').check();
		await expect.poll(() => q(page)).toBe('lantern res:2160p');
		await expect(page.getByTestId('facet-res-2160p')).toBeChecked();
		await page.getByTestId('facet-res-1080p').check();
		await expect.poll(() => q(page)).toBe('lantern res:2160p,1080p');
		await page.getByTestId('facet-res-480p-exclude').click();
		await expect.poll(() => q(page)).toBe('lantern res:2160p,1080p -res:480p');
		await expect(page.getByTestId('facet-res-480p-exclude')).toHaveAttribute(
			'aria-pressed',
			'true'
		);
		await page.goBack();
		await expect.poll(() => q(page)).toBe('lantern res:2160p,1080p');
		await page.getByTestId('facet-res-clear').click();
		await expect.poll(() => q(page)).toBe('lantern');
	});

	test('two quick clicks keep both values, however slow /search is', async ({ page, go, mock }) => {
		serve(mock);
		await go('/search?q=lantern');
		await expect(page.getByTestId('facet-res-1080p')).toBeVisible();
		// From here /search (W5's canonical text and the rows) answers only when released.
		let release!: () => void;
		const gate = new Promise<void>((r) => (release = r));
		mock.on('searchPost', async (request: Request) => {
			await gate;
			const sent = request.postDataJSON() as { ast: Ast; limit: number };
			if (sent.limit === 1) return body([], { canonical: print(sent.ast) });
			return body(hits(3), { next_cursor: 'lease-2' });
		});
		await page.getByTestId('facet-res-1080p').check();
		await page.getByTestId('facet-res-2160p').check();
		release();
		await expect.poll(() => q(page)).toBe('lantern res:1080p,2160p');
	});

	test('a click after a new query failed edits the typed query, not the rows’', async ({
		page,
		go,
		mock
	}) => {
		serve(mock);
		server(mock, (sent) =>
			JSON.stringify(sent.ast).includes('keeper')
				? { status: 503, body: { error: { code: 'unavailable', message: 'down' } } }
				: body(hits(3), { next_cursor: 'lease-1' })
		);
		await go('/search?q=lantern');
		await expect(row(page, 1)).toBeVisible();
		const bar = page.getByRole('combobox', { name: 'Search torrents' });
		await bar.click();
		await page.keyboard.press('ControlOrMeta+a');
		await page.keyboard.type('keeper');
		await page.keyboard.press('Enter');
		await expect.poll(() => q(page)).toBe('keeper');
		await expect(page.getByTestId('results-error')).toBeVisible();
		await page.getByTestId('facet-res-1080p').check();
		await expect.poll(() => q(page)).toBe('keeper res:1080p');
	});

	test('the size brush by keyboard, and a Discovered preset', async ({ page, go, mock }) => {
		serve(mock);
		await go('/search?q=lantern');
		const plot = page.getByRole('group', { name: /Size histogram/ });
		await plot.focus();
		await page.keyboard.press('Enter'); // every bin
		await page.getByRole('slider', { name: 'Upper bound' }).focus();
		await page.keyboard.press('ArrowLeft'); // drop the open top bin
		await expect.poll(() => q(page)).toBe(`lantern size:<=${16 * GiB - 1}`);
		await page.getByRole('slider', { name: 'Lower bound' }).focus();
		await page.keyboard.press('ArrowRight');
		await expect.poll(() => q(page)).toBe(`lantern size:${GiB}..${16 * GiB - 1}`);
		await page.getByTestId('facet-discovered-604800').click();
		await expect.poll(() => q(page)).toContain('discovered:<604800');
		await expect(page.getByTestId('facet-discovered-604800')).toHaveAttribute(
			'aria-pressed',
			'true'
		);
	});

	test('Show more lists up to 100 extensions, with a search box', async ({ page, go, mock }) => {
		serve(mock);
		await go('/search?q=lantern');
		await expect(page.getByTestId('facet-ext-other')).toBeVisible();
		await page.getByTestId('facet-ext-more').click();
		await expect.poll(() => facetCalls(mock).at(-1)?.limits.ext).toBe(100);
		await expect(page.getByTestId('facet-ext-e99')).toBeVisible();
		await page.getByTestId('facet-ext-find').fill('e9');
		await expect(page.getByTestId('facet-ext').getByRole('checkbox')).toHaveCount(11); // e9, e90–e99
		await page.getByTestId('facet-ext-more').click();
		await expect(page.getByTestId('facet-ext').getByRole('checkbox')).toHaveCount(8);
	});
});

test('large: Filters (N) opens a left sheet that applies at once', async ({ page, go, mock }) => {
	await page.setViewportSize({ width: 800, height: 900 });
	serve(mock);
	await go('/search?q=lantern+res%3A1080p');
	await expect(row(page, 1)).toBeVisible();
	await page.waitForLoadState('networkidle');
	expect(mock.calls('facetsPost')).toHaveLength(0); // no rail shown: no counts asked for
	await page.getByTestId('facet-open').click();
	await expect.poll(() => mock.calls('facetsPost').length).toBe(1); // asked for on open
	await expect(page.getByTestId('facet-open')).toContainText('Filters (1)');
	await expect(rail(page)).toBeVisible();
	await page.getByTestId('facet-type-movie').check();
	await expect.poll(() => q(page)).toBe('lantern res:1080p type:movie');
});

test('small: the bottom sheet stages its clicks and shows the staged count', async ({
	page,
	go,
	mock
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	serve(mock);
	await go('/search?q=lantern');
	await expect(row(page, 1)).toBeVisible();
	await page.getByTestId('facet-open').click();
	await expect(page.getByTestId('facet-show')).toHaveText('Show 1,284 results');
	await page.getByTestId('facet-type-movie').check();
	await expect(page.getByTestId('facet-show')).toHaveText('Show 884 results');
	await expect(page.getByTestId('facet-type-movie')).toBeChecked();
	expect(q(page)).toBe('lantern'); // nothing written yet
	expect(facetCalls(mock).at(-1)!.lease).toBeNull(); // a staged query isn't a shown one
	await page.getByTestId('facet-show').click();
	await expect.poll(() => q(page)).toBe('lantern type:movie');
});

test('small: closing the sheet without Show writes nothing, and reopening starts afresh', async ({
	page,
	go,
	mock
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	serve(mock);
	await go('/search?q=lantern');
	await expect(row(page, 1)).toBeVisible();
	await page.getByTestId('facet-open').click();
	await page.getByTestId('facet-type-movie').check();
	await expect(page.getByTestId('facet-show')).toHaveText('Show 884 results');
	await page.keyboard.press('Escape');
	await expect(page.getByTestId('facet-show')).toHaveCount(0);
	expect(q(page)).toBe('lantern');
	await page.getByTestId('facet-open').click();
	await expect(page.getByTestId('facet-type-movie')).not.toBeChecked();
	await expect(page.getByTestId('facet-show')).toHaveText('Show 1,284 results');
});

for (const width of [390, 800]) {
	test(`a pasted link at ${width} px: the Filters button doesn't move the results`, async ({
		page,
		go,
		mock
	}) => {
		await page.setViewportSize({ width, height: 900 });
		serve(mock);
		await page.addInitScript(() => {
			const w = window as unknown as { __cls: number };
			w.__cls = 0;
			new PerformanceObserver((list) => {
				for (const e of list.getEntries() as unknown as {
					value: number;
					hadRecentInput: boolean;
				}[])
					if (!e.hadRecentInput) w.__cls += e.value;
			}).observe({ type: 'layout-shift', buffered: true });
		});
		await go('/search?q=lantern');
		await expect(row(page, 1)).toBeVisible();
		await expect(page.getByTestId('facet-open')).toBeVisible();
		const cls = await page.evaluate(() => (window as unknown as { __cls: number }).__cls);
		expect(cls).toBeLessThan(0.01);
	});
}

for (const mode of ['light', 'dark'] as const) {
	test.describe(`axe, Riverstone ${mode}`, () => {
		test.use({ themeId: `riverstone-${mode}`, colorMode: mode });
		test('the rail at 1366 px, with a selection and a brush', async ({ page, go, mock }) => {
			await page.setViewportSize({ width: 1366, height: 1000 });
			serve(mock);
			await go(`/search?q=lantern+res%3A1080p+size%3A${GiB}..${4 * GiB - 1}`);
			await expect(page.getByTestId('facet-size-histogram-selection')).toBeVisible();
			await page.getByTestId('facet-more').locator('summary').click();
			await expectNoA11yViolations(page);
		});
		test('the left sheet at 800 px', async ({ page, go, mock }) => {
			await page.setViewportSize({ width: 800, height: 900 });
			serve(mock);
			await go('/search?q=lantern+res%3A1080p');
			await page.getByTestId('facet-open').click();
			await expect(rail(page)).toBeVisible();
			await expectNoA11yViolations(page);
		});
		test('the bottom sheet at 390 px', async ({ page, go, mock }) => {
			await page.setViewportSize({ width: 390, height: 844 });
			serve(mock);
			await go('/search?q=lantern');
			await page.getByTestId('facet-open').click();
			await expect(page.getByTestId('facet-show')).toBeVisible();
			await expectNoA11yViolations(page);
		});
	});
}
