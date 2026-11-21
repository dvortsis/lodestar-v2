// UI check (review 03 "Dialog and drawer focus return", flow-drawer-and-focus.mjs): every dialog
// and palette opened from the keyboard hands focus back to the control that opened it, whether it
// is closed with Esc or answered with its Cancel button. One case per dialog; a new one is one
// more entry in a table. Keyboard only (the `kbd` fixture fails the test on any pointer event);
// waits are conditions with a 10 s deadline, no fixed sleeps.
import type { Locator, Page } from '@playwright/test';
import {
	SettingsBackend,
	type MockLike
} from '../../src/lib/features/settings/form/testing/e2e-backend';
import type { components } from '../../src/lib/api/schema';
import type { OpenApiMock } from '../mock/openapi-mock';
import { dialogSettled } from '../support/dialog';
import { liveEvents } from '../support/events-server';
import { expect as baseExpect, test } from '../support/test';

const expect = baseExpect.configure({ timeout: 10_000 });
type Companion = components['schemas']['Companion'];

const SHA = '0123456789abcdef'.repeat(4);
const saved = {
	alert: true,
	alerts: 0,
	ast: { op: 'all' as const },
	created_at: 1_700_000_000,
	id: 1,
	last_alert_at: null,
	name: 'Lantern Keeper releases',
	paused: false,
	q: '',
	rss: false,
	sort: null,
	unread: 0,
	updated_at: 1_700_000_000
};
const companion: Companion = {
	available: '5.0.1',
	credentials: [],
	egress: null,
	health: 'healthy',
	id: 'qbittorrent',
	installed: '5.0.0',
	licence: 'GPL-2.0-or-later',
	name: 'qBittorrent',
	opt_in: false,
	refusal: null,
	runs: 'lodestar',
	templates: []
};

interface Case {
	name: string;
	path: string;
	mocks: (mock: OpenApiMock) => void;
	/** The control that opens the dialog, reached by Tab. */
	trigger: (page: Page) => Locator;
	/** How the dialog is open and settled. */
	dialog: (page: Page) => Promise<unknown>;
	/** The Cancel button that answers it. */
	cancel: (page: Page) => Locator;
	/** Esc is already covered for this dialog by another spec. */
	escCovered?: boolean;
}

const CASES: Case[] = [
	{
		name: 'Alerts, New alert',
		path: '/alerts',
		mocks: (mock) => mock.on('listSavedSearches', [saved]),
		trigger: (page) => page.getByTestId('alerts-new'),
		dialog: (page) => dialogSettled(page),
		cancel: (page) => page.getByRole('dialog').getByRole('button', { name: 'Cancel' }),
		escCovered: true // alerts.keyboard.test.ts: "Escape closes the save dialog ..."
	},
	{
		name: 'Settings → Companions, the update offer',
		path: '/settings/companions',
		mocks: (mock) => {
			new SettingsBackend(
				{ sections: [] } as unknown as components['schemas']['SettingsIndex'],
				[]
			).attach(mock as unknown as MockLike);
			const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
			on('listCompanions', { companions: [companion] });
			on('getCompanionInstallOffer', {
				id: 'qbittorrent',
				licence: 'GPL-2.0-or-later',
				name: 'qBittorrent',
				replaces: '5.0.0',
				sha256: SHA,
				size_bytes: 12_582_912,
				version: '5.0.1'
			});
		},
		trigger: (page) => page.getByRole('button', { name: 'Update' }),
		dialog: (page) => dialogSettled(page),
		cancel: (page) => page.getByRole('dialog').getByRole('button', { name: 'Cancel' })
	}
];

for (const c of CASES) {
	test.describe(`focus returns: ${c.name}`, () => {
		for (const how of c.escCovered ? (['Cancel'] as const) : (['Esc', 'Cancel'] as const)) {
			test(`closed with ${how}`, async ({ page, go, mock, kbd }) => {
				await liveEvents(page);
				c.mocks(mock);
				await go(c.path);
				const trigger = c.trigger(page);
				await expect(trigger).toBeVisible();
				await kbd.tabTo(trigger, 80);
				await kbd.press('Enter');
				await c.dialog(page);
				if (how === 'Esc') await kbd.press('Escape');
				else {
					await kbd.tabTo(c.cancel(page), 20);
					await kbd.press('Enter');
				}
				await expect(page.getByRole('dialog')).toHaveCount(0);
				await expect(trigger).toBeFocused();
			});
		}
	});
}
