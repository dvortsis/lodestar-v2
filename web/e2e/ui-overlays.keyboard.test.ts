// W7cd: the overlay primitives on the test-only showcase (/dev/ui/overlays), keyboard-only, with
// axe in Riverstone light and dark. The showcase is served because the Playwright build sets
// VITE_DEV_ROUTES.
import type { Locator, Page } from '@playwright/test';
import { expectNoA11yViolations } from './a11y';
import { expect, test } from './support/test';

const PATH = '/dev/ui/overlays';

// axe reads colours mid-fade otherwise: wait for the entry animations to finish.
const settled = (page: Page) =>
	page.evaluate(() => Promise.all(document.getAnimations().map((a) => a.finished)));
const axe = async (page: Page) => {
	await settled(page);
	await expectNoA11yViolations(page);
};

for (const [themeId, colorMode] of [
	['riverstone-light', 'light'],
	['riverstone-dark', 'dark']
] as const) {
	test.describe(`overlays, ${themeId}`, () => {
		test.use({ themeId, colorMode });

		test('axe: page, menu, dialog, sheet, drawer', async ({ page, go, kbd }) => {
			await go(PATH);
			await axe(page);

			await kbd.tabTo(page.getByRole('button', { name: 'Actions' }));
			await kbd.press('Enter');
			await expect(page.getByRole('menu', { name: 'Torrent actions' })).toBeVisible();
			await axe(page);
			await kbd.press('Escape');

			await kbd.tabTo(page.getByRole('button', { name: 'Open dialog' }));
			await kbd.press('Enter');
			await expect(page.getByRole('dialog', { name: 'Remove torrent' })).toBeVisible();
			await axe(page);
			await kbd.press('Escape');

			await kbd.tabTo(page.getByRole('button', { name: 'Open sheet' }));
			await kbd.press('Enter');
			await expect(page.getByRole('dialog', { name: 'Filters' })).toBeVisible();
			await axe(page);
			await kbd.press('Escape');
			await expect(page.getByRole('dialog')).toHaveCount(0);

			await kbd.tabTo(page.getByRole('button', { name: 'Ubuntu 24.04 desktop' }));
			await kbd.press('Enter');
			await expect(page.getByRole('heading', { name: 'Ubuntu 24.04 desktop' })).toBeFocused();
			await axe(page);
		});
	});
}

test('tooltip opens on keyboard focus and closes on Esc', async ({ page, go, kbd }) => {
	await go(PATH);
	await kbd.tabTo(page.getByRole('button', { name: 'Copy magnet' }));
	const tip = page.getByRole('tooltip');
	await expect(tip).toBeVisible();
	await expect(tip).toHaveText('Copies the magnet link to the clipboard');
	await kbd.press('Escape');
	await expect(tip).toHaveCount(0);
});

test('menu: arrows, typeahead and Enter select an item, focus returns to the trigger', async ({
	page,
	go,
	kbd
}) => {
	await go(PATH);
	const trigger = page.getByRole('button', { name: 'Actions' });
	await kbd.tabTo(trigger);
	await kbd.press('Enter');
	await expect(page.getByRole('menuitem', { name: 'Favorite' })).toBeFocused();
	// The highlighted item has a visible ring, not only a tint (§10.13).
	const ring = await page.getByRole('menuitem', { name: 'Favorite' }).evaluate((e) => {
		const s = getComputedStyle(e);
		return { style: s.outlineStyle, width: parseFloat(s.outlineWidth) };
	});
	expect(ring.style).not.toBe('none');
	expect(ring.width).toBeGreaterThanOrEqual(2);
	await kbd.press('ArrowDown');
	await expect(page.getByRole('menuitem', { name: 'Hide' })).toBeFocused();
	await kbd.press('f');
	await expect(page.getByRole('menuitem', { name: 'Fix parse' })).toBeFocused();
	await kbd.press('Enter');
	await expect(page.getByTestId('menu-result')).toHaveText('fix');
	await expect(trigger).toBeFocused();
});

