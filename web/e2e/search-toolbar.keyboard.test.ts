// S-search-3 keyboard-only: "Skip to results" past the bar and the facet rail, and the toolbar's
// density from the keys. No pointer events are allowed (the kbd fixture fails the test).
import type { Page } from '@playwright/test';
import { body, expect, hits, row, rowOf, server } from './search-results.fixtures';
import { interact } from './support/interact';
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
async function holdShortcuts(page: Page) {
	let release!: () => void;
	const gate = new Promise<void>((resolve) => (release = resolve));
	let held = 0;
	await page.route('**/_app/immutable/**/*.js', async (route) => {
		const response = await route.fetch();
		const text = await response.text();
		if (text.includes('startShortcuts')) {
			held += 1;
			await gate;
		}
		await route.fulfill({ response, body: text });
	});
	return { release, held: () => held };
}

test.describe('keys pressed before the key listener attaches', () => {
	// The held route's handlers may still be fetching when the page closes.
	test.afterEach(({ page }) => page.unrouteAll({ behavior: 'ignoreErrors' }));

	test('j right after Skip to results still moves to the next row', async ({ page, go, kbd }) => {
		const hold = await holdShortcuts(page);
		await page.setViewportSize({ width: 1366, height: 900 });
		await go('/search?q=lantern');
		await expect(rowOf(page, 1)).toBeVisible();
		const skip = page.getByRole('link', { name: 'Skip to results' });
		await kbd.tabTo(skip);
		// The first key started the chrome, whose shortcuts are now held; the list's own keys are in.
		await expect.poll(hold.held).toBeGreaterThan(0);
		await expect(page.locator('[data-keys-ready]')).toBeAttached();
		await kbd.press('Enter');
		await expect(rowOf(page, 1)).toBeFocused();
		await kbd.press('j');
		await expect(rowOf(page, 1)).toBeFocused();
		hold.release();
		await expect(rowOf(page, 2)).toBeFocused();
		// The Enter on the link was the browser's; it isn't run again as a shortcut (it would open
		// the detail drawer on the active row).
		await expect(page.getByRole('dialog')).toHaveCount(0);
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
		// A second press would close it again: it stays open once the shortcuts arrive.
		await expect(page.locator('[data-keys-ready]')).toBeAttached();
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
		await expect(page.locator('[data-keys-ready]')).toBeAttached();
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
});
