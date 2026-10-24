// S-search-2 keyboard-only: the results list is one tab stop; j/k walk it, Enter opens the drawer,
// and `c` copies the focused row's magnet with the drawer closed, falling back to the manual field
// when the clipboard is refused. No pointer events are allowed (the kbd fixture fails the test).
import {
	body,
	copied,
	expect,
	hashOf,
	hits,
	recordClipboard,
	rowOf,
	server
} from './search-results.fixtures';
import { test } from './support/test';

test.beforeEach(({ mock }) => server(mock, () => body(hits(5))));

test('/ then a query, Enter, Tab, j and k, Enter opens; j moves the open drawer; Esc comes back', async ({
	page,
	go,
	kbd
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	await go('/search');
	await expect(page.locator('#boot')).toHaveCount(0); // the app has started (W9's hand-over)
	await kbd.press('/');
	await expect(page.getByRole('combobox', { name: 'Search torrents' })).toBeFocused();
	await page.keyboard.type('lantern');
	await kbd.press('Enter');
	await expect(rowOf(page, 1)).toBeVisible();
	// Shortcuts don't fire in the bar (§10.16): Tab to the list, which is one tab stop.
	await kbd.tabTo(rowOf(page, 1));
	await kbd.press('j');
	await expect(rowOf(page, 2)).toBeFocused();
	await kbd.press('j');
	await expect(rowOf(page, 3)).toBeFocused();
	await kbd.press('k');
	await expect(rowOf(page, 2)).toBeFocused();
	await kbd.press('Enter');
	await expect(page.getByTestId('drawer')).toBeVisible();
	await expect(page).toHaveURL(new RegExp(`/t/${hashOf(2)}`));
	// §10.5: with the drawer open, j and k move it to the next torrent.
	await kbd.press('j');
	await expect(page).toHaveURL(new RegExp(`/t/${hashOf(3)}`));
	await expect(page.getByTestId('drawer')).toBeVisible();
	await kbd.press('Escape');
	await expect(page.getByTestId('drawer')).toHaveCount(0);
	await expect(rowOf(page, 3)).toBeFocused();
});

test('Shift+F10 opens the row’s More menu; Copy info hash from the keyboard', async ({
	page,
	go,
	kbd
}) => {
	await recordClipboard(page);
	await go('/search?q=lantern');
	await expect(rowOf(page, 1)).toBeVisible();
	await kbd.tabTo(rowOf(page, 1));
	await kbd.press('Shift+F10');
	const item = page.getByRole('menuitem', { name: 'Copy info hash' });
	await expect(item).toBeVisible();
	await kbd.press('ArrowDown'); // Open in a new tab
	await kbd.press('ArrowDown');
	await expect(item).toBeFocused();
	await kbd.press('Enter');
	await expect.poll(() => copied(page)).toBe(hashOf(1));
});

test('c copies the focused row’s magnet with the drawer closed', async ({ page, go, kbd }) => {
	await recordClipboard(page);
	await go('/search?q=lantern');
	await expect(rowOf(page, 1)).toBeVisible();
	await kbd.tabTo(rowOf(page, 1));
	await kbd.press('j');
	await expect(rowOf(page, 2)).toBeFocused();
	await kbd.press('c');
	await expect(page.getByText('Magnet copied')).toBeVisible();
	expect(await copied(page)).toBe(`magnet:?xt=urn:btih:${hashOf(2)}`);
	await expect(page.getByTestId('drawer')).toHaveCount(0);
});

test('c without the clipboard shows the magnet to copy by hand', async ({ page, go, kbd }) => {
	await page.addInitScript(() => {
		// Both ways to copy refuse: the Clipboard API and the textarea fallback.
		Object.defineProperty(navigator, 'clipboard', {
			value: { writeText: () => Promise.reject(new DOMException('denied', 'NotAllowedError')) }
		});
		document.execCommand = () => false;
	});
	await go('/search?q=lantern');
	await expect(rowOf(page, 1)).toBeVisible();
	await kbd.tabTo(rowOf(page, 1));
	await kbd.press('c');
	await expect(page.getByTestId('detail-magnet-field')).toHaveValue(
		`magnet:?xt=urn:btih:${hashOf(1)}`
	);
});
