// The Settings form engine (M-settings-form) on /dev/settings-form against the OpenAPI mock:
// edit, save and Undo; client and server validation with the error summary; the leave guard on
// a link and on Back; draft restore after a reload; an env-locked field; a two-context
// conflict; a disruptive save's dialog; axe in Riverstone light and dark at 1366 and 390.
import { expectNoA11yViolations } from './a11y';
import { OpenApiMock } from './mock/openapi-mock';
import { expect, test } from './support/test';
import { SettingsBackend } from '../src/lib/features/settings/form/testing/e2e-backend';

const PAGE = '/dev/settings-form';

test.describe('settings form', () => {
	let backend: SettingsBackend;
	test.beforeEach(async ({ mock }) => {
		backend = new SettingsBackend().attach(mock);
	});

	const ready = async (page: import('@playwright/test').Page) =>
		expect(page.getByRole('heading', { level: 1, name: 'Settings form' })).toBeVisible() &&
		expect(page.getByRole('radiogroup')).toBeVisible();

	test('edits, saves and undoes', async ({ page, go }) => {
		await go(PAGE);
		await ready(page);
		await page.getByRole('radio', { name: 'low' }).check();
		const bar = page.getByTestId('save-bar');
		await expect(bar).toContainText('1 unsaved change');
		await bar.getByRole('button', { name: 'Save changes' }).click();
		await expect(bar).toContainText('Saved');
		expect(backend.patches).toEqual([{ crawler: { if_revision: 3, intensity: 'low' } }]);
		await bar.getByRole('button', { name: 'Undo' }).click();
		await expect.poll(() => backend.patches.length).toBe(2);
		expect(backend.patches[1]).toEqual({ crawler: { if_revision: 4, intensity: null } });
		await expect(page.getByRole('radio', { name: 'balanced' })).toBeChecked();
	});

	test('validates on the client and the server, with the summary', async ({ page, go }) => {
		await go(PAGE);
		await ready(page);
		await page.getByText('Advanced (1)').click();
		const timeout = page.getByLabel('timeout_secs', { exact: true });
		await timeout.fill('0');
		await timeout.blur();
		await expect(page.getByText('Must be between 1 and 120 seconds.')).toBeVisible();
		await expect(timeout).toHaveAttribute('aria-invalid', 'true');
		await page.getByRole('button', { name: 'Save changes' }).click();
		const summary = page.getByTestId('error-summary');
		await expect(summary.getByRole('heading')).toBeFocused();
		expect(backend.patches).toHaveLength(0);
		await summary.getByRole('link').click();
		await expect(timeout).toBeFocused();

		// The server has the final say.
		backend.nextPatch.push({
			status: 422,
			body: {
				error: {
					code: 'invalid_setting',
					section: 'crawler',
					field: 'timeout_secs',
					message: 'Must be from 1 to 120.'
				}
			}
		});
		await timeout.fill('30');
		await page.getByRole('button', { name: 'Save changes' }).click();
		await expect(summary.getByRole('link', { name: /Must be from 1 to 120/ })).toBeVisible();
	});

	test('guards leaving by a link and by Back', async ({ page, go }) => {
		// Client-side history: the showcase, then another page, then back to the showcase.
		await go(PAGE);
		await ready(page);
		await page.getByRole('link', { name: 'Another page' }).click();
		await expect(page).toHaveURL(/\/dev\/ui\/controls/);
		await page.goBack();
		await ready(page);
		await page.getByRole('radio', { name: 'low' }).check();
		await page.getByRole('link', { name: 'Another page' }).click();
		const dialog = page.getByRole('dialog', { name: 'Leave without saving?' });
		await expect(dialog).toBeVisible();
		await dialog.getByRole('button', { name: 'Keep editing' }).click();
		await expect(page).toHaveURL(new RegExp(PAGE));
		await expect(dialog).toHaveCount(0);

		await page.goForward(); // history navigation (popstate), as Back is
		await expect(dialog).toBeVisible();
		await dialog.getByRole('button', { name: 'Discard' }).click();
		await expect(page).toHaveURL(/\/dev\/ui\/controls/);
		expect(backend.patches).toHaveLength(0);
	});

	test('restores a draft after a reload', async ({ page, go }) => {
		await go(PAGE);
		await ready(page);
		await page.getByRole('radio', { name: 'aggressive' }).check();
		await page.waitForTimeout(400); // the draft is written 300 ms after the last edit
		page.on('dialog', (d) => void d.accept()); // the browser's own leave prompt
		await page.reload();
		await ready(page);
		await expect(page.getByText('You have unsaved changes from earlier')).toBeVisible();
		await page.getByRole('button', { name: 'Restore' }).click();
		await expect(page.getByRole('radio', { name: 'aggressive' })).toBeChecked();
		await expect(page.getByTestId('save-bar')).toContainText('1 unsaved change');
	});

	test('shows an env-locked field as set by its variable', async ({ page, go }) => {
		await go(PAGE);
		await ready(page);
		const field = page.locator('[data-field="network.dht_port"]');
		await expect(field.getByTestId('status-env')).toContainText('Set by LODESTAR_DHT_PORT');
		await expect(field.getByLabel('dht_port', { exact: true })).toBeDisabled();
	});

	test('merges a two-context conflict: Keep mine, then Use theirs', async ({
		page,
		go,
		browser
	}) => {
		const other = await browser.newContext();
		const page2 = await other.newPage();
		const mock2 = new OpenApiMock();
		await mock2.install(page2);
		backend.attach(mock2);
		await go(PAGE);
		await page2.goto(PAGE);
		await ready(page);
		await ready(page2);

		await page.getByRole('radio', { name: 'low' }).check();
		await page2.getByRole('radio', { name: 'aggressive' }).check();
		await page2.getByRole('button', { name: 'Save changes' }).click();
		await expect(page2.getByTestId('save-bar')).toContainText('Saved');

		await page.getByRole('button', { name: 'Save changes' }).click();
		const conflict = page.locator('[data-field="crawler.intensity"]').getByTestId('conflict');
		await expect(conflict).toContainText('Changed elsewhere to aggressive');
		expect(backend.patches.at(-1)).toEqual({ crawler: { if_revision: 3, intensity: 'low' } });
		await conflict.getByRole('button', { name: 'Keep mine' }).click();
		await page.getByRole('button', { name: 'Save changes' }).click();
		await expect(page.getByTestId('save-bar')).toContainText('Saved');
		expect(backend.patches.at(-1)).toEqual({ crawler: { if_revision: 4, intensity: 'low' } });

		// The other tab now loses: Use theirs takes the first tab's value.
		await page2.getByRole('radio', { name: 'balanced' }).check();
		await page2.getByRole('button', { name: 'Save changes' }).click();
		const conflict2 = page2.locator('[data-field="crawler.intensity"]').getByTestId('conflict');
		await expect(conflict2).toContainText('Changed elsewhere to low');
		await conflict2.getByRole('button', { name: 'Use theirs' }).click();
		await expect(page2.getByRole('radio', { name: 'low' })).toBeChecked();
		await expect(page2.getByTestId('save-bar')).not.toContainText('unsaved');
		mock2.assertNoFailures();
		await other.close();
	});

	test('confirms a disruptive change, naming the new address', async ({ page, go }) => {
		await go(PAGE);
		await ready(page);
		await page.getByLabel('http_bind', { exact: true }).fill('0.0.0.0:9000');
		await page.getByRole('button', { name: 'Save changes' }).click();
		const dialog = page.getByRole('dialog', { name: 'Save this change?' });
		await expect(dialog).toContainText(
			'Lodestar will be at http://192.168.1.5:9000/ after a restart.'
		);
		await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused();
		await dialog.getByRole('button', { name: 'Cancel' }).click();
		expect(backend.patches).toHaveLength(0);
		await expect(dialog).toHaveCount(0);
		await page.getByTestId('save-bar').getByRole('button', { name: 'Save changes' }).click();
		await dialog.getByRole('button', { name: 'Save changes' }).click();
		await expect.poll(() => backend.patches.length).toBe(1);
	});
});

