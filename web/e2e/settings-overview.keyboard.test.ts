// Settings → Overview (X-settings-overview) against the OpenAPI mock, by keyboard alone, with axe:
// Needs attention lists what /system and the network check report, each with a link to the
// screen that fixes it; Changed from default lists the values that aren't defaults, grouped by
// screen, with a secret shown only as "Set"; a healthy install says so; a failed load can retry.
import { expectNoA11yViolations } from './a11y';
import { expect as baseExpect, test } from './support/test';
import {
	FIXTURE_INDEX,
	SettingsBackend,
	fixtureValues,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';
import type { components } from '../src/lib/api/schema';

const expect = baseExpect.configure({ timeout: 10_000 });
type System = components['schemas']['SystemInfo'];

const system = (over: Partial<System> = {}): System =>
	({
		address_change: null,
		data: { filesystem_warning: null },
		data_formats: { index: 'v3-t2', rebuild: null, store_schema: 12 },
		diagnostics: { changed: [], logging: null, recent_errors: 0 },
		restart_required: [],
		...over
	}) as unknown as System;

const network = (reachable: boolean | null) => ({ reachable });

test.describe('keyboard: Settings → Overview', () => {
	let current: System;
	let reachable: boolean | null;
	let systemFails: number;
	test.beforeEach(async ({ mock }) => {
		const values = fixtureValues();
		values.find((s) => s.section === 'secrets')!.fields.token!.value = 'hunter2-never-sent';
		new SettingsBackend(FIXTURE_INDEX, values).attach(mock as unknown as MockLike);
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		current = system();
		reachable = true;
		systemFails = 0;
		on('getSystem', () =>
			systemFails-- > 0
				? { status: 500, body: { code: 'internal', message: 'boom' } }
				: { status: 200, body: current }
		);
		on('getNetworkCheck', () => ({ status: 200, body: network(reachable) }));
	});

	test('lists problems with a link to each fix, and passes axe', async ({ page, go }) => {
		current = system({
			restart_required: ['network.http_bind'],
			diagnostics: { recent_errors: 12 }
		});
		reachable = false;
		await go('/settings');
		const list = page.getByTestId('attention-list');
		await expect(list.getByText("The DHT port isn't reachable", { exact: false })).toBeVisible();
		await expect(list.getByText('Changes waiting for a restart: 1.')).toBeVisible();
		await expect(list.getByText('Errors logged recently: 12.')).toBeVisible();
		await expect(list.getByRole('link')).toHaveCount(3);
		await expect(page.getByTestId('attention-empty')).toHaveCount(0);
		await expectNoA11yViolations(page);
	});

	test('lists changed settings by screen, links to the field, hides secrets', async ({
		page,
		go
	}) => {
		await go('/settings');
		await expect(page.getByTestId('attention-empty')).toHaveText('Nothing needs attention.');
		const rows = page.getByTestId('changed-row');
		await expect(rows.filter({ hasText: 'Saved in the app' }).first()).toBeVisible();
		await expect(rows.filter({ hasText: 'From config.toml' })).toHaveCount(1);
		await expect(rows.filter({ hasText: 'Set by LODESTAR_DHT_PORT' })).toHaveCount(1);
		await expect(rows.filter({ hasText: /token.*Set · Saved in the app/ })).toHaveCount(1);
		await expect(page.getByText('hunter2-never-sent')).toHaveCount(0);
		await expect(page.locator('a[href$="/settings/crawler#crawler.timeout_secs"]')).toBeVisible();
		await expectNoA11yViolations(page);
		await page.locator('a[href$="/settings/crawler#crawler.timeout_secs"]').focus();
		await page.keyboard.press('Enter');
		await expect(page).toHaveURL(/\/settings\/crawler#crawler\.timeout_secs$/);
	});

	test('says so when every setting is at its default', async ({ page, go, mock }) => {
		const values = fixtureValues().map((s) => ({
			...s,
			fields: Object.fromEntries(
				Object.entries(s.fields).map(([k, f]) => [k, { ...f, source: 'default' as const }])
			)
		}));
		new SettingsBackend(FIXTURE_INDEX, values).attach(mock as unknown as MockLike);
		await go('/settings');
		await expect(page.getByTestId('changed-empty')).toHaveText('Every setting is at its default.');
	});

	test('a failed load offers Retry', async ({ page, go, mock }) => {
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		let fail = true;
		on('getSettings', () =>
			fail ? { status: 500, body: { code: 'internal', message: 'boom' } } : undefined
		);
		await go('/settings');
		await expect(page.getByText("The overview couldn't be loaded.")).toBeVisible();
		fail = false;
		new SettingsBackend(FIXTURE_INDEX, fixtureValues()).attach(mock as unknown as MockLike);
		await page.getByRole('button', { name: 'Retry' }).click();
		await expect(page.getByRole('heading', { name: 'Changed from default' })).toBeVisible();
	});
});
