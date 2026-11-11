// Settings → Companions (CMP-9a) against the OpenAPI mock: the list renders what the server says,
// the install confirmation shows the offer and sends nothing on Cancel, only the offer's version
// and SHA-256 on Install, a refusal reads as plain words, and axe finds nothing. Waits are
// conditions only. (Named `.routes.test.ts` so the `core` project picks it up.)
import { expectNoA11yViolations } from './a11y';
import { dialogSettled } from './support/dialog';
import { expect as baseExpect, test } from './support/test';
import {
	SettingsBackend,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';
import type { components } from '../src/lib/api/schema';

const expect = baseExpect.configure({ timeout: 10_000 });
type Index = components['schemas']['SettingsIndex'];
type Companion = components['schemas']['Companion'];
type Offer = components['schemas']['InstallOffer'];

const SHA = '0123456789abcdef'.repeat(4);
const index = { sections: [] } as unknown as Index;
const rows: Companion[] = [
	{
		available: '5.0.1',
		credentials: ['webui_password'],
		health: 'healthy',
		id: 'qbittorrent',
		installed: '5.0.0',
		licence: 'GPL-2.0-or-later',
		name: 'qBittorrent',
		opt_in: false,
		refusal: null,
		templates: ['compose']
	},
	{
		available: null,
		credentials: [],
		health: null,
		id: 'prowlarr',
		installed: null,
		licence: 'GPL-3.0-only',
		name: 'Prowlarr',
		opt_in: false,
		refusal: 'native_in_container',
		templates: []
	}
];
const offer: Offer = {
	id: 'qbittorrent',
	licence: 'GPL-2.0-or-later',
	name: 'qBittorrent',
	replaces: '5.0.0',
	sha256: SHA,
	size_bytes: 12_582_912,
	version: '5.0.1'
};

test.describe('Settings → Companions', () => {
	let installs: unknown[];
	test.beforeEach(async ({ mock }) => {
		new SettingsBackend(index, []).attach(mock as unknown as MockLike);
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		installs = [];
		on('listCompanions', { companions: rows });
		on('getCompanionInstallOffer', offer);
		on('installCompanion', (request: { postDataJSON(): unknown }) => {
			installs.push(request.postDataJSON());
			return { status: 200, body: { ...rows[0], installed: '5.0.1', available: null } };
		});
	});

	test('lists the companions with a refusal in words, and passes axe', async ({ page, go }) => {
		await go('/settings/companions');
		await expect(page.getByRole('heading', { name: 'qBittorrent' })).toBeVisible();
		await expect(page.getByText(/can't install this program itself/)).toBeVisible();
		await expect(page.getByText('native_in_container')).toHaveCount(0);
		await expectNoA11yViolations(page);
	});

	test('Cancel sends nothing; Install sends only the offer', async ({ page, go }) => {
		await go('/settings/companions');
		await page.getByRole('button', { name: 'Update' }).click();
		await dialogSettled(page);
		const dialog = page.getByRole('dialog');
		await expect(dialog).toContainText(SHA);
		await expect(dialog).toContainText('GPL-2.0-or-later');
		await expectNoA11yViolations(page);
		await dialog.getByRole('button', { name: 'Cancel' }).click();
		await expect(dialog).toHaveCount(0);
		expect(installs).toEqual([]);
		await page.getByRole('button', { name: 'Update' }).click();
		await dialogSettled(page);
		await page.getByRole('dialog').getByRole('button', { name: 'Update' }).click();
		await expect.poll(() => installs.length).toBe(1);
		expect(installs[0]).toEqual({ version: '5.0.1', sha256: SHA });
	});
});
