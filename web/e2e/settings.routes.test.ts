import { readFileSync } from 'node:fs';
import type { components } from '../src/lib/api/schema';
import { expect as baseExpect, test } from './support/test';
import { expectNoA11yViolations } from './a11y';
import { liveEvents } from './support/events-server';

const expect = baseExpect.configure({ timeout: 10_000 });

test('General downloads the backend settings file with preferences only when selected', async ({
	page,
	go
}) => {
	const requests: string[] = [];
	const bytes = '# settings fixture\r\nlodestar_settings = 1\r\n[crawler]\r\npaused = true\r\n';
	await page.route('**/api/v1/settings/export?*', async (route) => {
		requests.push(route.request().url());
		await route.fulfill({ status: 200, contentType: 'application/toml', body: bytes });
	});
	await go('/settings/general');
	await expect(page.getByRole('heading', { name: 'Backup & portability' })).toBeVisible();
	expect(requests).toHaveLength(0);
	for (const preferences of [false, true]) {
		if (preferences)
			await page.getByRole('switch', { name: 'Include interface preferences' }).click();
		const downloading = page.waitForEvent('download', { timeout: 10_000 });
		await page.getByRole('button', { name: 'Download saved settings' }).click();
		const download = await downloading;
		expect(download.suggestedFilename()).toBe('lodestar-settings.toml');
		const path = await download.path();
		expect(path).not.toBeNull();
		expect(readFileSync(path!, 'utf8')).toBe(bytes);
		expect(new URL(requests.at(-1)!).searchParams.get('preferences')).toBe(String(preferences));
	}
	await expect(page.getByText('Download started. Check your browser’s downloads.')).toBeVisible();
	await expectNoA11yViolations(page);
});

test('General export failures stay on the screen and can retry without exposing response text', async ({
	page,
	go
}) => {
	let attempts = 0;
	await page.route('**/api/v1/settings/export?*', async (route) => {
		attempts++;
		await route.fulfill(
			attempts === 1
				? { status: 200, contentType: 'text/html', body: '<p>private upstream error</p>' }
				: { status: 200, contentType: 'application/toml', body: 'lodestar_settings = 1\n' }
		);
	});
	await go('/settings/general');
	await page.getByRole('button', { name: 'Download saved settings' }).click();
	await expect(
		page.getByText('Could not download settings. Try again when Lodestar is available.')
	).toBeVisible();
	await expect(page.getByText('private upstream error')).toHaveCount(0);
	await expect(page).toHaveURL(/\/settings\/general/);
	const downloading = page.waitForEvent('download', { timeout: 10_000 });
	await page.getByRole('button', { name: 'Download saved settings' }).click();
	await downloading;
	expect(attempts).toBe(2);
});

const raw = readFileSync(
	new URL('../../crates/lodestar/src/settings/golden/settings_index.txt', import.meta.url),
	'utf8'
);
const index: components['schemas']['SettingsIndex'] = JSON.parse(raw.slice(raw.indexOf('\n') + 1));
const value = (
	value: unknown,
	source: components['schemas']['SettingSource'] = 'default'
): components['schemas']['SettingValue'] => ({
	value,
	source,
	read_only: false,
	restart_required: false,
	env: null
});

function settingsValues(section: string, revision = 3) {
	const fields =
		section === 'crawler'
			? {
					intensity: value('balanced'),
					encryption: value('prefer'),
					max_sockets: value(100),
					timeout_secs: value(12),
					max_metadata_mib: value(4),
					paused: value(false)
				}
			: section === 'transport'
				? { utp: value(false), fallback_to_tcp: value(true) }
				: section === 'network'
					? {
							http_bind: value('127.0.0.1:8484'),
							secure_cookies: value(false),
							dht_port: value(6881),
							dht_enabled: value(false)
						}
					: section === 'general'
						? { data_dir: value('fixture-data') }
						: {};
	return { section, revision, fields, restart_required: [] as string[] };
}

test.beforeEach(({ mock }) => {
	mock.on('getAuthStatus', { logged_in: true, setup_required: false });
	mock.on('listSettings', index);
	mock.on('getSettings', (request) => ({
		status: 200,
		body: settingsValues(new URL(request.url()).pathname.split('/').at(-1)!)
	}));
	mock.on('previewSettings', { sections: {} });
});

