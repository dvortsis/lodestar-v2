// The search page's results (S-search-2; architecture §10.4, §10.5, §10.12): the rows, opening one,
// row actions, every state, no layout shift on a pasted link, and axe. Name ends in
// `routes.test.ts` so the `core` project runs it.
import type { Request } from '@playwright/test';
import { expectNoA11yViolations } from './a11y';
import type { Page } from '@playwright/test';
import {
	body,
	copied,
	expect,
	figures,
	grid,
	hashOf,
	hits,
	recordClipboard,
	resultsCalls,
	row,
	rowOf,
	server,
	type Ast
} from './search-results.fixtures';
import { interact } from './support/interact';
import { test } from './support/test';

const bar = (page: Page) => page.getByRole('combobox', { name: 'Search torrents' });
/** Types at the end of the query in the bar, then Enter. */
async function append(page: Page, text: string) {
	await bar(page).focus();
	await page.keyboard.press('End');
	await page.keyboard.type(text);
	await page.keyboard.press('Enter');
}

test('a pasted link shows the rows with their facts', async ({ page, go, mock }) => {
	server(mock, () => body(hits(3), { total: 3 }));
	await go('/search?q=lantern');
	await expect(row(page, 1)).toBeVisible();
	await expect(grid(page).getByRole('row')).toHaveCount(3);
	const id = `result-${hashOf(1)}`;
	await expect(page.getByTestId(`${id}-title`)).toHaveText('Lantern Keeper 1');
	await expect(page.getByTestId(`${id}-raw`)).toHaveText('Lantern.Keeper.1.1080p');
	await expect(page.getByTestId(`${id}-badge-type`)).toHaveText('TV');
	await expect(page.getByTestId(`${id}-badge-private`)).toBeVisible();
	await expect(page.getByTestId(`${id}-seeders`)).toHaveText('120');
	await expect(page.getByTestId(`${id}-trackers`)).toContainText('4 / 20');
	await expect(page.getByTestId('results-announce')).toHaveText('3 torrents');
});

test('one edit asks for its results once, and a parse error keeps the rows', async ({
	page,
	go,
	mock
}) => {
	server(mock, (sent) =>
		JSON.stringify(sent.ast).includes('keeper') ? body(hits(1, 10)) : body(hits(2))
	);
	await go('/search?q=lantern');
	await expect(row(page, 1)).toBeVisible();
	expect(resultsCalls(mock)).toHaveLength(1);
	await append(page, ' keeper');
	await expect(row(page, 11)).toBeVisible();
	await page.waitForLoadState('networkidle');
	expect(resultsCalls(mock)).toHaveLength(2);

	// An unclosed quote: the bar explains it, the rows stay, and nothing is asked for.
	await append(page, ' "open');
	await expect(page.locator('#search-query-message')).toContainText('unclosed quote');
	await expect(row(page, 11)).toBeVisible();
	expect(resultsCalls(mock)).toHaveLength(2);
});

test('leaving /search and coming back asks again: an earlier answer may be stale', async ({
	page,
	go,
	mock
}) => {
	let answer = 1;
	server(mock, () => body(hits(1, answer === 1 ? 0 : 40)));
	await go('/search?q=lantern');
	await expect(row(page, 1)).toBeVisible();
	const before = resultsCalls(mock).length;
	await page.getByTestId('nav-alerts').click(); // in the app: the store outlives the page
	await expect(page).toHaveURL(/\/alerts/);
	answer = 2; // e.g. "Show adult content" was turned off meanwhile
	await page.goBack();
	await expect(row(page, 41)).toBeVisible();
	await expect(row(page, 1)).toHaveCount(0);
	expect(resultsCalls(mock).length).toBeGreaterThan(before);
});