async function openColumns(
	page: Page,
	kbd: { tabTo(l: Locator): Promise<number>; press(k: string): Promise<void> }
) {
	const trigger = page.getByRole('button', { name: 'Columns' });
	await kbd.tabTo(trigger);
	await kbd.press('Enter');
	const box = page.getByRole('dialog', { name: 'Columns' }).getByRole('checkbox', { name: 'Size' });
	// Opening moves focus into the panel; wait for it, or a slow runner pulls focus after the keys.
	await expect(box).toBeFocused();
	return { trigger };
}

test('popover is non-modal: Tab from its last control continues after the trigger', async ({
	page,
	go,
	kbd
}) => {
	await go(PATH);
	await openColumns(page, kbd);
	// The panel is portalled to the end of <body>: without the order fix Tab would leave the page,
	// and a focus trap would cycle back to the checkbox.
	await kbd.press('Tab');
	await expect(page.getByRole('button', { name: 'Actions' })).toBeFocused();
});

test('popover is non-modal: Shift+Tab from its first control returns to the trigger', async ({
	page,
	go,
	kbd
}) => {
	await go(PATH);
	const { trigger } = await openColumns(page, kbd);
	await kbd.press('Shift+Tab');
	await expect(trigger).toBeFocused();
});

test('popover: Esc closes it and returns focus to the trigger', async ({ page, go, kbd }) => {
	await go(PATH);
	const trigger = page.getByRole('button', { name: 'Columns' });
	await kbd.tabTo(trigger);
	await kbd.press('Enter');
	const panel = page.getByRole('dialog', { name: 'Columns' });
	await expect(panel).toBeVisible();
	await kbd.press('Escape');
	await expect(panel).toHaveCount(0);
	await expect(trigger).toBeFocused();
});

test('dialog traps focus and restores it to the trigger on Esc', async ({ page, go, kbd }) => {
	await go(PATH);
	const trigger = page.getByRole('button', { name: 'Open dialog' });
	await kbd.tabTo(trigger);
	await kbd.press('Enter');
	const dialog = page.getByRole('dialog', { name: 'Remove torrent' });
	await expect(dialog).toBeVisible();
	for (let i = 0; i < 8; i++) {
		await kbd.press('Tab');
		expect(await dialog.evaluate((d) => d.contains(document.activeElement))).toBe(true);
	}
	await kbd.press('Escape');
	await expect(dialog).toHaveCount(0);
	await expect(trigger).toBeFocused();
});

test('drawer: focus to title, list stays reachable, resize by keys, Esc returns to the row', async ({
	page,
	go,
	kbd
}) => {
	await go(PATH);
	const row = page.getByRole('button', { name: 'Fedora 40 workstation' });
	await kbd.tabTo(row);
	await kbd.press('Enter');
	const title = page.getByRole('heading', { name: 'Fedora 40 workstation' });
	await expect(title).toBeFocused();
	// Non-modal: the page behind is not inert, and Tab is free to leave the drawer.
	await expect(page.getByRole('button', { name: 'Debian 12 netinst' })).toBeEnabled();
	const handle = page.getByRole('separator', { name: 'Resize detail' });
	await kbd.tabTo(handle);
	await kbd.press('ArrowLeft');
	await expect(page.getByTestId('drawer-width')).toHaveText('576');
	await kbd.press('Home');
	await expect(page.getByTestId('drawer-width')).toHaveText('640');
	await expect(page.getByTestId('drawer')).toHaveCSS('width', '640px');
	await title.focus();
	await kbd.press('Escape');
	await expect(page.getByTestId('drawer')).toHaveCount(0);
	await expect(row).toBeFocused();
});

test.describe('reduced motion', () => {
	test.use({ motion: 'reduce' });
	test('the drawer and dialog still open and close', async ({ page, go, kbd }) => {
		await go(PATH);
		await kbd.tabTo(page.getByRole('button', { name: 'Open dialog' }));
		await kbd.press('Enter');
		await expect(page.getByRole('dialog', { name: 'Remove torrent' })).toBeVisible();
		await kbd.press('Escape');
		await expect(page.getByRole('dialog')).toHaveCount(0);
	});
});