test('desktop navigation, field labels and unknown destinations', async ({ page, go, mock }) => {
	await page.setViewportSize({ width: 1366, height: 900 });
	await go('/settings');
	await expect(page.getByRole('heading', { name: 'Settings overview' })).toBeVisible();
	await page
		.getByRole('navigation', { name: 'Settings sections' })
		.getByRole('link', { name: 'Crawler', exact: true })
		.click();
	await expect(page.getByRole('switch', { name: 'Pause crawling' })).toBeVisible();
	await expect(page.getByRole('link', { name: 'Crawler', exact: true })).toHaveAttribute(
		'aria-current',
		'page'
	);
	await expectNoA11yViolations(page);
	await page.screenshot({ path: '../target/settings-desktop.png', fullPage: true });
	await page.getByLabel('Find a section').fill('network');
	await expect(
		page.getByRole('navigation', { name: 'Settings sections' }).getByRole('link')
	).toHaveCount(1);
	await page.getByRole('link', { name: 'Network & VPN', exact: true }).click();
	await expect(page.getByLabel('Web address', { exact: true })).not.toBeEditable();
	await expect(page.getByText(/Network settings are read-only/)).toBeVisible();
	expect(mock.calls('patchSettingsBatch')).toHaveLength(0);
	await go('/settings/does-not-exist');
	await expect(
		page.getByText('This Settings screen is not available in this build.')
	).toBeVisible();
});

test('mobile list-detail navigation preserves density and fits the viewport', async ({
	page,
	go
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await go('/settings');
	await page
		.getByRole('navigation', { name: 'Settings sections' })
		.getByRole('link', { name: 'General', exact: true })
		.click();
	await expect(page.getByRole('heading', { name: 'General', exact: true })).toBeVisible();
	await expect(page.getByRole('navigation', { name: 'Settings sections' })).toBeHidden();
	await expect(page.getByRole('link', { name: 'Back to Settings' })).toHaveAttribute(
		'href',
		'/settings?density=comfortable'
	);
	expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
	await page.screenshot({ path: '../target/settings-mobile.png', fullPage: true });
	await page.getByRole('link', { name: 'Back to Settings' }).click();
	await expect(page.getByRole('navigation', { name: 'Settings sections' })).toBeVisible();
	await page.goBack();
	await expect(page.getByRole('heading', { name: 'General', exact: true })).toBeVisible();
});

for (const width of [390, 1366]) {
	test(`a hand-built screen (Keyboard) opens from the navigation at ${width} px`, async ({
		page,
		go,
		mock
	}) => {
		const field = (value: unknown) => ({
			value,
			env: null,
			read_only: false,
			restart_required: false,
			source: 'default' as const
		});
		mock.on('getSettings', {
			section: 'preferences',
			revision: 1,
			restart_required: [],
			fields: { shortcuts: field(true), shortcut_keys: field({}) }
		});
		mock.on('patchSettings', {
			section: 'preferences',
			revision: 2,
			restart_required: [],
			fields: { shortcuts: field(false), shortcut_keys: field({}) }
		});
		await page.setViewportSize({ width, height: 900 });
		await go('/settings');
		const nav = page.getByRole('navigation', { name: 'Settings sections' });
		await expect(nav.getByRole('heading', { name: 'Preferences' })).toBeVisible();
		await nav.getByRole('link', { name: 'Keyboard', exact: true }).click();
		await expect(page).toHaveURL(/\/settings\/keyboard/);
		await expect(page.getByRole('heading', { level: 1, name: 'Keyboard' })).toBeVisible();
		await expect(page.getByRole('heading', { level: 1 })).toHaveCount(1);
		await expect(
			page.getByText('Choose how Lodestar responds to the keyboard, and which key does what.')
		).toBeVisible();
		expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(
			true
		);
		// A preference: it applies at once with Undo, never Save and Cancel (§10.10.1).
		await expect(page.getByRole('button', { name: 'Save' })).toHaveCount(0);
		await page.getByRole('switch', { name: 'Single-key shortcuts' }).click();
		await expect(page.getByText('Saved.')).toBeVisible();
		await expect(page.getByRole('button', { name: 'Undo' })).toBeVisible();
		expect(mock.calls('patchSettings').map((request) => request.postDataJSON())).toEqual([
			{ shortcuts: false, if_revision: 1 }
		]);
		await expectNoA11yViolations(page);
	});
}

test('direct Advanced field link opens its disclosure and focuses the control', async ({
	page
}) => {
	await page.goto('/settings/crawler#transport.utp');
	const control = page.getByRole('switch', { name: 'uTP (not active yet)', exact: true });
	await expect(control).toBeVisible();
	await expect(control).toBeFocused();
	await page.reload();
	await expect(control).toBeFocused();
});

test('schema acquisition failure offers a working retry', async ({ page, go, mock }) => {
	mock.on('listSettings', {
		status: 503,
		body: { error: { code: 'unavailable', message: 'fixture failure' } }
	});
	await go('/settings/crawler');
	await expect(page.getByRole('button', { name: 'Retry', exact: true })).toBeVisible();
	mock.on('listSettings', index);
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByRole('switch', { name: 'Pause crawling' })).toBeVisible();
});

