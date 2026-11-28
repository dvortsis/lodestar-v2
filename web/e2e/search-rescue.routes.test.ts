// Zero-result rescue (X-zero-rescue; §10.4 "No results", U8, U9): a filter typed as words offers
// "Did you mean", words that read as filters offer their chips, and each is one explicit click.
// Name ends in `routes.test.ts` so the `core` project runs it.
import { body, expect, hits, row, server, toAst } from './search-results.fixtures';
import type { Request } from '@playwright/test';
import { expectNoA11yViolations } from './a11y';
import { test } from './support/test';

type Operator = 'eq' | 'gt' | 'gte' | 'lt' | 'lte';
const field = (name: string, operators: Operator[]) => ({
	name,
	aliases: [],
	facetable: false,
	type: 'number' as const,
	operators
});

function setUp(mock: Parameters<typeof server>[0]) {
	// Rows for any query except the filter written as words, and a bare "nothing".
	server(mock, (sent) => {
		const text = JSON.stringify(sent.ast);
		return text.includes('seeders>5') || text.includes('nothing')
			? body([])
			: body(hits(3), { total: 3 });
	});
	mock.on('search_fields', { fields: [field('seeders', ['eq', 'gt', 'gte', 'lt', 'lte'])] });
	const parse = (request: Request) => {
		const url = new URL(request.url());
		const q = url.searchParams.get('q') ?? '';
		const base = { ast: toAst(q), canonical: q.trim().replace(/\s+/g, ' ') };
		if (url.searchParams.get('natural') !== 'true' || !q.includes('4k')) return base;
		return {
			...base,
			natural: {
				ast: toAst('res:2160p'),
				canonical: 'nothing res:2160p',
				text: ['nothing'],
				chips: [{ text: 'res:2160p', from: { start: 8, end: 10 }, to: { start: 8, end: 10 } }]
			}
		};
	};
	mock.on('parseQuery', parse as never);
}

test('a filter typed as words offers "Did you mean", and one click searches it', async ({
	page,
	go,
	mock
}) => {
	setUp(mock);
	await go('/search?q=seeders%3E5');
	const none = page.getByTestId('results-none');
	await expect(none).toBeVisible();
	const offer = page.getByTestId('rescue-didyoumean');
	await expect(offer).toHaveText('Did you mean seeders:>5?');
	// It only offers: the zero-result words advice for text is not shown, and nothing was re-run.
	await expect(none).not.toContainText('Try fewer');
	expect(new URL(page.url()).searchParams.get('q')).toBe('seeders>5');
	await expectNoA11yViolations(page, { include: ['#results'] });
	await offer.click();
	await expect(row(page, 1)).toBeVisible();
	await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBe('seeders:>5');
});

test('words that read as filters offer their chips on zero results, with Apply', async ({
	page,
	go,
	mock
}) => {
	setUp(mock);
	await go('/search?q=nothing%204k');
	const chips = page.getByTestId('rescue-natural');
	await expect(chips).toContainText('res:2160p');
	await expectNoA11yViolations(page, { include: ['#results'] });
	await expect(page.getByTestId('rescue-didyoumean')).toHaveCount(0);
	await chips.getByRole('button', { name: 'Apply filters' }).click();
	await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBe('nothing res:2160p');
});

test('nothing is offered when the words are plain and no filter was meant', async ({
	page,
	go,
	mock
}) => {
	setUp(mock);
	// The rescue has asked and been answered before anything is asserted absent.
	const answered = page.waitForResponse(
		(r) => r.url().includes('/parse') && r.url().includes('natural=true')
	);
	await go('/search?q=nothing');
	await answered;
	await expect(page.getByTestId('results-none')).toBeVisible();
	await expect(page.getByTestId('results-none')).toContainText('Try fewer');
	await expect(page.getByTestId('rescue-didyoumean')).toHaveCount(0);
	await expect(page.getByTestId('rescue-natural')).toHaveCount(0);
});
