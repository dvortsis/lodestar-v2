// The query bar from the keyboard alone (S-search-1; §10.13, §10.16): `/` as the very first key
// focuses the bar (the shell replays it once the key registry has loaded), then type, Enter, and
// Esc clears (undo brings it back). The chips' Tab and Delete flow arrives with the strip.
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
test('/ focuses the bar, Enter searches, Esc clears', async ({ page, go, mock, kbd }) => {
	mock.on('parseQuery', (request: Request) => {
		const q = new URL(request.url()).searchParams.get('q') ?? '';
		return { ast: toAst(q), canonical: q.trim() };
	});
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

	await kbd.press('Escape');
	await expect(bar).toHaveText('');
	await kbd.press('Control+z'); // undo brings it back
	await expect(bar).toHaveText('lantern res:1080p ext:mkv');
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
	await go('/search?q=bunny');
	const bar = page.getByRole('combobox', { name: 'Search torrents' });
	await expect(bar).toHaveText('bunny');
	await kbd.tabTo(page.getByRole('link', { name: 'Discover' })); // focus elsewhere on the page
	await kbd.press('/');
	await expect(bar).toBeFocused();
	await page.keyboard.type('x');
	await expect(bar).toHaveText('bunnyx');
});
