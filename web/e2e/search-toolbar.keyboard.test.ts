// S-search-3 keyboard-only: "Skip to results" past the bar and the facet rail, and the toolbar's
// density from the keys. No pointer events are allowed (the kbd fixture fails the test).
import type { Page } from '@playwright/test';
import { body, expect, hits, row, rowOf, server } from './search-results.fixtures';
import { interact } from './support/interact';
import { KeyboardOnly } from './support/keyboard';
import { test } from './support/test';

test.beforeEach(({ mock }) => server(mock, () => body(hits(5))));

test('Skip to results shows on focus and lands on the list’s active row', async ({
	page,
	go,
	kbd
}) => {
	await page.setViewportSize({ width: 1366, height: 900 });
	await go('/search?q=lantern');
	await expect(rowOf(page, 1)).toBeVisible();
	const skip = page.getByRole('link', { name: 'Skip to results' });
	await kbd.tabTo(skip);
	await expect(skip).toBeVisible();
	await kbd.press('Enter');
	await expect(rowOf(page, 1)).toBeFocused();
	await kbd.press('j');
	await expect(rowOf(page, 2)).toBeFocused();
	// From the bar, it's one Shift+Tab back.
	await kbd.press('/');
	await expect(page.getByRole('combobox', { name: 'Search torrents' })).toBeFocused();
	await kbd.press('Shift+Tab');
	await expect(skip).toBeFocused();
	await kbd.press('Enter');
	await expect(rowOf(page, 2)).toBeFocused();
});

test('with no list yet, Skip to results goes to the results region', async ({ page, go, kbd }) => {
	await go('/search');
	await interact(page);
	await expect(page.getByTestId('results-empty')).toBeVisible();
	await kbd.tabTo(page.getByRole('link', { name: 'Skip to results' }));
	await kbd.press('Enter');
	await expect(page.locator('#results')).toBeFocused();
});

test('the view from the keys: the select changes the rows', async ({ page, go, kbd }) => {
	await go('/search?q=lantern');
	await expect(rowOf(page, 1)).toBeVisible();
	const view = page.getByTestId('view-select');
	await kbd.tabTo(view);
	// Type-ahead picks the option by its first letters, with no popup to open (a native select).
	await kbd.press('c');
	await expect(view).toHaveValue('compact');
	await expect(page).toHaveURL(/density=compact/);
	await expect(row(page, 1)).toHaveAttribute('data-density', 'compact');
});

test('at 390 px with no rows, Skip to results moves focus to the results region', async ({
	page,
	go,
	mock,
	kbd
}) => {
	server(mock, () => ({
		status: 503,
		body: { error: { code: 'unavailable', message: 'down' } }
	}));
	await page.setViewportSize({ width: 390, height: 800 });
	await go('/search?q=lantern');
	await expect(page.getByTestId('results-error')).toBeVisible();
	const skip = page.getByTestId('skip-to-results');
	await kbd.tabTo(skip);
	await kbd.press('Enter');
	await expect(page.locator('#results')).toBeFocused();
});

// X-skip-results-flake: keys pressed after the first one but before the shell's key listener
// attaches (it waits on the chrome's lazy chunks) are kept and handed to the shortcuts in order,
// never twice. The chunk that starts the shortcuts is held until the test lets it go, so the keys
// land in that window every time.
/** The chunk that starts the shortcuts. The chrome's own chunk names `startShortcuts` too (it calls
 *  it), so the defaults' `help.shortcuts` tells the two apart: the chrome mounts, its shortcuts wait. */
const shortcutsBoot = (text: string) =>
	text.includes('startShortcuts') && text.includes('help.shortcuts');

async function holdShortcuts(page: Page, also?: string) {
	let release!: () => void;
	const gate = new Promise<void>((resolve) => (release = resolve));
	let held = 0;
	await page.route('**/_app/immutable/**/*.js', async (route) => {
		const response = await route.fetch();
		const text = await response.text();
		if (shortcutsBoot(text) || (also !== undefined && text.includes(also))) {
			held += 1;
			await gate;
		}
		await route.fulfill({ response, body: text });
	});
	return { release, held: () => held };
}

/** The real status pill (not the bar's "Connecting" stand-in) arrives in the same step that attaches
 *  the listener and replays the kept keys; only it has the live announcement. */
const attached = (page: Page) => expect(page.getByTestId('status-pill-announce')).toBeAttached();

const focused = (page: Page) =>
	page.evaluate(() => document.activeElement?.outerHTML.slice(0, 120) ?? '');

