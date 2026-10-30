// Keyboard-only flows on the Settings form engine (M-settings-form), no pointer: edit fields,
// save with Ctrl+S, follow the error summary's links, resolve a conflict with Keep mine, and
// answer the leave dialog.
import { expect, test } from './support/test';
import { SettingsBackend } from '../src/lib/features/settings/form/testing/e2e-backend';

const PAGE = '/dev/settings-form';

test.describe('keyboard: settings form', () => {
	let backend: SettingsBackend;
	test.beforeEach(async ({ page, go, mock }) => {
		backend = new SettingsBackend().attach(mock);
		await go(PAGE);
		await expect(
			page.locator('[data-field="crawler.intensity"]').getByRole('radiogroup')
		).toBeVisible();
	});

	test('edits a field and saves with Ctrl+S', async ({ page, kbd }) => {
		await kbd.tabTo(page.getByRole('radio', { name: 'balanced' }));
		await kbd.press('ArrowDown'); // radios move with the arrows
		await expect(page.getByRole('radio', { name: 'aggressive' })).toBeChecked();
		await kbd.tabTo(page.getByLabel('Web address', { exact: true }));
		await kbd.press('Control+s');
		await expect(page.getByTestId('save-bar')).toContainText('Saved');
		expect(backend.patches).toEqual([{ crawler: { if_revision: 3, intensity: 'aggressive' } }]);
	});

	test('follows the error summary to the field', async ({ page, kbd }) => {
		await kbd.tabTo(page.getByText('Advanced (1)'));
		await kbd.press('Enter');
		await kbd.tabTo(page.getByLabel('Fetch timeout', { exact: true }));
		await page.keyboard.type('0'); // Tab selected the value: now 0, out of bounds
		await kbd.press('Control+s');
		const summary = page.getByTestId('error-summary');
		await expect(summary.getByRole('heading')).toBeFocused();
		await kbd.press('Tab');
		await expect(summary.getByRole('link')).toBeFocused();
		await kbd.press('Enter');
		await expect(page.getByLabel('Fetch timeout', { exact: true })).toBeFocused();
		expect(backend.patches).toHaveLength(0);
	});

	test('keeps mine in a conflict', async ({ page, kbd }) => {
		await kbd.tabTo(page.getByRole('radio', { name: 'balanced' }));
		await kbd.press('ArrowUp');
		await expect(page.getByRole('radio', { name: 'low' })).toBeChecked();
		backend.change('crawler', { intensity: { value: 'aggressive', source: 'db' } });
		await kbd.press('Control+s');
		await kbd.tabTo(page.getByRole('button', { name: 'Keep mine' }));
		await kbd.press('Enter');
		await kbd.press('Control+s');
		await expect(page.getByTestId('save-bar')).toContainText('Saved');
		expect(backend.patches.at(-1)).toEqual({ crawler: { if_revision: 4, intensity: 'low' } });
	});

	test('answers the leave dialog', async ({ page, kbd }) => {
		await kbd.tabTo(page.getByRole('radio', { name: 'balanced' }));
		await kbd.press('ArrowUp');
		await kbd.tabTo(page.getByRole('link', { name: 'Another page' }), 80);
		await kbd.press('Enter');
		const dialog = page.getByRole('dialog', { name: 'Leave without saving?' });
		await expect(dialog.getByRole('button', { name: 'Save changes' })).toBeFocused();
		await kbd.press('Enter');
		await expect(page).toHaveURL(/\/dev\/ui\/controls/);
		expect(backend.patches).toEqual([{ crawler: { if_revision: 3, intensity: 'low' } }]);
	});
});
