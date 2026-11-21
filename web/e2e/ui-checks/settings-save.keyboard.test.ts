// UI check (review 03, flow-settings-save.mjs): gap-fill on the real /settings/crawler route,
// complementing settings-form.keyboard.test.ts (which runs on /dev/settings-form). Keyboard only:
// an unsaved change guards navigation, Esc keeps editing, Save changes saves and leaves, and a
// field's menu returns focus to its button on Esc. Waits are conditions with a 10 s deadline.
import type { Locator } from '@playwright/test';
import type { KeyboardOnly } from '../support/keyboard';
import { SettingsBackend } from '../../src/lib/features/settings/form/testing/e2e-backend';
import { expect as baseExpect, test } from '../support/test';

const expect = baseExpect.configure({ timeout: 10_000 });

/** The section list sits before the form in tab order, so a link in it is reached backwards. */
async function shiftTabTo(kbd: KeyboardOnly, target: Locator, max = 80): Promise<void> {
	for (let i = 0; i < max; i++) {
		await kbd.press('Shift+Tab');
		if (await target.evaluate((el) => el === document.activeElement)) return;
	}
	throw new Error(`keyboard-only: ${target} not reached within ${max} Shift+Tab presses`);
}

test.describe('keyboard: settings save on /settings/crawler', () => {
	test.setTimeout(90_000); // long Tab runs through the shell and the section list
	let backend: SettingsBackend;
	test.beforeEach(async ({ page, go, mock }) => {
		backend = new SettingsBackend().attach(mock);
		await go('/settings/crawler');
		await expect(
			page.locator('[data-field="crawler.intensity"]').getByRole('radiogroup')
		).toBeVisible();
	});

	test('Esc on the leave dialog keeps editing, Save changes saves and leaves', async ({
		page,
		kbd
	}) => {
		await kbd.tabTo(page.getByRole('radio', { name: 'balanced' }), 80);
		await kbd.press('ArrowUp');
		await expect(page.getByRole('radio', { name: 'low' })).toBeChecked();
		const leave = page.getByRole('navigation', { name: 'Settings sections' }).getByRole('link', {
			name: 'Appearance'
		});
		await shiftTabTo(kbd, leave);
		await kbd.press('Enter');
		const dialog = page.getByRole('dialog', { name: 'Leave without saving?' });
		await expect(dialog.getByRole('button', { name: 'Save changes' })).toBeFocused();
		await kbd.press('Escape');
		await expect(dialog).toHaveCount(0);
		await expect(page).toHaveURL(/\/settings\/crawler/);
		await expect(page.getByRole('radio', { name: 'low' })).toBeChecked();
		await expect(leave).toBeFocused();
		expect(backend.patches).toHaveLength(0);

		await shiftTabTo(kbd, leave);
		await kbd.press('Enter');
		await expect(dialog.getByRole('button', { name: 'Save changes' })).toBeFocused();
		await kbd.press('Enter');
		await expect(page).toHaveURL(/\/settings\/appearance/);
		expect(backend.patches).toEqual([{ crawler: { if_revision: 3, intensity: 'low' } }]);
	});

	test('a field menu opens from the keyboard and Esc returns focus to its button', async ({
		page,
		kbd
	}) => {
		const field = page.locator('[data-field="crawler.intensity"]');
		const button = field.locator('button[aria-haspopup="menu"]');
		await kbd.tabTo(button, 80);
		await kbd.press('Enter');
		await expect(page.getByRole('menu')).toBeVisible();
		await kbd.press('Escape');
		await expect(page.getByRole('menu')).toHaveCount(0);
		await expect(button).toBeFocused();
	});
});
