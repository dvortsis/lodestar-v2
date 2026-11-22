// Finding a setting (X-settings-find, §10.10.3) against the OpenAPI mock, by keyboard alone, with
// axe: `/` focuses the box, results come from every field's label, help and keys with a breadcrumb
// and the current value, Enter opens the screen, reveals Advanced, focuses the field and outlines
// it; the command palette offers the same fields; and on a phone the
// Overview reads search, attention, then the list of screens.
import type { Page } from '@playwright/test';
import { expectNoA11yViolations } from './a11y';
import { liveEvents, openPaletteWithKey } from './support/events-server';
import { expect as baseExpect, test } from './support/test';
import {
	FIXTURE_INDEX,
	SettingsBackend,
	fixtureValues,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';

const expect = baseExpect.configure({ timeout: 10_000 });

// The `/` key and the search load once the page is idle: press it until the box takes focus.
async function focusBoxWithSlash(page: Page, kbd: { press: (key: string) => Promise<void> }) {
	await expect(page.getByRole('searchbox')).toBeVisible();
	await expect
		.poll(async () => {
			await kbd.press('/');
			return page.evaluate(() => document.activeElement?.tagName === 'INPUT');
		})
		.toBe(true);
}

test.describe('keyboard: Settings search', () => {
	test.beforeEach(async ({ mock, page }) => {
		await liveEvents(page);
		const values = fixtureValues();
		values.find((s) => s.section === 'secrets')!.fields.token!.value = 'hunter2-never-shown';
		new SettingsBackend(FIXTURE_INDEX, values).attach(mock as unknown as MockLike);
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		on('getSystem', () => ({
			status: 200,
			body: {
				address_change: null,
				data: { filesystem_warning: null },
				data_formats: { index: 'v3-t2', rebuild: null, store_schema: 12 },
				diagnostics: { changed: [], logging: null, recent_errors: 0 },
				restart_required: []
			}
		}));
		on('getNetworkCheck', () => ({ status: 200, body: { reachable: true } }));
	});

	test.use({ viewport: { width: 1440, height: 900 } });

	test('/ focuses the box; Enter opens the field, focuses it and outlines it', async ({
		page,
		go,
		kbd
	}) => {
		await go('/settings');
		await focusBoxWithSlash(page, kbd);
		const box = page.getByRole('combobox');
		await expect(box).toBeFocused();
		await page.keyboard.type('pause');
		const options = page.getByRole('option');
		await expect(options.first()).toContainText('Pause crawling');
		await expect(options.first()).toContainText('Crawling › Crawler');
		// The current value and where it comes from.
		await expect(options.first()).toContainText('·');
		await expectNoA11yViolations(page);
		await kbd.press('Enter');
		await expect(page).toHaveURL(/\/settings\/crawler#crawler\.paused$/);
		const field = page.locator('[data-field="crawler.paused"]');
		await expect(field.getByRole('switch')).toBeFocused();
		await expect(field).toHaveCSS('outline-style', 'solid');
	});

	test('finds a field by its help or config key, expands Advanced, and clears with Escape', async ({
		page,
		go,
		kbd
	}) => {
		await go('/settings');
		await focusBoxWithSlash(page, kbd);
		await page.keyboard.type('timeout');
		const first = page.getByRole('option').first();
		await expect(first).toContainText('Fetch timeout');
		await expect(first).toContainText('Advanced');
		await kbd.press('ArrowDown');
		await kbd.press('ArrowUp');
		await kbd.press('Enter');
		await expect(page).toHaveURL(/\/settings\/crawler#crawler\.timeout_secs$/);
		const field = page.locator('[data-field="crawler.timeout_secs"]');
		await expect(field).toBeVisible();
		await expect(field.getByRole('textbox', { name: 'Fetch timeout' })).toBeFocused();

		await go('/settings');
		await focusBoxWithSlash(page, kbd);
		await page.keyboard.type('zzzz-nothing');
		await expect(page.getByRole('status').filter({ hasText: 'No setting matches' })).toBeVisible();
		await kbd.press('Escape');
		await expect(page.getByRole('combobox')).toHaveValue('');
		await expect(page.getByRole('navigation', { name: 'Settings sections' })).toBeVisible();
	});

	test('the command palette offers the same fields and opens one', async ({ page, go, kbd }) => {
		await go('/settings');
		await expect(page.getByTestId('status-pill')).toHaveAttribute('data-state', 'crawling');
		await openPaletteWithKey(page, () => kbd.press('Control+k'));
		await page.keyboard.type('pause crawling');
		const option = page
			.getByTestId('palette-option')
			.filter({ hasText: 'Settings: Pause crawling' });
		await expect(option).toBeVisible();
		await expectNoA11yViolations(page);
		await expect(async () => {
			if ((await option.getAttribute('aria-selected')) !== 'true') await kbd.press('ArrowDown');
			await expect(option).toHaveAttribute('aria-selected', 'true', { timeout: 500 });
		}).toPass({ timeout: 10_000 });
		await kbd.press('Enter');
		await expect(page).toHaveURL(/\/settings\/crawler#crawler\.paused$/);
		await expect(page.locator('[data-field="crawler.paused"]').getByRole('switch')).toBeFocused();
	});

	test('the current item is shaded, not only bold', async ({ page, go }) => {
		await go('/settings/crawler');
		const current = page.getByRole('link', { name: 'Crawler', exact: true });
		await expect(current).toHaveAttribute('aria-current', 'page');
		await expect(current).not.toHaveCSS('background-color', 'rgba(0, 0, 0, 0)');
	});

	test.describe('on a phone', () => {
		test.use({ viewport: { width: 390, height: 844 } });
		test('the Overview reads search, what needs attention, Start here, then the screens', async ({
			page,
			go
		}) => {
			await go('/settings');
			const box = await page.getByRole('searchbox').boundingBox();
			const attention = await page.getByRole('heading', { name: 'Needs attention' }).boundingBox();
			const start = await page.getByRole('heading', { name: 'Start here' }).boundingBox();
			const list = await page.getByRole('navigation', { name: 'Settings sections' }).boundingBox();
			expect(box!.y).toBeLessThan(attention!.y);
			expect(attention!.y).toBeLessThan(start!.y);
			expect(start!.y).toBeLessThan(list!.y);
			await expect(page.getByTestId('start-row').first()).toBeVisible();
			await expectNoA11yViolations(page);
		});
	});
});
