// "Removing X gives N" and the index-state banners (S-search-recover-ui; architecture §10.4 States),
// against the mock: the server's `recovery` and `/search/state`. Name ends in `routes.test.ts` so
// the `core` project runs it.
import { expectNoA11yViolations } from './a11y';
import {
	body,
	expect,
	hits,
	indexState,
	resultsCalls,
	server,
	toAst
} from './search-results.fixtures';
import { interact } from './support/interact';
import { test } from './support/test';

const recovery = {
	skipped: 0,
	clauses: [
		{
			canonical: 'lantern',
			clause: 'ext:mkv',
			index: 1,
			total: 1_240,
			total_approx: false,
			without: toAst('lantern')
		},
		{
			canonical: 'ext:mkv',
			clause: 'lantern',
			index: 0,
			total_approx: false,
			without: toAst('ext:mkv')
		},
		{
			canonical: 'x',
			clause: 'seeders:>=9',
			index: 2,
			total: 0,
			total_approx: false,
			without: toAst('x')
		}
	]
};

test('zero-result recovery applies a counted suggestion from the keyboard', async ({
	page,
	go,
	mock
}) => {
	server(mock, (sent) =>
		JSON.stringify(sent.ast).includes('"ext"') ? body([], { recovery }) : body(hits(2))
	);
	mock.on('searchIndexState', indexState(5_000_000));
	await go('/search?q=lantern+ext%3Amkv');
	const list = page.getByTestId('results-recover');
	await expect(list).toBeVisible();
	// Only the clause with a count above 0 is offered.
	await expect(list.getByRole('button')).toHaveCount(1);
	await expect(list.getByRole('button')).toHaveText('Removing ext:mkv gives 1,240');
	await expectNoA11yViolations(page);
	await list.getByRole('button').focus();
	await page.keyboard.press('Enter');
	await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBe('lantern');
	await expect(page.getByTestId('results-none')).toHaveCount(0);
	// The server's reduced query was searched as it came, not re-parsed.
	expect(resultsCalls(mock).at(-1)!.ast).toEqual(toAst('lantern'));
});

test('an estimated count shows with ≈', async ({ page, go, mock }) => {
	const approx = {
		skipped: 0,
		clauses: [{ ...recovery.clauses[0]!, total: 2_000_000, total_approx: true }]
	};
	server(mock, () => body([], { recovery: approx }));
	mock.on('searchIndexState', indexState(5_000_000));
	await go('/search?q=lantern+ext%3Amkv');
	await expect(page.getByTestId('results-recover').getByRole('button')).toHaveText(
		'Removing ext:mkv gives ≈2,000,000'
	);
});

test('the growing banner shows the count and the hourly rate', async ({ page, go, mock }) => {
	server(mock, () => body(hits(2)));
	mock.on('searchIndexState', indexState(84_210, { growing: true, growth_per_hour: 2104.5 }));
	await go('/search?q=lantern');
	await expect(page.getByTestId('results-cold-start')).toHaveText(/84,210 torrents, \+2,105\/h/);
	await expectNoA11yViolations(page);
});

test('the growing banner leaves the rate out when none was measured', async ({
	page,
	go,
	mock
}) => {
	server(mock, () => body(hits(2)));
	mock.on('searchIndexState', indexState(84_210, { growing: true, growth_per_hour: null }));
	await go('/search?q=lantern');
	await expect(page.getByTestId('results-cold-start')).toContainText('84,210 torrents so far');
	await expect(page.getByTestId('results-cold-start')).not.toContainText('/h');
});

test('a cold start fills the page on the bare search', async ({ page, go, mock }) => {
	mock.on('parseQuery', { ast: { op: 'all' }, canonical: '' });
	mock.on('listSavedSearches', []);
	mock.on('searchIndexState', indexState(120, { cold_start: true, growing: true }));
	await go('/search');
	await interact(page);
	await expect(page.getByTestId('results-cold-page')).toBeVisible();
	await expect(page.getByTestId('results-empty')).toHaveCount(0);
	await expectNoA11yViolations(page);
});

test('the rebuilding banner shows progress and the fields that match nothing', async ({
	page,
	go,
	mock
}) => {
	server(mock, () => body(hits(2)));
	mock.on(
		'searchIndexState',
		indexState(5_000_000, {
			growing: true,
			rebuild: {
				phase: 'building',
				done: 1_200,
				total: 4_000,
				started_at: 1_700_000_000,
				from: 'v3-t1',
				to: 'v4-t1',
				unavailable: ['codec'],
				error: null
			}
		})
	);
	await go('/search?q=lantern');
	await expect(page.getByTestId('results-rebuilding')).toContainText('1,200 of 4,000');
	await expect(page.getByTestId('results-unavailable')).toContainText('codec');
	await expect(page.getByTestId('results-cold-start')).toHaveCount(0);
	await expectNoA11yViolations(page);
});
