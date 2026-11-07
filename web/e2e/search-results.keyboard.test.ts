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
import { expectNoA11yViolations } from './a11y';
import { test } from './support/test';
import type { Page } from '@playwright/test';
import type { KeyboardOnly } from './support/keyboard';

// The list's j/k/x and c/o commands load lazily and a key pressed before they register is dropped, so wait
// for the list to say they are in. Then focus the first row and wait until it really has focus.
async function focusFirstRow(page: Page, kbd: KeyboardOnly) {
	await expect(page.locator('[data-keys-ready]')).toBeAttached();
	await kbd.tabTo(rowOf(page, 1));
	await expect(rowOf(page, 1)).toBeFocused();
}

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
	const bar = page.getByRole('combobox', { name: 'Search torrents' });
	await expect(bar).toBeFocused();
	await page.keyboard.type('lantern');
	await expect(bar).toHaveText('lantern'); // typing has settled before Enter submits it
	// The suggestions open only once the lazy autocomplete has mounted; Enter then closes them.
	await expect(bar).toHaveAttribute('aria-expanded', 'true');
	await kbd.press('Enter');
	await expect(page).toHaveURL(/[?&]q=lantern/);
	await expect(rowOf(page, 1)).toBeVisible();
	// Suggestions closed: a Tab with them showing would accept one instead of leaving the bar.
	await expect(bar).toHaveAttribute('aria-expanded', 'false');
	// Shortcuts don't fire in the bar (§10.16): Tab to the list, which is one tab stop.
	await focusFirstRow(page, kbd);
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
	await focusFirstRow(page, kbd);
	await kbd.press('Shift+F10');
	const item = page.getByRole('menuitem', { name: 'Copy info hash' });
	await expect(item).toBeVisible();
	// Opened from the keyboard, the menu lands on its first item (Open in a new tab).
	await expect(page.getByRole('menuitem', { name: 'Open in a new tab' })).toBeFocused();
	await kbd.press('ArrowDown');
	await expect(item).toBeFocused();
	await kbd.press('Enter');
	await expect.poll(() => copied(page)).toBe(hashOf(1));
});

// No `kbd` here: this one needs a mouse click after a keyboard open.
test('a pointer open of the More menu after a keyboard open does not focus the first item', async ({
	page,
	go
}) => {
	await go('/search?q=lantern');
	await expect(rowOf(page, 1)).toBeVisible();
	await rowOf(page, 1).focus();
	await page.keyboard.press('Shift+F10');
	const first = page.getByRole('menuitem', { name: 'Open in a new tab' });
	await expect(first).toBeFocused();
	await page.keyboard.press('Escape');
	await expect(page.getByRole('menu')).toHaveCount(0);
	await rowOf(page, 1).hover();
	await rowOf(page, 1).locator('[data-row-more]').click();
	// Focus has settled in the menu (on the menu itself), so the negative check below cannot pass early.
	await expect(page.getByRole('menu', { name: /More actions/ })).toBeFocused();
	await expect(first).not.toBeFocused();
});

test('c copies the focused row’s magnet with the drawer closed', async ({ page, go, kbd }) => {
	await recordClipboard(page);
	await go('/search?q=lantern');
	await expect(rowOf(page, 1)).toBeVisible();
	await focusFirstRow(page, kbd);
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
	await focusFirstRow(page, kbd);
	await kbd.press('c');
	await expect(page.getByTestId('detail-magnet-field')).toHaveValue(
		`magnet:?xt=urn:btih:${hashOf(1)}`
	);
});

test('search, open, send: the drawer’s primary action and `s` send to the client', async ({
	page,
	go,
	kbd,
	mock
}) => {
	const sent: unknown[] = [];
	mock.on('getSettings', {
		section: 'integrations',
		revision: 1,
		restart_required: [],
		fields: {
			clients: { value: [{ id: 'q', name: 'qBittorrent' }], source: 'db', read_only: false },
			default_client: { value: '', source: 'default', read_only: false }
		}
	} as never);
	mock.on('sendToClient', (request) => {
		const { hashes } = request.postDataJSON() as { hashes: string[] };
		sent.push(hashes);
		return {
			client: 'q',
			sent: 1,
			results: hashes.map((hash) => ({ hash, outcome: 'sent' }))
		} as never;
	});
	await page.setViewportSize({ width: 1440, height: 900 });
	await go('/search?q=lantern');
	await expect(rowOf(page, 1)).toBeVisible();
	await focusFirstRow(page, kbd);
	await kbd.press('Enter');
	const send = page.getByTestId('detail-send');
	await expect(send).toHaveText('Send to qBittorrent');
	await expectNoA11yViolations(page);
	await kbd.press('s');
	await expect(page.getByText('Sent to qBittorrent')).toBeVisible();
	expect(sent).toEqual([[hashOf(1)]]);
});
