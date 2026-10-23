// Keyboard-only flows on the Settings shell (S-settings-1), no pointer: reach each screen from the
// navigation, open Advanced, edit and save with Ctrl+S. Search and Restart now are added with their
// parts.
import { expect, test } from './support/test';
import { SettingsBackend } from '../src/lib/features/settings/form/testing/e2e-backend';
import { goldenIndex, goldenValues } from '../src/lib/features/settings/shell/golden';

test.use({ viewport: { width: 1366, height: 900 } });

test.describe('keyboard: settings', () => {
	let backend: SettingsBackend;
	test.beforeEach(async ({ mock }) => {
		backend = new SettingsBackend(goldenIndex(), goldenValues()).attach(mock);
	});

	test('reaches every screen from the navigation', async ({ page, go, kbd }) => {
		await go('/settings/crawler');
		const nav = page.getByRole('navigation', { name: 'Settings sections' });
		for (const [link, heading] of [
			['Network & VPN', 'Network & VPN'],
			['Storage & retention', 'Storage & retention'],
			['Search & ranking', 'Search & ranking'],
			['General', 'General']
		]) {
			await kbd.tabTo(nav.getByRole('link', { name: link, exact: true }), 80);
			await kbd.press('Enter');
			const h1 = page.getByRole('heading', { level: 1, name: heading });
			await expect(h1).toBeVisible();
			await expect(h1).toBeFocused();
		}
	});

	test('opens Advanced, edits a field and saves with Ctrl+S', async ({ page, go, kbd }) => {
		await go('/settings/crawler');
		await expect(page.locator('[data-field="crawler.intensity"]')).toBeVisible();
		await kbd.tabTo(page.getByText('Advanced (3)'), 80);
		await kbd.press('Enter');
		await kbd.tabTo(page.getByLabel('Fetch timeout', { exact: true }), 20);
		await page.keyboard.type('40');
		await kbd.press('Control+s');
		await expect(page.getByTestId('save-bar')).toContainText('Saved');
		expect(backend.patches.at(-1)).toEqual({ crawler: { if_revision: 1, timeout_secs: 40 } });
	});

	test('goes back to the list by keyboard on a phone', async ({ page, go, kbd }) => {
		await page.setViewportSize({ width: 390, height: 800 });
		await go('/settings/network');
		await kbd.tabTo(page.getByTestId('settings-back'), 30);
		await kbd.press('Enter');
		await expect(page.getByRole('heading', { level: 1, name: 'Settings' })).toBeFocused();
	});
});