test('saves Crawler and Transport in one batch with separate revisions', async ({
	page,
	go,
	mock
}) => {
	const crawler = settingsValues('crawler', 3);
	const transport = settingsValues('transport', 7);
	mock.on('getSettings', (request) => ({
		status: 200,
		body: request.url().endsWith('/transport') ? transport : crawler
	}));
	mock.on('patchSettingsBatch', {
		status: 200,
		body: {
			sections: {
				crawler: {
					...crawler,
					revision: 4,
					fields: { ...crawler.fields, paused: value(true, 'db') }
				},
				transport: {
					...transport,
					revision: 8,
					fields: { ...transport.fields, utp: value(true, 'db') }
				}
			}
		}
	});
	await go('/settings/crawler');
	await page.getByRole('switch', { name: 'Pause crawling' }).click();
	await page.getByRole('switch', { name: 'uTP (not active yet)', exact: true }).click();
	await page.getByRole('button', { name: 'Save changes', exact: true }).click();
	await expect(page.getByTestId('save-bar-status')).toHaveText('Saved');
	expect(mock.calls('patchSettingsBatch').map((request) => request.postDataJSON())).toEqual([
		{ crawler: { paused: true, if_revision: 3 }, transport: { utp: true, if_revision: 7 } }
	]);
	await expect(page.getByRole('switch', { name: 'Pause crawling' })).toBeChecked();
	await expect(
		page.getByRole('switch', { name: 'uTP (not active yet)', exact: true })
	).toBeChecked();
});

test('server validation reveals and focuses a collapsed Advanced field', async ({ page, mock }) => {
	mock.error('patchSettingsBatch', 422, 'invalid_setting', 'Fixture timeout limit.', {
		section: 'crawler',
		field: 'timeout_secs'
	});
	await page.goto('/settings/crawler#crawler.timeout_secs');
	const timeout = page.getByRole('textbox', { name: 'Fetch timeout', exact: true });
	await expect(timeout).toBeFocused();
	await timeout.fill('30');
	const disclosure = timeout.locator('xpath=ancestor::details');
	await disclosure.locator('summary').click();
	await page.getByRole('button', { name: 'Save changes', exact: true }).click();
	const summary = page.getByTestId('error-summary');
	await expect(summary.getByRole('heading')).toBeFocused();
	await summary.getByRole('link', { name: /Fixture timeout limit/ }).click();
	await expect(timeout).toBeVisible();
	await expect(timeout).toBeFocused();
	await expect(timeout).toHaveValue('30');
	expect(mock.calls('patchSettingsBatch')).toHaveLength(1);
});

test('stale revisions require an explicit conflict choice and another save', async ({
	page,
	go,
	mock
}) => {
	const current = settingsValues('crawler', 4);
	current.fields.intensity = value('aggressive', 'db');
	mock.on('patchSettingsBatch', {
		status: 409,
		body: {
			error: { code: 'stale_revision', message: 'Fixture revision conflict.' },
			current: { crawler: current }
		}
	});
	await go('/settings/crawler');
	await page.getByRole('radio', { name: 'Low', exact: true }).click();
	await page.getByRole('button', { name: 'Save changes', exact: true }).click();
	await expect(page.getByTestId('conflict-text')).toContainText('Aggressive');
	await page.getByRole('button', { name: 'Save changes', exact: true }).click();
	await expect(page.locator('.ls-form [aria-live="polite"]')).toHaveText(
		'Some settings were changed elsewhere. Choose which value to keep, then save again.'
	);
	expect(mock.calls('patchSettingsBatch')).toHaveLength(1);
	await page.getByRole('button', { name: 'Keep mine', exact: true }).click();
	await expect(page.getByTestId('conflict-text')).toHaveCount(0);
	expect(mock.calls('patchSettingsBatch')).toHaveLength(1);
	mock.on('patchSettingsBatch', {
		status: 200,
		body: {
			sections: {
				crawler: {
					...current,
					revision: 5,
					fields: { ...current.fields, intensity: value('low', 'db') }
				}
			}
		}
	});
	await page.getByRole('button', { name: 'Save changes', exact: true }).click();
	await expect(page.getByTestId('save-bar-status')).toHaveText('Saved');
	expect(mock.calls('patchSettingsBatch').map((request) => request.postDataJSON())).toEqual([
		{ crawler: { intensity: 'low', if_revision: 3 } },
		{ crawler: { intensity: 'low', if_revision: 4 } }
	]);
});

