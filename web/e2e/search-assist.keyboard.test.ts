// Autocomplete, smart chips, natural phrasing and the Builder from the keyboard alone (S-search-4;
// §10.4, §10.13, §10.16): type, arrow through the suggestions, Tab accepts, the results update; the
// Builder round-trips with the bar. Against the OpenAPI mock, with axe on the open states.
import type { Request } from '@playwright/test';
import type { components } from '../src/lib/api/schema';
import { expectNoA11yViolations } from './a11y';
import { expect, test } from './support/test';

type Ast = components['schemas']['AstNode'];
type SuggestItem = components['schemas']['SuggestItem'];

const toAst = (q: string): Ast => {
	const of = q
		.split(/\s+/)
		.filter(Boolean)
		.map((w): Ast => {
			if (w.startsWith('res:')) return { op: 'tag', key: 'res', any: [w.slice(4)] };
			return { op: 'word', field: 'any', word: w };
		});
	return of.length === 0 ? { op: 'all' } : of.length === 1 ? of[0]! : { op: 'and', of };
};
const print = (ast: Ast): string =>
	(ast.op === 'and' ? ast.of : ast.op === 'all' ? [] : [ast])
		.map((n) => (n.op === 'word' ? n.word : n.op === 'tag' ? `res:${n.any[0]}` : ''))
		.join(' ');

const SEARCH = (request: Request) => ({
	canonical: print((request.postDataJSON() as { ast: Ast }).ast),
	hits: [],
	limit: 1,
	next_cursor: null,
	notices: [],
	offset: 0,
	stemmed: false,
	took_ms: 1,
	total: 0,
	total_approx: false
});

test.beforeEach(async ({ mock }) => {
	mock.on('parseQuery', (request: Request) => {
		const url = new URL(request.url());
		const q = url.searchParams.get('q') ?? '';
		const base = { ast: toAst(q), canonical: q.trim() };
		if (url.searchParams.get('natural') !== 'true' || !q.includes('4k')) return base;
		return {
			...base,
			natural: {
				ast: toAst('res:2160p movies'),
				canonical: 'res:2160p movies',
				chips: [{ from: { start: 0, end: 2 }, text: 'res:2160p' }],
				text: ['movies']
			}
		};
	});
	mock.on('searchPost', SEARCH);
	mock.on('listSavedSearches', []);
	mock.on('suggest', (request: Request) => {
		const q = new URL(request.url()).searchParams.get('q') ?? '';
		const items: SuggestItem[] = q.startsWith('2160')
			? [
					{
						kind: 'chip',
						text: 'res:2160p',
						insert: 'res:2160p',
						replace: { start: 0, end: q.length },
						count: 1284
					},
					{
						kind: 'title',
						text: '2160 Frames',
						insert: 'title:"2160 Frames"',
						replace: { start: 0, end: q.length },
						count: 3
					}
				]
			: [];
		return { items };
	});
});

test('type, arrow, Tab accepts the smart chip, and the results query updates', async ({
	page,
	go,
	kbd
}) => {
	await go('/search');
	const bar = page.getByRole('combobox', { name: 'Search torrents' });
	await expect(page.locator('#boot')).toHaveCount(0);
	await kbd.press('/');
	await expect(bar).toBeFocused();
	await page.keyboard.type('2160p');
	const list = page.getByRole('listbox', { name: 'Suggestions' });
	await expect(list).toBeVisible();
	await expect(bar).toHaveAttribute('aria-expanded', 'true');
	await expect(list.getByRole('option')).toHaveCount(2);
	await expect(list.getByRole('option').first()).toContainText('1,284');
	await expectNoA11yViolations(page);

	await kbd.press('ArrowDown');
	await kbd.press('ArrowDown');
	await expect(bar).toHaveAttribute(
		'aria-activedescendant',
		(await list.getByRole('option').nth(1).getAttribute('id'))!
	);
	await kbd.press('ArrowUp');
	await kbd.press('Tab');
	await expect(list).toHaveCount(0);
	await expect(bar).toHaveText('res:2160p');
	await expect(bar).toBeFocused();
	await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBe('res:2160p');
	await expect(
		page.getByRole('button', { name: 'Remove filter: Resolution: 2160p' })
	).toBeVisible();
});

test('Esc closes the suggestions and keeps the text', async ({ page, go, kbd }) => {
	await go('/search');
	const bar = page.getByRole('combobox', { name: 'Search torrents' });
	await expect(page.locator('#boot')).toHaveCount(0); // the app has started
	await kbd.press('/');
	await expect(bar).toBeFocused();
	await page.keyboard.type('2160p');
	await expect(page.getByRole('listbox', { name: 'Suggestions' })).toBeVisible();
	await kbd.press('Escape');
	await expect(page.getByRole('listbox')).toHaveCount(0);
	await expect(bar).toHaveText('2160p');
	await expect(bar).toHaveAttribute('aria-expanded', 'false');
});

test('natural phrasing previews chips and applies only on Apply', async ({ page, go, kbd }) => {
	await go('/search');
	const bar = page.getByRole('combobox', { name: 'Search torrents' });
	await expect(page.locator('#boot')).toHaveCount(0); // the app has started
	await kbd.press('/');
	await expect(bar).toBeFocused();
	await page.keyboard.type('4k movies');
	await kbd.tabTo(page.getByRole('button', { name: 'Natural phrasing' }));
	await kbd.press('Enter');
	const preview = page.getByRole('region', { name: 'Natural phrasing preview' });
	await expect(preview.getByText('res:2160p')).toBeVisible();
	await expect(preview.getByRole('button', { name: 'Apply' })).toBeFocused();
	await expect(bar).toHaveText('4k movies'); // nothing changed yet
	await expectNoA11yViolations(page);
	await kbd.press('Enter');
	await expect(preview).toHaveCount(0);
	await expect(bar).toHaveText('res:2160p movies');
	await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBe('res:2160p movies');
});

test('Builder: a row edit updates the bar, and typing updates the rows', async ({
	page,
	go,
	kbd
}) => {
	await go('/search?q=bunny+res%3A720p');
	const bar = page.getByRole('combobox', { name: 'Search torrents' });
	await expect(page.getByRole('button', { name: 'Remove filter: Resolution: 720p' })).toBeVisible();
	await kbd.press('/');
	await expect(bar).toBeFocused();
	await kbd.tabTo(page.getByRole('button', { name: 'Builder' }));
	await kbd.press('Enter');
	const builder = page.getByRole('region', { name: 'Query builder' });
	const row = builder.getByRole('group', { name: 'Resolution: 720p' });
	const value = row.getByLabel('Value');
	await expect(value).toHaveValue('720p');
	await expectNoA11yViolations(page);

	await value.focus();
	await page.keyboard.press('Control+a');
	await page.keyboard.type('1080p');
	await page.keyboard.press('Enter'); // commits (the control's change event)
	await page.keyboard.press('Tab');
	await expect(bar).toHaveText('bunny res:1080p');
	await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBe('bunny res:1080p');

	await bar.focus();
	await page.keyboard.press('End');
	await page.keyboard.type(' res:2160p');
	await expect(builder.getByLabel('Value')).toHaveCount(3); // two rows and the new-filter row
});
