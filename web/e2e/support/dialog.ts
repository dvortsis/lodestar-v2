import { expect, type Page } from '@playwright/test';

/**
 * Waits until a dialog has finished opening: visible, `data-state="open"`, fully opaque and
 * nothing animating. axe reads colours mid-fade otherwise, and a key press before then can land
 * before focus has moved into the panel.
 */
export async function dialogSettled(page: Page, name?: string) {
	const dialog = page.getByRole('dialog', name ? { name } : undefined);
	await expect(dialog).toBeVisible();
	await expect
		.poll(
			() =>
				page.evaluate(() => {
					const el = document.querySelector('[role="dialog"]');
					return (
						el?.getAttribute('data-state') === 'open' &&
						Number(getComputedStyle(el).opacity) === 1 &&
						document.getAnimations().every((a) => a.playState !== 'running')
					);
				}),
			{ timeout: 15_000, message: 'the dialog did not finish opening' }
		)
		.toBe(true);
}
