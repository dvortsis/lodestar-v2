// The restart banner and flow (§10.10.6) against the OpenAPI mock: the pending banner, the
// confirmation, the full-page Restarting state that polls /healthz, and the two-minute give-up.
// No fixed sleeps: every wait is a condition with a deadline, and the give-up runs on the test
// clock.
import type { Page } from '@playwright/test';
import { expectNoA11yViolations } from './a11y';
import { expect, test } from './support/test';
import type { OpenApiMock } from './mock/openapi-mock';

const DEADLINE = { timeout: 15_000 };

type Json = Record<string, unknown>;

/** `GET /system` as the mock's documented example, with the restart facts this spec varies. */
function systemWith(mock: OpenApiMock, over: Json): Json {
	const example = mock.operations.find((o) => o.id === 'getSystem')?.example?.body as Json;
	return { ...example, ...over };
}

/**
 * The chrome starts the restart watch (and with it the banner) on the first interaction, or 30 s
 * after load. A key press is the interaction; it is repeated until the page has hydrated and taken
 * it, then the banner is waited for.
 */
const engage = (page: Page) =>
	expect(async () => {
		await page.keyboard.press('Shift');
		await expect(page.getByTestId('restart-count')).toBeVisible({ timeout: 1_000 });
	}).toPass({ timeout: 15_000 });

const pending = { restart_required: ['network.dht_port', 'crawler.timeout_secs'] };

async function openRestartConfirmation(page: Page) {
	await engage(page);
	await expect(page.getByTestId('restart-count')).toHaveText(
		/2 changes apply after a restart/,
		DEADLINE
	);
	await page.getByRole('button', { name: 'Restart now' }).click();
	await expect(page.getByRole('dialog', { name: 'Restart Lodestar?' })).toBeVisible();
	await expect(page.getByTestId('restart-warning')).toContainText(
		/Crawling pauses for about \d+ seconds/
	);
}

test.describe('restart banner', () => {
	test('shows the pending changes, with a button only when the server can restart', async ({
		page,
		go,
		mock
	}) => {
		mock.on(
			'getSystem',
			systemWith(mock, {
				...pending,
				restart: {
					available: false,
					estimated_seconds: 4,
					hint: 'Restart the container',
					method: 'manual'
				}
			}) as never
		);
		await go('/settings');
		await engage(page);
		await expect(page.getByTestId('restart-count')).toBeVisible(DEADLINE);
		await expect(page.getByTestId('restart-hint')).toContainText('Restart the container');
		await expect(page.getByRole('button', { name: 'Restart now' })).toHaveCount(0);
		await expectNoA11yViolations(page);
	});

	test('restart now: confirm, wait for /healthz, reload with nothing pending', async ({
		page,
		go,
		mock
	}) => {
		let restarted = false;
		let healthChecks = 0;
		mock.on('getSystem', (() =>
			restarted
				? systemWith(mock, { restart_required: [], started_at: 2_000 })
				: systemWith(mock, { ...pending, started_at: 1_000 })) as never);
		mock.on('restartSystem', (() => {
			restarted = true;
			return { status: 202, body: { estimated_seconds: 4, restarting: true } };
		}) as never);
		// Down for the first two probes, then back: the page must wait for it.
		await page.route('**/healthz', (route) =>
			++healthChecks <= 2 && restarted ? route.abort() : route.fulfill({ status: 200, body: 'ok' })
		);
		await go('/settings');
		await openRestartConfirmation(page);
		await expectNoA11yViolations(page);
		await page.getByRole('dialog').getByRole('button', { name: 'Restart now' }).click();
		await expect(page.getByTestId('restarting-page')).toBeVisible();
		await expect(page.getByTestId('restarting-status')).toContainText('restarting');
		await expectNoA11yViolations(page);
		// Back: the page reloads onto Settings with no banner.
		await expect(page.getByTestId('restarting-page')).toHaveCount(0, DEADLINE);
		await expect(page.getByRole('heading', { name: 'Needs attention' })).toBeVisible(DEADLINE);
		await expect(page.getByTestId('restart-banner')).toHaveCount(0);
	});

	test('a 409 shows the server message and stops', async ({ page, go, mock }) => {
		mock.on('getSystem', systemWith(mock, pending) as never);
		mock.error('restartSystem', 409, 'restart_failed', 'It could not get ready to restart.');
		await go('/settings');
		await openRestartConfirmation(page);
		await page.getByRole('dialog').getByRole('button', { name: 'Restart now' }).click();
		await expect(page.getByTestId('restart-failure')).toHaveText(
			'It could not get ready to restart.'
		);
		await expect(page.getByTestId('restarting-page')).toHaveCount(0);
	});

	test('not back within two minutes: says what to check', async ({ page, go, mock }) => {
		await page.clock.install();
		mock.on('getSystem', systemWith(mock, pending) as never);
		mock.on('restartSystem', {
			status: 202,
			body: { estimated_seconds: 4, restarting: true }
		} as never);
		await page.route('**/healthz', (route) => route.abort());
		await go('/settings');
		await openRestartConfirmation(page);
		await page.getByRole('dialog').getByRole('button', { name: 'Restart now' }).click();
		await expect(page.getByTestId('restarting-page')).toBeVisible();
		// The test clock moves the page's timers; each step lets the next probe run.
		await expect
			.poll(
				async () => {
					await page.clock.runFor(15_000);
					return page.getByTestId('restarting-page').getAttribute('data-phase');
				},
				{ timeout: 30_000 }
			)
			.toBe('timeout');
		await expect(page.getByTestId('restarting-checks')).toContainText('lodestar healthcheck');
		await expect(page.getByRole('button', { name: 'Keep waiting' })).toBeVisible();
	});

	test('keyboard only: Restart now opens the dialog on Cancel, Escape closes the dialog', async ({
		page,
		go,
		mock
	}) => {
		mock.on('getSystem', systemWith(mock, pending) as never);
		await go('/settings');
		await engage(page);
		await expect(page.getByTestId('restart-count')).toBeVisible(DEADLINE);
		await page.getByRole('button', { name: 'Restart now' }).focus();
		await page.keyboard.press('Enter');
		const dialog = page.getByRole('dialog', { name: 'Restart Lodestar?' });
		await expect(dialog).toBeVisible();
		await expect(dialog.getByRole('button', { name: 'Cancel', exact: true })).toBeFocused();
		await page.keyboard.press('Escape');
		await expect(dialog).toHaveCount(0);
		await expect(page.getByRole('button', { name: 'Restart now' })).toBeFocused();
	});
});
