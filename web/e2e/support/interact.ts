import { expect, type Locator, type Page } from '@playwright/test';

/**
 * The first interaction, once: one `focusin` loads the chrome (and the search bar's lazy part), then
 * this waits for `ready`, an element only the loaded chrome has (S-search-1 decisions). The shell's
 * listener is attached by the time `#boot` has gone.
 */
export async function interact(page: Page, ready: Locator = page.getByTestId('more-button')) {
	await expect(page.locator('#boot')).toHaveCount(0);
	await page.evaluate(() => window.dispatchEvent(new Event('focusin')));
	await expect(ready).toBeAttached();
}
