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
		egress: { from: 'saved', setting: { route: 'direct' } },
		health: 'healthy',
		id: 'qbittorrent',
		installed: '5.0.0',
		licence: 'GPL-2.0-or-later',
		name: 'qBittorrent',
		opt_in: false,
		refusal: null,
		runs: 'lodestar',
		templates: ['compose']
	},
	{
		available: null,
		credentials: [],
		egress: null,
		health: null,
		id: 'prowlarr',
		installed: null,
		licence: 'GPL-3.0-only',
		name: 'Prowlarr',
		opt_in: false,
		refusal: 'native_in_container',
		runs: 'own_container',
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

	// X-checksum-wrap: the checksum wraps 32+32 or not at all, and copies as the 64 characters. Each
	// character's line comes from a one-character Range's box, so the check reads real line boxes.
	for (const [width, height] of [
		[1366, 900],
		[390, 844]
	] as const) {
		test(`the checksum wraps evenly and copies whole at ${width} px`, async ({
			page,
			context,
			go
		}) => {
			await page.setViewportSize({ width, height });
			await context.grantPermissions(['clipboard-read', 'clipboard-write']);
			await go('/settings/companions');
			await page.getByRole('button', { name: 'Update' }).click();
			await dialogSettled(page);
			const sha = page.locator('[data-offer-sha]');
			await expect(sha).toBeVisible();
			const lines = () =>
				sha.evaluate((dd) => {
					const tops: number[] = [];
					for (const node of Array.from(dd.childNodes)) {
						if (node.nodeType !== Node.TEXT_NODE) continue;
						const text = node.textContent ?? '';
						for (let i = 0; i < text.length; i++) {
							if (/\s/.test(text[i]!)) continue;
							const range = document.createRange();
							range.setStart(node, i);
							range.setEnd(node, i + 1);
							tops.push(Math.round(range.getBoundingClientRect().top));
						}
					}
					const counts = new Map<number, number>();
					for (const top of tops) counts.set(top, (counts.get(top) ?? 0) + 1);
					return [...counts.entries()].sort((a, b) => a[0] - b[0]).map(([, n]) => n);
				});
			await expect.poll(async () => JSON.stringify(await lines())).toMatch(/^\[(64|32,32)\]$/);
			test
				.info()
				.annotations.push({ type: 'checksum lines', description: JSON.stringify(await lines()) });
			// Nothing sticks out of the dialog sideways.
			expect(await sha.evaluate((dd) => dd.scrollWidth <= dd.clientWidth)).toBe(true);
			expect(await page.getByRole('dialog').evaluate((d) => d.scrollWidth <= d.clientWidth)).toBe(
				true
			);
			const selected = await sha.evaluate((dd) => {
				const range = document.createRange();
				range.selectNodeContents(dd);
				const selection = window.getSelection()!;
				selection.removeAllRanges();
				selection.addRange(range);
				return selection.toString();
			});
			expect(selected).toBe(SHA);
			// Headless Chromium shares one clipboard across a worker's contexts: clear it first, so an
			// earlier test's copy can't pass this read.
			await page.evaluate(() => navigator.clipboard.writeText(''));
			await page.keyboard.press('ControlOrMeta+c');
			await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(SHA);
		});
	}
});
