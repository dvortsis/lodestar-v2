// UI primitive showcases (W7ab): axe in Riverstone light and dark, plus keyboard behaviour.
// Named `*.routes.test.ts` so the `core` project's testMatch picks it up.
// No snapshot baselines: they are generated in CI's container (see README, "Snapshots").
import { expectNoA11yViolations } from './a11y';
import { expect, test } from './support/test';

const GROUPS = ['controls', 'fields', 'labels', 'feedback'] as const;

for (const mode of ['light', 'dark'] as const) {
	test.describe(`Riverstone ${mode}`, () => {
		test.use({ themeId: `riverstone-${mode}`, colorMode: mode });
		for (const group of GROUPS) {
			test(`ui/${group} has no axe violations`, async ({ page, go }) => {
				await go(`/dev/ui/${group}`);
				await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
				if (group === 'fields') {
					// Surface the inline error, the invalid range and the open combobox too.
					await page.getByRole('combobox', { name: 'Tracker' }).focus();
					await page.keyboard.press('ArrowDown');
				}
				if (group === 'feedback') {
					for (const name of ['info', 'success', 'warning', 'error']) {
						await page.getByRole('button', { name: `Toast ${name}` }).click();
					}
					await expect(page.getByTestId('toast')).toHaveCount(4);
				}
				await expectNoA11yViolations(page);
			});
		}
	});
}

test('segmented control: arrows move, tab leaves the group', async ({ page, go }) => {
	await go('/dev/ui/controls');
	const compact = page.getByRole('radio', { name: 'Compact' });
	await page.getByRole('radio', { name: 'Comfortable' }).focus();
	await page.keyboard.press('ArrowRight');
	await expect(compact).toBeFocused();
	await expect(page.getByTestId('density-value')).toHaveText('Selected: compact');
	await page.keyboard.press('Tab');
	await expect(compact).not.toBeFocused();
});

test('button loading keeps its width', async ({ page, go }) => {
	await go('/dev/ui/controls');
	const button = page.getByTestId('loading-button');
	const before = (await button.boundingBox())!.width;
	await button.click();
	await expect(button).toHaveAttribute('aria-busy', 'true');
	expect((await button.boundingBox())!.width).toBe(before);
});

test('controls meet the 44 px target under a coarse pointer', async ({ browser }) => {
	const context = await browser.newContext({ hasTouch: true, isMobile: true });
	const page = await context.newPage();
	await page.goto('/dev/ui/controls');
	for (const el of await page.getByRole('button').all()) {
		const box = (await el.boundingBox())!;
		expect(box.height).toBeGreaterThanOrEqual(44);
	}
	await context.close();
});

test('combobox and chip work from the keyboard', async ({ page, go }) => {
	await go('/dev/ui/fields');
	const combo = page.getByRole('combobox', { name: 'Tracker' });
	await combo.focus();
	await page.keyboard.press('ArrowDown');
	await page.keyboard.press('ArrowDown');
	await page.keyboard.press('Enter');
	await expect(combo).toHaveValue('torrent.eu.org');

	await go('/dev/ui/labels');
	const remove = page.getByRole('button', { name: 'Remove filter: audio' });
	await remove.focus();
	await page.keyboard.press('Backspace');
	await expect(remove).toHaveCount(0);
});

test('toasts do not steal focus and dismiss from the keyboard', async ({ page, go }) => {
	await go('/dev/ui/feedback');
	const trigger = page.getByRole('button', { name: 'Toast success' });
	await trigger.focus();
	await page.keyboard.press('Enter');
	await expect(page.getByTestId('toast')).toHaveCount(1);
	await expect(trigger).toBeFocused();
	await page.keyboard.press('Tab');
	await page.keyboard.press('Tab');
});
