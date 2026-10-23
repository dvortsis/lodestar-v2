// Keyboard-only flows on the Settings shell (S-settings-1), no pointer: reach each screen from the
// navigation, open Advanced, edit and save with Ctrl+S. Search and Restart now are added with their
// parts.
import { expect, test } from './support/test';
import { SettingsBackend } from '../src/lib/features/settings/form/testing/e2e-backend';
import { SystemBackend } from '../src/lib/features/settings/shell/e2e-system';
import { goldenIndex, goldenValues } from '../src/lib/features/settings/shell/golden';

test.use({ viewport: { width: 1366, height: 900 } });

test.describe('keyboard: settings', () => {
	let system: SystemBackend;
	let backend: SettingsBackend;
	test.beforeEach(async ({ mock }) => {
		backend = new SettingsBackend(goldenIndex(), goldenValues()).attach(mock);
		system = new SystemBackend({ restart_required: ['crawler.encryption'] }).attach(mock);
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

	test('restarts through its dialog', async ({ page, go, kbd }) => {
		await page.route('**/healthz', (route) => route.fulfill({ status: 200, body: 'ok' }));
		await go('/settings/crawler');
		await kbd.tabTo(page.getByTestId('settings-restart-now'), 80);
		await kbd.press('Enter');
		const dialog = page.getByRole('dialog', { name: 'Restart Lodestar?' });
		await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused();
		await kbd.press('Tab');
		await expect(dialog.getByRole('button', { name: 'Restart' })).toBeFocused();
		await kbd.press('Enter');
		await expect.poll(() => system.posts).toEqual(['restart']);
		await expect(page.getByTestId('settings-restart-banner')).toHaveCount(0, { timeout: 15_000 });
	});

	test('searches with / and chooses a result', async ({ page, go, kbd }) => {
		await go('/settings/storage');
		await expect(
			page.getByRole('heading', { level: 1, name: 'Storage & retention' })
		).toBeFocused();
		await kbd.press('/');
		const box = page.getByRole('combobox', { name: 'Search settings' });
		await expect(box).toBeFocused();
		await page.keyboard.type('fetch timeout');
		await expect(page.getByRole('option').first()).toContainText('Fetch timeout');
		await kbd.press('Enter');
		await expect(page.getByLabel('Fetch timeout', { exact: true })).toBeFocused();
	});

	test('does not take / while typing in a field', async ({ page, go, kbd }) => {
		await go('/settings/network');
		await kbd.tabTo(page.getByLabel('Web address', { exact: true }), 80);
		await page.keyboard.type('/');
		await expect(page.getByLabel('Web address', { exact: true })).toBeFocused();
		await expect(page.getByRole('combobox', { name: 'Search settings' })).not.toBeFocused();
	});
});
