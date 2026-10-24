// The query bar from the keyboard alone (S-search-1; §10.13, §10.16): `/` as the very first key
// focuses the bar (the shell replays it once the key registry has loaded), then type, Enter, Tab
// to a chip, Delete, focus to the next chip and then back to the bar, and Esc clears (undo brings
// it back).
import type { Request } from '@playwright/test';
import type { components } from '../src/lib/api/schema';
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

const print = (ast: Ast): string =>
	(ast.op === 'and' ? ast.of : ast.op === 'all' ? [] : [ast])
		.map((n) =>
			n.op === 'word'
				? n.word
				: n.op === 'tag'
					? `res:${n.any[0]}`
					: n.op === 'ext'
						? `ext:${n.any[0]}`
						: ''
		)
		.join(' ');

test('/ focuses the bar, Enter searches, chips go with Delete, Esc clears', async ({
	page,
	go,
	mock,
	kbd
}) => {
	mock.on('parseQuery', (request: Request) => {
		const q = new URL(request.url()).searchParams.get('q') ?? '';
		return { ast: toAst(q), canonical: q.trim() };
	});
	mock.on('searchPost', (request: Request) => ({
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
	}));
	await go('/search');
	const bar = page.getByRole('combobox', { name: 'Search torrents' });

	await expect(page.locator('#boot')).toHaveCount(0); // the app has started (W9's hand-over)
	await kbd.press('/'); // the first key of the session
	await expect(bar).toBeFocused();
	await page.keyboard.type('lantern res:1080p ext:mkv');
	await expect(bar).toHaveText('lantern res:1080p ext:mkv'); // the "/" was not typed
	await kbd.press('Enter');
	await expect
		.poll(() => new URL(page.url()).searchParams.get('q'))
		.toBe('lantern res:1080p ext:mkv');

	await expect(bar).toBeFocused();
	const first = page.getByRole('button', { name: 'Remove filter: Resolution: 1080p' });
	const second = page.getByRole('button', { name: 'Remove filter: Extension: .mkv' });
	await kbd.tabTo(first);
	await kbd.press('Delete');
	await expect(first).toHaveCount(0);
	await expect(second).toBeFocused(); // the next chip
	await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBe('lantern ext:mkv');
	await kbd.press('Delete');
	await expect(second).toHaveCount(0);
	await expect(bar).toBeFocused(); // no chips left: back to the bar
	await expect(bar).toHaveText('lantern');

	await kbd.press('Escape');
	await expect(bar).toHaveText('');
	await kbd.press('Control+z'); // undo brings it back
	await expect(bar).toHaveText('lantern');
});

test('/ works as the first key after the 30 s fallback loaded the shortcuts', async ({
	page,
	go,
	kbd
}) => {
	await page.clock.install();
	await go('/search');
	await expect(page.locator('#boot')).toHaveCount(0);
	await page.clock.runFor(31_000); // no interaction: the shell's fallback attaches the keys
	await expect(page.getByTestId('palette-button')).toBeAttached();
	await kbd.press('/');
	await expect(page.getByRole('combobox', { name: 'Search torrents' })).toBeFocused();
});

test('/ focuses the bar again from elsewhere on the page, with the caret at the end', async ({
	page,
	go,
	mock,
	kbd
}) => {
	mock.on('parseQuery', { ast: { op: 'word', field: 'any', word: 'bunny' }, canonical: 'bunny' });
	// The results panel (S-search-2) asks for the query's results.
	mock.on('searchPost', {
		canonical: 'bunny',
		hits: [],
		limit: 50,
		next_cursor: null,
		notices: [],
		offset: 0,
		stemmed: false,
		took_ms: 1,
		total: 0,
		total_approx: false
	});
	await go('/search?q=bunny');
	const bar = page.getByRole('combobox', { name: 'Search torrents' });
	await expect(bar).toHaveText('bunny');
	await kbd.tabTo(page.getByRole('link', { name: 'Discover' })); // focus elsewhere on the page
	// The Tab is the first interaction: the shell's key registry attaches a moment after it, and
	// nothing on the page marks that. So press `/` until it reaches the registry (a press before
	// does nothing on a link), and never again once the bar has focus.
	await expect(async () => {
		if (!(await bar.evaluate((el) => el === document.activeElement))) await kbd.press('/');
		await expect(bar).toBeFocused({ timeout: 1_000 });
	}).toPass({ timeout: 10_000 });
	await page.keyboard.type('x');
	await expect(bar).toHaveText('bunnyx');
});