test.describe('keys pressed before the key listener attaches', () => {
	// The held route's handlers may still be fetching when the page closes.
	test.afterEach(({ page }) => page.unrouteAll({ behavior: 'ignoreErrors' }));

	async function skipToRows(
		page: Page,
		kbd: Pick<KeyboardOnly, 'tabTo' | 'press'>,
		hold: { held: () => number }
	) {
		await page.setViewportSize({ width: 1366, height: 900 });
		await expect(rowOf(page, 1)).toBeVisible();
		await kbd.tabTo(page.getByRole('link', { name: 'Skip to results' }));
		// The first key started the chrome, whose shortcuts are now held; the list's own keys are in.
		await expect.poll(hold.held).toBeGreaterThan(0);
		await expect(page.locator('[data-keys-ready]')).toBeAttached();
		await expect(page.getByTestId('status-pill-announce')).toHaveCount(0);
		await kbd.press('Enter');
		await expect(rowOf(page, 1)).toBeFocused();
	}

	test('j right after Skip to results still moves to the next row', async ({ page, go, kbd }) => {
		const hold = await holdShortcuts(page);
		await go('/search?q=lantern');
		await skipToRows(page, kbd, hold);
		await kbd.press('j');
		await expect(rowOf(page, 1)).toBeFocused();
		hold.release();
		await expect(rowOf(page, 2)).toBeFocused();
	});

	test('a key pressed before Tab is not replayed over it', async ({ page, go, kbd }) => {
		const hold = await holdShortcuts(page);
		await go('/search?q=lantern');
		await skipToRows(page, kbd, hold);
		// x selects the active row; Tab, which the browser acts on at once, comes after it.
		await kbd.press('x');
		await kbd.press('Tab');
		await expect(rowOf(page, 1)).not.toBeFocused();
		const after = await focused(page);
		hold.release();
		await attached(page);
		// A live x toggles the row: selected once if the kept x stayed dropped, back to none if it
		// had been replayed.
		await kbd.press('x');
		await expect(page.getByTestId('bulk-count')).toContainText('1');
		expect(await focused(page)).toBe(after);
	});

	test('letters after a replayed / are typing, not shortcuts', async ({ page, go, kbd }) => {
		const hold = await holdShortcuts(page);
		await go('/search?q=lantern');
		await expect(rowOf(page, 1)).toBeVisible();
		const box = page.getByRole('combobox', { name: 'Search torrents' });
		await kbd.press('/');
		await expect.poll(hold.held).toBeGreaterThan(0);
		// j moves through the rows and x selects one: neither may run once `/` has put focus in the box.
		await kbd.press('j');
		await kbd.press('x');
		hold.release();
		await attached(page);
		await expect(box).toBeFocused();
		// A live key still types into the box: a replayed j would have moved focus to the rows.
		await kbd.press('z');
		await expect(box).toHaveText('lanternz');
		await expect(box).toBeFocused();
		await expect(page.getByTestId('bulk-bar')).toHaveCount(0);
	});

	test('a key kept longer than 5 s is dropped', async ({ page, go, kbd }) => {
		const hold = await holdShortcuts(page);
		await go('/search?q=lantern');
		await skipToRows(page, kbd, hold);
		await page.evaluate(() =>
			addEventListener(
				'keydown',
				(e) => ((window as { lastKey?: number } & Window).lastKey = e.timeStamp),
				true
			)
		);
		await kbd.press('j');
		// Held until the press is older than the 5 s the chrome keeps a press for (the same clock it
		// reads): a condition on the press's age, not a fixed wait.
		await expect
			.poll(
				() =>
					page.evaluate(
						() =>
							performance.now() - ((window as { lastKey?: number } & Window).lastKey ?? Infinity)
					),
				{ timeout: 15_000 }
			)
			.toBeGreaterThan(5_200);
		hold.release();
		await attached(page);
		// A live j moves one row: to row 2 if the old j was dropped, to row 3 if it had run.
		await kbd.press('j');
		await expect(rowOf(page, 2)).toBeFocused();
	});

	test('Enter on a button acts once', async ({ page, go, kbd }) => {
		const hold = await holdShortcuts(page);
		await page.setViewportSize({ width: 1366, height: 900 });
		await go('/search?q=lantern&view=table');
		await expect(page.getByTestId('results-table')).toBeVisible();
		const columns = page.getByTestId('table-columns');
		await kbd.tabTo(columns);
		await expect.poll(hold.held).toBeGreaterThan(0);
		await kbd.press('Enter');
		await expect(page.getByTestId('table-chooser')).toBeVisible();
		hold.release();
		await attached(page);
		// A second press would close it again.
		await expect(columns).toHaveAttribute('aria-expanded', 'true');
		await expect(page.getByTestId('table-chooser')).toBeVisible();
	});

	test('text typed into the search box (contenteditable) appears once', async ({
		page,
		go,
		kbd
	}) => {
		const hold = await holdShortcuts(page);
		await go('/search');
		const box = page.getByRole('combobox', { name: 'Search torrents' });
		await expect(box).toBeVisible();
		await kbd.tabTo(box);
		await expect.poll(hold.held).toBeGreaterThan(0);
		await kbd.press('j');
		await kbd.press('k');
		await kbd.press('x');
		await expect(box).toHaveText('jkx');
		hold.release();
		await attached(page);
		await expect(box).toHaveText('jkx');
		await expect(box).toBeFocused();
	});

	test('the very first key still finds its shortcut', async ({ page, go, kbd }) => {
		const hold = await holdShortcuts(page);
		await go('/search?q=lantern');
		await expect(rowOf(page, 1)).toBeVisible();
		await kbd.press('/');
		await expect.poll(hold.held).toBeGreaterThan(0);
		hold.release();
		await expect(page.getByRole('combobox', { name: 'Search torrents' })).toBeFocused();
	});
	test('x before a key the list handled is not replayed over it', async ({ page, go, kbd }) => {
		const hold = await holdShortcuts(page);
		await go('/search?q=lantern');
		await skipToRows(page, kbd, hold);
		// Home on the first row is the list's own (default-prevented) and leaves focus where it is.
		await kbd.press('x');
		await kbd.press('Home');
		hold.release();
		await attached(page);
		// A live x selects the row once if the kept x stayed dropped, none if it had been replayed.
		await kbd.press('x');
		await expect(page.getByTestId('bulk-count')).toContainText('1');
	});

	test('x before Enter on a button is not replayed over it', async ({ page, go, kbd }) => {
		const hold = await holdShortcuts(page);
		await page.setViewportSize({ width: 1366, height: 900 });
		await go('/search?q=lantern&view=table');
		await expect(page.getByTestId('results-table')).toBeVisible();
		const columns = page.getByTestId('table-columns');
		await kbd.tabTo(columns);
		await expect.poll(hold.held).toBeGreaterThan(0);
		await kbd.press('x');
		await kbd.press('Enter');
		await expect(page.getByTestId('table-chooser')).toBeVisible();
		hold.release();
		await attached(page);
		await kbd.press('Escape');
		await expect(columns).toBeFocused();
		await kbd.press('x');
		await expect(page.getByTestId('bulk-count')).toContainText('1');
	});

	test('keys after Ctrl/Cmd+K were for the palette, not the rows', async ({ page, go, kbd }) => {
		// The palette's own chunk is held too, so x lands on the row, not in the palette's field.
		const hold = await holdShortcuts(page, 'palette-input');
		await go('/search?q=lantern');
		await skipToRows(page, kbd, hold);
		await kbd.press('ControlOrMeta+k');
		await kbd.press('x');
		hold.release();
		await attached(page);
		await expect(page.getByTestId('palette-input')).toBeFocused();
		await kbd.press('Escape');
		await expect(page.getByTestId('palette-input')).toHaveCount(0);
		await kbd.press('x');
		await expect(page.getByTestId('bulk-count')).toContainText('1');
	});

	test('a key before a click is not replayed over it', async ({ page, go }) => {
		// Pointer test: the keyboard-only guard is left out, the keys go through the same helper.
		const keys = new KeyboardOnly(page);
		const hold = await holdShortcuts(page);
		await go('/search?q=lantern');
		await skipToRows(page, keys, hold);
		await keys.press('x');
		await page.getByTestId('results-count').click();
		hold.release();
		await attached(page);
		await keys.press('x');
		await expect(page.getByTestId('bulk-count')).toContainText('1');
	});

	test('the latest 32 presses are kept', async ({ page, go, kbd }) => {
		const hold = await holdShortcuts(page);
		await go('/search?q=lantern');
		await skipToRows(page, kbd, hold);
		// F2 does nothing here; 33 of them, then j, keep j only if the newest presses are kept.
		for (let i = 0; i < 33; i++) await kbd.press('F2');
		await kbd.press('j');
		hold.release();
		await expect(rowOf(page, 2)).toBeFocused();
	});

	test('a shortcuts chunk that fails to load stops the shell keeping presses', async ({
		page,
		go,
		kbd
	}) => {
		let failed = 0;
		await page.route('**/_app/immutable/**/*.js', async (route) => {
			const response = await route.fetch();
			const text = await response.text();
			if (shortcutsBoot(text)) {
				failed += 1;
				return route.abort();
			}
			await route.fulfill({ response, body: text });
		});
		await go('/search?q=lantern');
		await expect(rowOf(page, 1)).toBeVisible();
		await kbd.press('Tab');
		await expect.poll(() => failed).toBeGreaterThan(0);
		// Until then the shell takes Ctrl/Cmd+K from the browser; afterwards it lets it through.
		const taken = () =>
			page.evaluate(() => {
				const e = new KeyboardEvent('keydown', {
					key: 'k',
					ctrlKey: true,
					metaKey: true,
					bubbles: true,
					cancelable: true
				});
				document.body.dispatchEvent(e);
				return e.defaultPrevented;
			});
		await expect.poll(taken).toBe(false);
	});
});
