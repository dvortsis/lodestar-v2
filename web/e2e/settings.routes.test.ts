// The Settings shell and its generated screens (S-settings-1, §10.10) against the OpenAPI mock,
// the settings API answered from B2's golden index: every screen of this slice and an interim one,
// edit, save and Undo, "save only sends changed fields", an env-locked field, the leave guard
// between screens, a draft after a reload, a deep link into Advanced, the layouts at 390, 768 and
// 1366, and axe on each screen and the Overview in Riverstone light and dark.
import type { Page } from '@playwright/test';
import { expectNoA11yViolations } from './a11y';
import { expect, test } from './support/test';
import { SettingsBackend } from '../src/lib/features/settings/form/testing/e2e-backend';
import { goldenIndex, goldenValues } from '../src/lib/features/settings/shell/golden';

const nav = (page: Page) => page.getByRole('navigation', { name: 'Settings sections' });
const h1 = (page: Page, name: string) => page.getByRole('heading', { level: 1, name });

function backendWith(over: (v: ReturnType<typeof goldenValues>) => void = () => {}) {
	const values = goldenValues();
	over(values);
	return new SettingsBackend(goldenIndex(), values);
}

test.describe('screens', () => {
	test.use({ viewport: { width: 1366, height: 900 } });
	let backend: SettingsBackend;
	test.beforeEach(async ({ mock }) => {
		backend = backendWith().attach(mock);
	});

	for (const [id, heading] of [
		['crawler', 'Crawler'],
		['network', 'Network & VPN'],
		['storage', 'Storage & retention'],
		['search', 'Search & ranking'],
		['general', 'General'],
		['logging', 'Logging'],
		['preferences', 'Preferences'],
		['content', 'Content']
	] as const) {
		test(`opens ${id}`, async ({ page, go }) => {
			await go(`/settings/${id}`);
			await expect(h1(page, heading)).toBeVisible();
			await expect(h1(page, heading)).toBeFocused();
			await expect(nav(page).getByRole('link', { name: heading, exact: true })).toHaveAttribute(
				'aria-current',
				'page'
			);
		});
	}

	test('shows an unknown id as the 404', async ({ page, go }) => {
		await go('/settings/nonsense');
		await expect(page.getByText(/not found/i).first()).toBeVisible();
	});

	test('sends /settings/backup and /settings/import to General', async ({ page, go }) => {
		await go('/settings/backup');
		await expect(page).toHaveURL(/\/settings\/general#backup$/);
		await expect(h1(page, 'General')).toBeVisible();
	});

	test('edits, saves and undoes on Crawler', async ({ page, go }) => {
		await go('/settings/crawler');
		await page.getByRole('radio', { name: 'Aggressive', exact: true }).check();
		const bar = page.getByTestId('save-bar');
		await expect(bar).toContainText('1 unsaved change');
		await bar.getByRole('button', { name: 'Save changes' }).click();
		await expect(bar).toContainText('Saved');
		expect(backend.patches).toEqual([{ crawler: { if_revision: 1, intensity: 'aggressive' } }]);
		await bar.getByRole('button', { name: 'Undo' }).click();
		await expect.poll(() => backend.patches.length).toBe(2);
		expect(backend.patches[1]).toEqual({ crawler: { if_revision: 2, intensity: null } });
	});

	test('re-saving one Network field sends only that field', async ({ page, go }) => {
		await go('/settings/network');
		const port = page.getByLabel('DHT port', { exact: true });
		await port.fill('7000');
		await page.getByRole('button', { name: 'Save changes' }).click();
		const dialog = page.getByRole('dialog', { name: 'Apply this DHT change?' });
		await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused();
		await dialog.getByRole('button', { name: 'Save changes' }).click();
		await expect.poll(() => backend.patches.length).toBe(1);
		expect(backend.patches[0]).toEqual({ network: { if_revision: 1, dht_port: 7000 } });
		await expect(page.getByTestId('save-bar')).toContainText('Saved');
		await expect(dialog).toHaveCount(0);
		// Saving again after another edit still sends nothing but the edited field.
		await port.fill('7001');
		await page.getByRole('button', { name: 'Save changes' }).click();
		await dialog.getByRole('button', { name: 'Save changes' }).click();
		await expect.poll(() => backend.patches.length).toBe(2);
		expect(Object.keys(backend.patches[1]!.network!).sort()).toEqual(['dht_port', 'if_revision']);
	});

	test('names the new address in the lockout dialog', async ({ page, go }) => {
		await go('/settings/network');
		await page.getByLabel('Web address', { exact: true }).fill('0.0.0.0:9000');
		await page.getByRole('button', { name: 'Save changes' }).click();
		const dialog = page.getByRole('dialog', { name: 'Change the web address?' });
		await expect(dialog).toContainText('After a restart, Lodestar is at http://192.168.1.5:9000/.');
		await expect(dialog).toContainText('confirm within 5 minutes');
		await dialog.getByRole('button', { name: 'Cancel' }).click();
		expect(backend.patches).toHaveLength(0);
	});

	test('guards leaving for another screen with unsaved changes', async ({ page, go }) => {
		await go('/settings/crawler');
		await page.getByRole('radio', { name: 'Aggressive', exact: true }).check();
		await nav(page).getByRole('link', { name: 'Storage & retention' }).click();
		const dialog = page.getByRole('dialog', { name: 'Leave without saving?' });
		await expect(dialog).toBeVisible();
		await dialog.getByRole('button', { name: 'Keep editing' }).click();
		await expect(page).toHaveURL(/\/settings\/crawler(\?.*)?$/);
		await nav(page).getByRole('link', { name: 'Storage & retention' }).click();
		await dialog.getByRole('button', { name: 'Discard' }).click();
		await expect(h1(page, 'Storage & retention')).toBeVisible();
		expect(backend.patches).toHaveLength(0);
	});

	test('restores a draft after a reload', async ({ page, go }) => {
		await go('/settings/crawler');
		await page.getByRole('radio', { name: 'Low', exact: true }).check();
		await expect
			.poll(
				() => page.evaluate(() => sessionStorage.getItem('lodestar.settings.draft.v1.crawler')),
				{ timeout: 10_000 }
			)
			.toContain('low');
		page.on('dialog', (d) => void d.accept());
		await page.reload();
		await expect(page.getByText('You have unsaved changes from earlier')).toBeVisible();
		await page.getByRole('button', { name: 'Restore' }).click();
		await expect(page.getByRole('radio', { name: 'Low', exact: true })).toBeChecked();
	});

	test('opens Advanced for a deep link and focuses the field', async ({ page, go }) => {
		await page.goto('/settings/crawler#crawler.timeout_secs'); // `go` drops the hash
		const field = page.getByLabel('Fetch timeout', { exact: true });
		await expect(field).toBeFocused();
		await expect(page.getByTestId('advanced-fetching')).toHaveAttribute('open', '');
		// Not remembered: a visit without the hash starts closed again.
		await go('/settings/crawler');
		await expect(page.getByTestId('advanced-fetching')).not.toHaveAttribute('open');
	});

	test('remembers an Advanced the user opens', async ({ page, go }) => {
		await go('/settings/crawler');
		await page.getByText('Advanced (3)').click();
		await expect(page.getByTestId('advanced-fetching')).toHaveAttribute('open', '');
		await page.reload();
		await expect(page.getByTestId('advanced-fetching')).toHaveAttribute('open', '');
	});

	test('shows only changed fields with Show changed only', async ({ page, go }) => {
		await go('/settings/crawler?changed=1');
		await expect(page.getByTestId('settings-nothing-changed')).toBeVisible();
		await page.getByTestId('settings-changed-only').uncheck();
		await expect(page).not.toHaveURL(/changed=1/);
		await expect(page.locator('[data-field="crawler.intensity"]')).toBeVisible();
	});
});

test.describe('an environment-locked field', () => {
	test.use({ viewport: { width: 1366, height: 900 } });
	test('shows Set by its variable and cannot be edited', async ({ page, go, mock }) => {
		backendWith((values) => {
			const port = values.find((v) => v.section === 'network')!.fields.dht_port!;
			Object.assign(port, { source: 'env', env: 'LODESTAR_DHT_PORT', read_only: true });
		}).attach(mock);
		await go('/settings/network');
		const field = page.locator('[data-field="network.dht_port"]');
		await expect(field.getByTestId('status-env')).toContainText('Set by LODESTAR_DHT_PORT');
		await expect(field.getByLabel('DHT port', { exact: true })).toBeDisabled();
	});
});

test.describe('layout', () => {
	test.beforeEach(async ({ mock }) => void backendWith().attach(mock));

	for (const width of [390, 768]) {
		test(`takes turns, list then detail, at ${width}`, async ({ page, go }) => {
			await page.setViewportSize({ width, height: 900 });
			await go('/settings');
			await expect(h1(page, 'Settings')).toBeVisible();
			await expect(nav(page)).toHaveCount(1);
			await nav(page).getByRole('link', { name: 'Network & VPN' }).click();
			await expect(h1(page, 'Network & VPN')).toBeVisible();
			await expect(nav(page)).toHaveCount(0);
			await page.getByTestId('settings-back').click();
			await expect(h1(page, 'Settings')).toBeVisible();
			await expect(nav(page).getByRole('link', { name: 'Crawler' })).toBeVisible();
		});
	}

	test('shows two panes at 1366, the navigation staying while a screen opens', async ({
		page,
		go
	}) => {
		await page.setViewportSize({ width: 1366, height: 900 });
		await go('/settings/crawler');
		await expect(nav(page)).toBeVisible();
		await expect(page.getByTestId('settings-back')).toHaveCount(0);
		const before = await nav(page).boundingBox();
		await nav(page).getByRole('link', { name: 'Storage & retention' }).click();
		await expect(h1(page, 'Storage & retention')).toBeVisible();
		expect(await nav(page).boundingBox()).toEqual(before);
	});
});

for (const mode of ['light', 'dark'] as const) {
	for (const [width, screens] of [
		[
			1366,
			[
				'/settings',
				'/settings/crawler',
				'/settings/network',
				'/settings/storage',
				'/settings/search',
				'/settings/general',
				'/settings/logging',
				'/settings/preferences'
			]
		],
		[390, ['/settings', '/settings/crawler', '/settings/network']]
	] as const) {
		test.describe(`Riverstone ${mode} at ${width}`, () => {
			test.use({
				themeId: `riverstone-${mode}`,
				colorMode: mode,
				viewport: { width, height: 900 }
			});
			for (const path of screens) {
				test(`is axe-clean: ${path}`, async ({ page, go, mock }) => {
					backendWith().attach(mock);
					await go(path);
					await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
					await expect(page.getByText('Loading settings…')).toHaveCount(0);
					if (path !== '/settings')
						await expect(page.locator('[data-field]').first()).toBeVisible();
					await expectNoA11yViolations(page);
				});
			}
		});
	}
}