test('Enter opens the drawer on the row; Esc gives focus back to it', async ({
	page,
	go,
	mock
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	server(mock, () => body(hits(3)));
	await go('/search?q=lantern');
	await expect(row(page, 2)).toBeVisible();
	await rowOf(page, 2).focus();
	await page.keyboard.press('Enter');
	await expect(page.getByTestId('drawer')).toBeVisible();
	await expect(page).toHaveURL(new RegExp(`/t/${hashOf(2)}`));
	await page.keyboard.press('Escape');
	await expect(page.getByTestId('drawer')).toHaveCount(0);
	await expect(rowOf(page, 2)).toBeFocused();
	await expect(page).toHaveURL(/\/search\?/);
});

test('Copy magnet on a row copies it without opening the drawer', async ({ page, go, mock }) => {
	await recordClipboard(page);
	server(mock, () => body(hits(2)));
	await go('/search?q=lantern');
	await row(page, 1).hover();
	await page.getByTestId(`result-${hashOf(1)}-copy`).click();
	await expect
		.poll(() => copied(page), { timeout: 10_000 })
		.toBe(`magnet:?xt=urn:btih:${hashOf(1)}`);
	await expect(page.getByTestId('drawer')).toHaveCount(0);
});

test('More loads its menu on the first click and copies the info hash', async ({
	page,
	go,
	mock
}) => {
	await recordClipboard(page);
	server(mock, () => body(hits(2)));
	await go('/search?q=lantern');
	await row(page, 2).hover();
	await page.getByTestId(`result-${hashOf(2)}-more`).click();
	await page.getByRole('menuitem', { name: 'Copy info hash' }).click();
	await expect.poll(() => copied(page), { timeout: 10_000 }).toBe(hashOf(2));
	await expect(page.getByTestId('drawer')).toHaveCount(0);
});

test('scrolling to the end loads the next page with the cursor', async ({ page, go, mock }) => {
	server(mock, (sent) => {
		const cursor = (sent as { cursor?: string | null }).cursor;
		return cursor === 'p2' ? body(hits(10, 50)) : body(hits(50), { next_cursor: 'p2', total: 60 });
	});
	await go('/search?q=lantern');
	await expect(row(page, 1)).toBeVisible();
	await rowOf(page, 1).focus();
	await page.keyboard.press('End');
	await expect
		.poll(() => mock.calls('searchPost').some((r) => r.postDataJSON().cursor === 'p2'), {
			timeout: 10_000
		})
		.toBe(true);
	await expect(row(page, 51)).toBeAttached();
	await page.keyboard.press('End');
	await expect(row(page, 60)).toBeVisible();
});

test.describe('states', () => {
	test('the bare page after an interaction: the empty state with examples', async ({
		page,
		go,
		mock
	}) => {
		mock.on('parseQuery', { ast: { op: 'all' }, canonical: '' });
		mock.on('getStats', figures(2_400_000));
		mock.on('listSavedSearches', []);
		await go('/search');
		await interact(page);
		await expect(page.getByTestId('results-empty-title')).toHaveText('Search 2,400,000 torrents');
		await expect(page.getByTestId('results-empty').getByRole('link')).toHaveCount(3);
		await page.getByRole('button', { name: 'Start typing' }).click();
		await expect(page.getByRole('combobox', { name: 'Search torrents' })).toBeFocused();
	});

	test('loading shows skeletons after 300 ms, then the rows', async ({ page, go, mock }) => {
		let release!: () => void;
		const gate = new Promise<void>((r) => (release = r));
		server(mock, () => body(hits(2)));
		mock.on('searchPost', (async (request: Request) => {
			const sent = request.postDataJSON() as { limit: number };
			if (sent.limit !== 1) await gate;
			return body(sent.limit === 1 ? [] : hits(2));
		}) as never);
		await go('/search?q=lantern');
		await expect(page.getByTestId('results-loading')).toBeVisible();
		await expect(
			page.getByTestId('results-loading').locator('.rp-skeleton-row').first()
		).toBeVisible({
			timeout: 10_000
		});
		release();
		await expect(row(page, 1)).toBeVisible();
	});

	test('an error says so with Retry, and Retry recovers', async ({ page, go, mock }) => {
		let fail = true;
		server(mock, () =>
			fail
				? { status: 503, body: { error: { code: 'unavailable', message: 'down' } } }
				: body(hits(1))
		);
		await go('/search?q=lantern');
		await expect(page.getByTestId('results-error')).toHaveText(
			'Lodestar is busy. Try again in a moment.'
		);
		fail = false;
		await page.getByRole('button', { name: 'Retry' }).click();
		await expect(row(page, 1)).toBeVisible();
		await expect(page.getByTestId('results-error')).toHaveCount(0);
	});

	test('no matches, and adult results hidden with a link to the setting', async ({
		page,
		go,
		mock
	}) => {
		server(mock, (sent) =>
			JSON.stringify(sent.ast).includes('adult')
				? body([], { notices: ['adult_hidden'] })
				: body([])
		);
		await go('/search?q=nothing');
		await expect(page.getByTestId('results-none')).toContainText('Try fewer or different words.');
		await expect(page.getByTestId('results-announce')).toHaveText('No torrents match');
		await go('/search?q=adult');
		await expect(page.getByTestId('results-adult-hidden')).toBeVisible();
		await expect(page.getByRole('link', { name: /appearance/i })).toHaveAttribute(
			'href',
			/\/settings\/appearance/
		);
	});

	test('no matches with filters: remove one, or clear them all', async ({ page, go, mock }) => {
		server(mock, (sent) => (JSON.stringify(sent.ast).includes('"ext"') ? body([]) : body(hits(2))));
		await go('/search?q=lantern+ext%3Amkv');
		const none = page.getByTestId('results-none');
		await expect(none).toBeVisible();
		await none.getByRole('button', { name: 'Remove Extension: .mkv' }).click();
		await expect(row(page, 1)).toBeVisible();
		await expect(page).toHaveURL(/[?&]q=lantern(&|$)/);

		await go('/search?q=lantern+ext%3Amkv');
		await expect(none).toBeVisible();
		await none.getByRole('button', { name: 'Clear all filters' }).click();
		await expect(row(page, 1)).toBeVisible();
		await expect(page).toHaveURL(/[?&]q=lantern(&|$)/);
		// One history entry: Back returns to the filtered search.
		await page.goBack();
		await expect(none).toBeVisible();
	});

	test('an expired cursor starts the list again from the top and says so', async ({
		page,
		go,
		mock
	}) => {
		let expired = false;
		server(mock, (sent) => {
			if (sent.cursor === 'p2') {
				expired = true;
				return { status: 410, body: { error: { code: 'cursor_expired', message: 'gone' } } };
			}
			return expired ? body(hits(3, 100)) : body(hits(50), { next_cursor: 'p2', total: 60 });
		});
		await go('/search?q=lantern');
		await expect(row(page, 1)).toBeVisible();
		await rowOf(page, 1).focus();
		await page.keyboard.press('End');
		await expect(page.getByTestId('results-expired')).toBeVisible();
		await expect(row(page, 101)).toBeVisible();
	});

	test('refining keeps the last rows under a hairline', async ({ page, go, mock }) => {
		let release!: () => void;
		const gate = new Promise<void>((r) => (release = r));
		server(mock, () => body(hits(2)));
		mock.on('searchPost', (async (request: Request) => {
			const sent = request.postDataJSON() as { ast: Ast; limit: number };
			if (sent.limit === 1) return body([]);
			if (JSON.stringify(sent.ast).includes('keeper')) {
				await gate;
				return body(hits(1, 10));
			}
			return body(hits(2));
		}) as never);
		await go('/search?q=lantern');
		await expect(row(page, 1)).toBeVisible();
		await append(page, ' keeper');
		await expect(page.getByTestId('results-refining')).toBeAttached();
		await expect(row(page, 1)).toBeVisible();
		release();
		await expect(row(page, 11)).toBeVisible();
		await expect(page.getByTestId('results-refining')).toHaveCount(0);
	});

	test('a cold start says how many are searchable so far', async ({ page, go, mock }) => {
		server(mock, () => body(hits(1)));
		mock.on('getStats', figures(4_200));
		await go('/search?q=lantern');
		await expect(page.getByTestId('results-cold-start')).toContainText('4,200');
	});
});

test('a pasted link with filters does not shift the results as the strip loads', async ({
	page,
	go,
	mock
}) => {
	await page.setViewportSize({ width: 1366, height: 900 });
	server(mock, () => body(hits(3)));
	await page.addInitScript(() => {
		const w = window as unknown as { __cls: number };
		w.__cls = 0;
		new PerformanceObserver((list) => {
			for (const e of list.getEntries() as unknown as { value: number; hadRecentInput: boolean }[])
				if (!e.hadRecentInput) w.__cls += e.value;
		}).observe({ type: 'layout-shift', buffered: true });
	});
	await go('/search?q=lantern+ext%3Amkv');
	await expect(page.getByTestId('filter-strip')).toBeVisible();
	await expect(row(page, 1)).toBeVisible();
	const cls = await page.evaluate(() => (window as unknown as { __cls: number }).__cls);
	expect(cls).toBeLessThan(0.01);
});

test('a cold start answered after the results does not shift them', async ({ page, go, mock }) => {
	await page.setViewportSize({ width: 1366, height: 900 });
	let answered!: () => void;
	const results = new Promise<void>((r) => (answered = r));
	server(mock, () => {
		answered();
		return body(hits(3));
	});
	// /stats answers only once the results have: the worst order for the banner above the rows.
	mock.on('getStats', (async () => {
		await results;
		return figures(2_208);
	}) as never);
	await page.addInitScript(() => {
		const w = window as unknown as { __cls: number };
		w.__cls = 0;
		new PerformanceObserver((list) => {
			for (const e of list.getEntries() as unknown as { value: number; hadRecentInput: boolean }[])
				if (!e.hadRecentInput) w.__cls += e.value;
		}).observe({ type: 'layout-shift', buffered: true });
	});
	await go('/search?q=lantern');
	await expect(page.getByTestId('results-cold-start')).toBeVisible();
	await expect(row(page, 1)).toBeVisible();
	const cls = await page.evaluate(() => (window as unknown as { __cls: number }).__cls);
	expect(cls).toBeLessThan(0.01);
});

for (const mode of ['light', 'dark'] as const) {
	test.describe(`axe, Riverstone ${mode}`, () => {
		test.use({ themeId: `riverstone-${mode}`, colorMode: mode });
		for (const width of [1366, 390]) {
			test(`the rows, an error and the empty state at ${width} px`, async ({ page, go, mock }) => {
				await page.setViewportSize({ width, height: 900 });
				server(mock, (sent) =>
					JSON.stringify(sent.ast).includes('broken')
						? { status: 503, body: { error: { code: 'unavailable', message: 'down' } } }
						: body(hits(4))
				);
				await go('/search?q=lantern');
				await expect(row(page, 1)).toBeVisible();
				await row(page, 1).hover();
				await expectNoA11yViolations(page);
				await go('/search?q=broken');
				await expect(page.getByTestId('results-error')).toBeVisible();
				await expectNoA11yViolations(page);
				await go('/search');
				await interact(page);
				await expect(page.getByTestId('results-empty')).toBeVisible();
				await expectNoA11yViolations(page);
			});

			test(`no matches, adult hidden, a cold start and skeletons at ${width} px`, async ({
				page,
				go,
				mock
			}) => {
				await page.setViewportSize({ width, height: 900 });
				let release!: () => void;
				const gate = new Promise<void>((r) => (release = r));
				server(mock, () => body([]));
				mock.on('getStats', figures(4_200));
				mock.on('searchPost', (async (request: Request) => {
					const sent = request.postDataJSON() as { ast: Ast; limit: number };
					const text = JSON.stringify(sent.ast);
					if (sent.limit === 1) return body([]);
					if (text.includes('slow')) await gate;
					if (text.includes('adult')) return body([], { notices: ['adult_hidden'] });
					return text.includes('"ext"') ? body([]) : body(hits(2));
				}) as never);
				await go('/search?q=lantern+ext%3Amkv');
				await expect(page.getByTestId('results-none-clauses')).toBeVisible();
				await expect(page.getByTestId('results-cold-start')).toBeVisible();
				await expectNoA11yViolations(page);
				await go('/search?q=adult');
				await expect(page.getByTestId('results-adult-hidden')).toBeVisible();
				await expectNoA11yViolations(page);
				await go('/search?q=slow');
				await expect(page.locator('.rp-skeleton-row').first()).toBeVisible();
				await expectNoA11yViolations(page);
				release();
			});
		}
	});
}
