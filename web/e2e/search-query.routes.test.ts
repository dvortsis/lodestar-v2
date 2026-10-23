// The search page's query bar, highlighter and error states (S-search-1;
// architecture §10.4, §10.13, §10.15). The server's parser is mocked by a tiny stand-in that knows
// `res:`, `ext:` and words. Name ends in `routes.test.ts` so the `core` project runs it.
import type { Page, Request } from '@playwright/test';
import type { components } from '../src/lib/api/schema';
import { expectNoA11yViolations } from './a11y';
import type { OpenApiMock } from './mock/openapi-mock';
import { interact } from './support/interact';
import { expect, test } from './support/test';

type Ast = components['schemas']['AstNode'];

const toAst = (q: string): Ast => {
	const of = q
		.split(/\s+/)
		.filter(Boolean)
		.map((w): Ast => {
			if (w.startsWith('res:')) return { op: 'tag', key: 'res', any: [w.slice(4)] };
			if (w.startsWith('ext:')) return { op: 'ext', any: [w.slice(4)] };
			return { op: 'word', field: 'any', word: w };
		});
	return of.length === 0 ? { op: 'all' } : of.length === 1 ? of[0]! : { op: 'and', of };
};

/** The parser stand-in: an unclosed quote is a syntax error spanning the quote to the end. */
function parser(mock: OpenApiMock) {
	mock.on('parseQuery', (request: Request) => {
		const q = new URL(request.url()).searchParams.get('q') ?? '';
		const quote = Buffer.from(q).indexOf('"');
		if (quote >= 0 && q.split('"').length % 2 === 0) {
			return {
				status: 400,
				body: {
					error: {
						code: 'query_syntax',
						message: 'unclosed quote',
						field: 'q',
						start: quote,
						end: Buffer.byteLength(q)
					}
				}
			};
		}
		return { ast: toAst(q), canonical: q.trim().replace(/\s+/g, ' ') };
	});
}

/** The URL's path and `q` (the `go` fixture adds `density`, which every write keeps). */
const at = (page: Page) => {
	const u = new URL(page.url());
	return { path: u.pathname, q: u.searchParams.get('q'), hidden: u.searchParams.get('hidden') };
};
const expectQuery = (page: Page, q: string | null, hidden: string | null = null) =>
	expect.poll(() => at(page)).toEqual({ path: '/search', q, hidden });

const editor = (page: Page) => page.getByRole('combobox', { name: 'Search torrents' });

test.beforeEach(({ mock }) => parser(mock));

test('the bar is a combobox in a search landmark, with its message line', async ({ page, go }) => {
	await go('/search');
	await expect(page.getByRole('search', { name: 'Search torrents' })).toBeVisible();
	await expect(editor(page)).toHaveAttribute('aria-describedby', 'search-query-message');
	await expect(editor(page)).toHaveAttribute('aria-expanded', 'false');
	const box = await editor(page).boundingBox();
	expect(box!.height).toBe(48); // --query-bar-height
});

test('typed tokens get their classes', async ({ page, go }) => {
	await go('/search');
	await editor(page).click();
	await page.keyboard.type('size:>4GB res:1080p "big buck" -cam');
	const bar = editor(page);
	await expect(bar.locator('.qt-field')).toHaveText(['size:', 'res:']);
	await expect(bar.locator('.qt-range')).toHaveText('>4GB');
	await expect(bar.locator('.qt-value')).toHaveText('1080p');
	await expect(bar.locator('.qt-phrase.qt-quote')).toHaveText(['"', '"']);
	await expect(bar.locator('.qt-operator')).toHaveText('-');
	await expect(bar).toHaveText('size:>4GB res:1080p "big buck" -cam');
	// Weight, mono and colour (§10.4), from the lazy stylesheet.
	const style = await bar
		.locator('.qt-field')
		.first()
		.evaluate((el) => [getComputedStyle(el).fontFamily, getComputedStyle(el).fontWeight]);
	expect(style[0]).toContain('Atkinson Hyperlegible Mono');
	expect(style[1]).toBe('600');
	// The caret stayed at the end through every re-render.
	await page.keyboard.type('x');
	await expect(bar).toHaveText('size:>4GB res:1080p "big buck" -camx');
});