test('cancelled navigation keeps edits, and Discard leaves without saving', async ({
	page,
	go,
	mock
}) => {
	await go('/settings/crawler');
	await page.getByRole('switch', { name: 'Pause crawling' }).click();
	const destination = page
		.getByRole('navigation', { name: 'Settings sections' })
		.getByRole('link', { name: 'General', exact: true });
	await destination.click();
	const dialog = page.getByRole('dialog');
	await dialog.getByRole('button', { name: 'Keep editing', exact: true }).click();
	await expect(dialog).toBeHidden();
	await expect(page).toHaveURL(/\/settings\/crawler/);
	await expect(page.getByRole('switch', { name: 'Pause crawling' })).toBeChecked();
	await destination.click();
	await dialog.getByRole('button', { name: 'Discard', exact: true }).click();
	await expect(page.getByRole('heading', { name: 'General', exact: true })).toBeVisible();
	expect(mock.calls('patchSettingsBatch')).toHaveLength(0);
});

test('failed Save on leaving keeps the current screen and its edits', async ({
	page,
	go,
	mock
}) => {
	mock.error('patchSettingsBatch', 503, 'unavailable', 'Fixture writer unavailable.');
	await go('/settings/crawler');
	await page.getByRole('switch', { name: 'Pause crawling' }).click();
	await page
		.getByRole('navigation', { name: 'Settings sections' })
		.getByRole('link', { name: 'General', exact: true })
		.click();
	await page.getByRole('dialog').getByRole('button', { name: 'Save changes', exact: true }).click();
	await expect(page.getByRole('dialog')).toBeHidden();
	await expect(page.getByTestId('offline-text')).toHaveText(
		"Couldn't reach Lodestar. Your changes are kept."
	);
	await expect(page).toHaveURL(/\/settings\/crawler/);
	await expect(page.getByRole('switch', { name: 'Pause crawling' })).toBeChecked();
	expect(mock.calls('patchSettingsBatch')).toHaveLength(1);
	const current = settingsValues('crawler', 4);
	current.fields.paused = value(true, 'db');
	mock.on('patchSettingsBatch', { status: 200, body: { sections: { crawler: current } } });
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('save-bar-status')).toHaveText('Saved');
	await expect(page).toHaveURL(/\/settings\/crawler/);
	expect(mock.calls('patchSettingsBatch').map((request) => request.postDataJSON())).toEqual([
		{ crawler: { paused: true, if_revision: 3 } },
		{ crawler: { paused: true, if_revision: 3 } }
	]);
});

test('resync refreshes restart requirements and locks while preserving unrelated edits', async ({
	page,
	go,
	mock
}) => {
	const events = await liveEvents(page);
	const crawler = settingsValues('crawler');
	crawler.restart_required = ['timeout_secs'];
	mock.on('getSettings', (request) => ({
		status: 200,
		body: request.url().endsWith('/crawler') ? crawler : settingsValues('transport')
	}));
	await go('/settings/crawler');
	await expect(page.getByTestId('settings-restart-count')).toContainText('1');
	await page.getByRole('switch', { name: 'uTP (not active yet)', exact: true }).click();
	await expect
		.poll(async () => (await events.requests()).length, { timeout: 10_000 })
		.toBeGreaterThan(0);
	crawler.restart_required = [];
	crawler.fields.paused = { ...value(false, 'env'), read_only: true, env: 'LODESTAR_PAUSED' };
	await events.send('resync', {});
	await expect(page.getByRole('switch', { name: 'Pause crawling' })).toBeDisabled();
	await expect(page.getByTestId('settings-restart-count')).toHaveCount(0);
	await expect(
		page.getByRole('switch', { name: 'uTP (not active yet)', exact: true })
	).toBeChecked();
	await expect(page.getByTestId('save-bar-status')).toContainText('1 unsaved change');
	expect(mock.calls('patchSettingsBatch')).toHaveLength(0);
});