for (const mode of ['light', 'dark'] as const) {
	for (const width of [1366, 390]) {
		test.describe(`Riverstone ${mode} at ${width}`, () => {
			test.use({
				themeId: `riverstone-${mode}`,
				colorMode: mode,
				viewport: { width, height: 900 }
			});
			test('is axe-clean with every state shown', async ({ page, go, mock }) => {
				const backend = new SettingsBackend().attach(mock);
				await go(PAGE);
				await expect(page.getByRole('radiogroup')).toBeVisible();
				const bar = page.getByTestId('save-bar');
				// The error summary and an inline error.
				await page.getByText('Advanced (1)').click();
				const timeout = page.getByLabel('timeout_secs', { exact: true });
				await timeout.fill('500');
				await bar.getByRole('button', { name: 'Save changes' }).click();
				await expect(page.getByTestId('error-summary')).toBeVisible();
				await expectNoA11yViolations(page);
				// A conflict with another tab's save.
				await timeout.fill('12');
				await page.getByRole('radio', { name: 'low' }).check();
				backend.change('crawler', { intensity: { value: 'aggressive', source: 'db' } });
				await bar.getByRole('button', { name: 'Save changes' }).click();
				await expect(page.getByTestId('conflict')).toBeVisible();
				await page.getByRole('button', { name: 'Replace' }).click(); // the secret's input
				await expectNoA11yViolations(page);
				// The disruptive confirmation.
				await page.getByRole('button', { name: 'Use theirs' }).click();
				await page.getByLabel('http_bind', { exact: true }).fill('0.0.0.0:9000');
				await bar.getByRole('button', { name: 'Save changes' }).click();
				await expect(page.getByRole('dialog', { name: 'Save this change?' })).toBeVisible();
				await expectNoA11yViolations(page);
			});
		});
	}
}