test('a parse error is marked in the bar and explained under it', async ({ page, go, mock }) => {
	mock.error('parseQuery', 400, 'query_syntax', 'unclosed quote', { field: 'q', start: 4, end: 9 });
	await go('/search');
	await editor(page).click();
	await page.keyboard.type('big "buck');
	await page.keyboard.press('Enter');
	await expect(editor(page)).toHaveAttribute('aria-invalid', 'true');
	const message = page.locator('#search-query-message');
	await expect(message).toHaveText(/Can't read this query:\s+unclosed quote/);
	await expect(editor(page).locator('.qt-error')).toHaveText(['"', 'buck']);
	await expectQuery(page, 'big "buck'); // the raw text stays in the URL
});

for (const [lang, composing, committed] of [
	['zh', 'dian', '电影'],
	['ja', 'にほん', '日本']
] as const) {
	test(`a parse answer during an IME composition (${lang}) leaves it intact`, async ({
		page,
		go,
		mock
	}) => {
		// The server rejects the bare `name:` the user typed before composing.
		mock.error('parseQuery', 400, 'query_syntax', 'name: needs a value', {
			field: 'q',
			start: 0,
			end: 5
		});
		await go('/search');
		await editor(page).click();
		// The editor has loaded, so typing reaches the parse (before it loads, the text is adopted).
		await expect(page.locator('#search-query-message[data-live]')).toBeAttached();
		await page.keyboard.type('name:');
		const cdp = await page.context().newCDPSession(page);
		await cdp.send('Input.imeSetComposition', {
			text: composing,
			selectionStart: composing.length,
			selectionEnd: composing.length
		});
		// The parse of "name:" answers (debounced) while the composition is open.
		await expect(editor(page)).toHaveAttribute('aria-invalid', 'true');
		await expect.poll(() => mock.calls('parseQuery').length).toBeGreaterThan(0);
		await expect(editor(page)).toHaveText(`name:${composing}`);
		await cdp.send('Input.insertText', { text: committed });
		await expect(editor(page)).toHaveText(`name:${committed}`);
		await expect(editor(page).locator('.qt-field')).toHaveText('name:');
	});
}

test('Enter pushes the search; back and forward walk them', async ({ page, go }) => {
	await go('/search');
	await editor(page).click();
	await page.keyboard.type('lantern');
	await page.keyboard.press('Enter');
	await expectQuery(page, 'lantern');
	await page.keyboard.type(' res:1080p');
	await page.keyboard.press('Enter');
	await expectQuery(page, 'lantern res:1080p');
	await page.goBack();
	await expect(editor(page)).toHaveText('lantern');
	await page.goForward();
	await expect(editor(page)).toHaveText('lantern res:1080p');
	await expect(editor(page).locator('.qt-field')).toHaveText('res:');
});

test('below large the bar stays at the top while the page scrolls', async ({ page, go }) => {
	await page.setViewportSize({ width: 390, height: 700 });
	await go('/search?q=res%3A1080p');
	await expect(editor(page)).toHaveText('res:1080p');
	await page.evaluate(() => {
		const tall = document.createElement('div');
		tall.style.height = '3000px';
		document.getElementById('results')!.append(tall);
	});
	await page.mouse.wheel(0, 1200);
	await expect
		.poll(async () => (await page.getByRole('search').boundingBox())!.y)
		.toBeLessThanOrEqual(1);
	expect(await page.evaluate(() => scrollY)).toBeGreaterThan(400);
	// From large up it scrolls away with the page.
	await page.setViewportSize({ width: 1024, height: 700 });
	await expect.poll(async () => (await page.getByRole('search').boundingBox())!.y).toBeLessThan(0);
});

test('the bare page does not shift, and fetches nothing for the query', async ({
	page,
	go,
	mock
}) => {
	await page.addInitScript(() => {
		(window as unknown as { __cls: number }).__cls = 0;
		new PerformanceObserver((list) => {
			for (const e of list.getEntries() as unknown as { value: number; hadRecentInput: boolean }[])
				if (!e.hadRecentInput) (window as unknown as { __cls: number }).__cls += e.value;
		}).observe({ type: 'layout-shift', buffered: true });
	});
	await go('/search');
	await interact(page, page.locator('#search-query-message[data-live]')); // the chrome and the bar's lazy part arrive
	await expect(page.getByTestId('more-button')).toBeAttached();
	expect(await page.evaluate(() => (window as unknown as { __cls: number }).__cls)).toBeLessThan(
		0.001
	);
	expect(mock.calls('parseQuery')).toHaveLength(0);
});

test('a signed-out visitor is sent to sign-in by the first 401, with no interaction', async ({
	page,
	go,
	mock
}) => {
	mock.error('parseQuery', 401, 'login_required', 'Sign in first');
	mock.on('getAuthStatus', { logged_in: false, setup_required: false });
	await go('/search?q=bunny');
	await expect(page).toHaveURL(/\/login\?next=/);
	expect(new URL(page.url()).searchParams.get('next')).toMatch(/^\/search\?q=bunny(&|$)/);
});

for (const [name, themeId, colorMode] of [
	['Riverstone light', 'riverstone-light', 'light'],
	['Riverstone dark', 'riverstone-dark', 'dark']
] as const) {
	test.describe(`axe, ${name}`, () => {
		test.use({ themeId, colorMode });
		for (const width of [1366, 390]) {
			test(`a highlighted query at ${width} px`, async ({ page, go }) => {
				await page.setViewportSize({ width, height: 800 });
				await go('/search?q=lantern+res%3A1080p+ext%3Amkv');
				await expect(editor(page).locator('.qt-field')).toHaveCount(2);
				await expectNoA11yViolations(page);
			});
			test(`a parse error at ${width} px`, async ({ page, go }) => {
				await page.setViewportSize({ width, height: 800 });
				await go('/search?q=big+%22buck');
				await expect(editor(page)).toHaveAttribute('aria-invalid', 'true');
				await expectNoA11yViolations(page);
			});
		}
	});
}
