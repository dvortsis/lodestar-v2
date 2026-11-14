// Settings → Sources' layout (X-sources-restructure, owner §2.66) against the OpenAPI mock: the
// filter box, a row that expands, and a switch whose consent dialog opens exactly as before, at
// 1366 and 390 px; axe at 390, 1366 and 1440 px. Waits are conditions only.
import { expectNoA11yViolations } from './a11y';
import { expect as baseExpect, test } from './support/test';
import {
	SettingsBackend,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';
import { darknet, dht, lime, nyaa, prowlarr } from '../src/lib/features/sources/fixtures';

const expect = baseExpect.configure({ timeout: 10_000 });

test.describe('Settings → Sources layout', () => {
	let sent: unknown[];
	test.beforeEach(async ({ mock }) => {
		new SettingsBackend().attach(mock as unknown as MockLike);
		sent = [];
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		on('listSources', () => ({
			status: 200,
			body: { sources: [dht, nyaa, prowlarr, lime, darknet] }
		}));
		on('patchSource', (req: { postDataJSON(): unknown }) => {
			sent.push(req.postDataJSON());
			return { status: 500, body: { error: { code: 'unexpected', message: 'no' } } };
		});
	});

	for (const width of [1366, 390]) {
		test(`filter, expand a row and open a consent dialog at ${width} px`, async ({ page, go }) => {
			await page.setViewportSize({ width, height: 900 });
			await go('/settings/sources');
			const filter = page.getByRole('searchbox', { name: 'Filter sources' });
			await expect(filter).toBeVisible();

			// The filter narrows the rows and says how many show.
			await filter.fill('nya');
			await expect(page.getByTestId('sources-filter-count')).toHaveText('Showing 1 of 5 sources');
			await expect(page.getByTestId('source-1')).toBeVisible();
			await expect(page.getByTestId('source-0')).toHaveCount(0);

			// The row is one line; its detail opens from the name.
			const expand = page.getByRole('button', { name: 'Nyaa', exact: true });
			await expect(expand).toHaveAttribute('aria-expanded', 'false');
			await expect(page.getByTestId('source-1-detail')).toBeHidden();
			await expand.click();
			await expect(expand).toHaveAttribute('aria-expanded', 'true');
			await expect(page.getByTestId('source-1-addresses')).toBeVisible();
			await expect(page.getByRole('combobox', { name: 'Nyaa: priority' })).toBeVisible();

			// The switch opens the same consent dialog as before, and sends nothing.
			const toggle = page.getByRole('switch', { name: 'Nyaa: on' });
			await toggle.click();
			const dialog = page.getByRole('dialog', { name: 'Nyaa connects to outside services.' });
			await expect(dialog).toBeVisible();
			await expect(toggle).not.toBeChecked();
			await page.keyboard.press('Escape');
			await expect(dialog).toBeHidden();
			await expect(toggle).not.toBeChecked();

			// Nothing matches: an empty state, and Clear filter brings the rows back.
			await filter.fill('zzz');
			await expect(page.getByTestId('sources-filter-empty')).toBeVisible();
			await page.getByRole('button', { name: 'Clear filter' }).click();
			await expect(page.getByTestId('source-0')).toBeVisible();
			expect(sent).toEqual([]);
			// No horizontal scroll.
			expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(
				width
			);
		});
	}

	for (const width of [390, 1366, 1440]) {
		test(`axe-clean with a group closed and a row open at ${width} px`, async ({ page, go }) => {
			await page.setViewportSize({ width, height: 900 });
			await go('/settings/sources');
			await expect(page.getByTestId('sources-legend')).toBeVisible();
			await page.getByTestId('sources-group-network-toggle').click();
			await expect(page.getByTestId('sources-group-network')).toBeHidden();
			await page.getByRole('button', { name: 'Mainline DHT', exact: true }).click();
			await expect(page.getByTestId('source-0-detail')).toBeVisible();
			await expectNoA11yViolations(page);
		});
	}
});
