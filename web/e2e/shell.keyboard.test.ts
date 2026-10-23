// The shell with no pointer (architecture §10.13, §10.16): the skip link, the top bar, the menu
// button, the palette and the overflow menu are all reachable and operable from the keyboard.
import { liveEvents, openPaletteWithKey } from './support/events-server';
import { expect, test } from './support/test';

// The shell registers its shortcuts and opens its stream when the browser is idle (≤ 1 s after load).
const ready = (page: import('@playwright/test').Page) =>
	expect(page.getByTestId('status-pill')).toHaveAttribute('data-state', 'crawling');

test.beforeEach(async ({ page }) => {
	await liveEvents(page);
});

test('Tab reaches the skip link first, then the logo and the five destinations in order', async ({
	page,
	go,
	kbd
}) => {
	await go('/search');
	await ready(page);
	const names: string[] = [];
	for (let i = 0; i < 7; i++) {
		await kbd.press('Tab');
		names.push(
			await page.evaluate(
				() =>
					document.activeElement?.getAttribute('aria-label') ??
					document.activeElement?.textContent?.trim() ??
					''
			)
		);
	}
	expect(names).toEqual([
		'Skip to main content',
		'Lodestar, home',
		'Search',
		'Discover',
		'Alerts',
		'Insights',
		'Settings'
	]);
});

test('every stop on the top bar shows a visible focus ring', async ({ page, go, kbd }) => {
	await go('/search');
	await ready(page);
	// skip link, logo, five destinations, status pill, palette button, overflow button
	for (let i = 0; i < 10; i++) {
		await kbd.press('Tab');
		const ring = await page.evaluate(() => {
			const el = document.activeElement as HTMLElement;
			const s = getComputedStyle(el);
			return {
				visible: el.matches(':focus-visible'),
				width: parseFloat(s.outlineWidth),
				style: s.outlineStyle
			};
		});
		expect(ring.visible).toBe(true);
		expect(ring.style).not.toBe('none');
		expect(ring.width).toBeGreaterThan(0);
	}
});

test('Enter on a destination navigates, marks it current and moves focus to main', async ({
	page,
	go,
	kbd,
	mock
}) => {
	mock.on('listSavedSearches', []);
	await go('/search');
	await kbd.tabTo(page.getByRole('link', { name: 'Alerts' }));
	await kbd.press('Enter');
	await expect(page).toHaveURL(/\/alerts/);
	await expect(page.getByRole('link', { name: 'Alerts' })).toHaveAttribute('aria-current', 'page');
	await expect(page.locator('main')).toBeFocused();
});

test('Ctrl+K opens the palette, typing filters, Enter runs, and the page changes', async ({
	page,
	go,
	kbd
}) => {
	await go('/search');
	await ready(page);
	await openPaletteWithKey(page, () => kbd.press('Control+k'));
	const input = page.getByRole('combobox', { name: 'Type a command' });
	await expect(input).toBeFocused();
	await page.keyboard.type('disc');
	await expect(page.getByRole('option')).toHaveCount(1);
	await kbd.press('Enter');
	await expect(page).toHaveURL(/\/discover/);
	await expect(page.getByRole('dialog')).toBeHidden();
});

test('arrow keys move through the palette and Escape closes it, returning focus', async ({
	page,
	go,
	kbd
}) => {
	await go('/search');
	await ready(page);
	const first = page.getByRole('link', { name: 'Search', exact: true });
	await kbd.tabTo(first);
	await openPaletteWithKey(page, () => kbd.press('Control+k'));
	const input = page.getByRole('combobox', { name: 'Type a command' });
	await expect(input).toHaveAttribute('aria-activedescendant', 'palette-option-0');
	await kbd.press('ArrowDown');
	await expect(input).toHaveAttribute('aria-activedescendant', 'palette-option-1');
	await expect(page.getByRole('option').nth(1)).toHaveAttribute('aria-selected', 'true');
	await kbd.press('Escape');
	await expect(page.getByRole('dialog')).toBeHidden();
	await expect(first).toBeFocused();
});

test('the overflow menu opens with Enter, moves with arrows and closes with Escape', async ({
	page,
	go,
	kbd
}) => {
	await go('/search');
	await kbd.tabTo(page.getByTestId('more-button'));
	await kbd.press('Enter');
	await expect(page.getByRole('menu', { name: 'More' })).toBeVisible();
	await kbd.press('ArrowDown');
	await kbd.press('Escape');
	await expect(page.getByRole('menu')).toBeHidden();
	await expect(page.getByTestId('more-button')).toBeFocused();
});

test.describe('narrow screens', () => {
	test.use({ viewport: { width: 390, height: 800 } });

	test('the menu button opens the destinations, Escape closes them and returns focus', async ({
		page,
		go,
		kbd
	}) => {
		await go('/search');
		const button = page.getByTestId('menu-button');
		await kbd.tabTo(button);
		await kbd.press('Enter');
		await expect(button).toHaveAttribute('aria-expanded', 'true');
		await kbd.press('Tab'); // the logo comes after the button in the bar; the panel is after the bar
		await kbd.tabTo(page.getByRole('link', { name: 'Insights', exact: true }));
		await kbd.press('Enter');
		await expect(page).toHaveURL(/\/insights/);
		await expect(page.getByRole('navigation', { name: 'Main' })).toHaveCount(0);

		await kbd.tabTo(button);
		await kbd.press('Enter');
		await kbd.press('Escape');
		await expect(button).toHaveAttribute('aria-expanded', 'false');
		await expect(button).toBeFocused();
	});
});
