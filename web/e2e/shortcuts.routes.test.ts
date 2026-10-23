// The shortcut sheet (`?`) and the Keyboard screen on their test page (/dev/shortcuts): opening,
// closing, focus and axe, light and dark, desktop and phone. Keyboard-only flows are in
// shortcuts.keyboard.test.ts. Name ends in `routes.test.ts` so the `core` project runs it.
import type { Page } from '@playwright/test';
import { expectNoA11yViolations } from './a11y';
import { liveEvents } from './support/events-server';
import { expect, test } from './support/test';

const section = (shortcuts = true, shortcut_keys: Record<string, string> = {}) => ({
	section: 'preferences',
	revision: 3,
	restart_required: [],
	fields: {
		shortcuts: {
			value: shortcuts,
			env: null,
			read_only: false,
			restart_required: false,
			source: 'db' as const
		},
		shortcut_keys: {
			value: shortcut_keys,
			env: null,
			read_only: false,
			restart_required: false,
			source: 'db' as const
		}
	}
});

// axe reads colours mid-fade otherwise.
const settled = (page: Page) =>
	page.evaluate(() => Promise.all(document.getAnimations().map((a) => a.finished)));

// The shell's shortcuts exist a moment after the first interaction: press until the sheet is open.
async function openSheet(page: Page) {
	const dialog = page.getByRole('dialog', { name: 'Keyboard shortcuts' });
	await expect(async () => {
		if (!(await dialog.isVisible())) await page.keyboard.press('?');
		await expect(dialog).toBeVisible({ timeout: 800 });
	}).toPass({ timeout: 10_000 });
	return dialog;
}

test.beforeEach(async ({ page, mock }) => {
	await liveEvents(page);
	mock.on('getSettings', section());
});

test('? opens the sheet, which lists the palette and the sheet itself; Esc closes it and returns focus', async ({
	page,
	go
}) => {
	await go('/search');
	const home = page.getByRole('link', { name: 'Lodestar, home' });
	await home.focus();
	const dialog = await openSheet(page);
	await expect(dialog.getByText('Ctrl+K').first()).toBeAttached();
	await expect(dialog.getByText('Question mark').first()).toBeAttached();
	await expect(dialog.getByRole('link', { name: 'Change shortcuts' })).toHaveAttribute(
		'href',
		'/settings/keyboard'
	);
	await page.keyboard.press('Escape');
	await expect(dialog).toBeHidden();
	await expect(home).toBeFocused();
});

test('the sheet does not stack on a second ?', async ({ page, go }) => {
	await go('/search');
	const dialog = await openSheet(page);
	await page.keyboard.press('?');
	await expect(page.getByRole('dialog')).toHaveCount(1);
	await expect(dialog).toBeVisible();
});

for (const [themeId, colorMode] of [
	['riverstone-light', 'light'],
	['riverstone-dark', 'dark']
] as const) {
	for (const [name, viewport] of [
		['1366', { width: 1366, height: 768 }],
		['390', { width: 390, height: 844 }]
	] as const) {
		test.describe(`axe, ${themeId}, ${name}`, () => {
			test.use({ themeId, colorMode, viewport });

			test('the sheet', async ({ page, go }) => {
				await go('/search');
				await openSheet(page);
				await settled(page);
				await expectNoA11yViolations(page);
			});

			test('the Keyboard screen', async ({ page, go }) => {
				await go('/dev/shortcuts');
				await expect(page.getByRole('switch', { name: 'Single-key shortcuts' })).toBeVisible();
				await expectNoA11yViolations(page);
			});

			test('the Keyboard screen while capturing and on a conflict', async ({ page, go }) => {
				await go('/dev/shortcuts');
				await page.getByRole('button', { name: 'Change the key for Next result' }).click();
				await expect(page.getByTestId('shortcut-key-results.next')).toContainText('Press a key');
				await page.keyboard.press('c');
				await expect(page.getByText('Already used by Copy magnet')).toBeVisible();
				await expectNoA11yViolations(page);
			});
		});
	}
}
