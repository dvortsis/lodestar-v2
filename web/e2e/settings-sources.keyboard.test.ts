// Settings → Sources (SRC-15a) against the OpenAPI mock, by keyboard alone, with axe: rows grouped
// by kind, status as icon and text, a source that needs setup opens its consent dialog and sends
// nothing until asked, and a switch or priority change shows only what the server answers.
// Waits are conditions only.
import { expectNoA11yViolations } from './a11y';
import { expect as baseExpect, test } from './support/test';
import {
	SettingsBackend,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';
import { dht, nyaa, prowlarr } from '../src/lib/features/sources/fixtures';
import type { components } from '../src/lib/api/schema';

type Source = components['schemas']['Source'];

const expect = baseExpect.configure({ timeout: 10_000 });

test.describe('keyboard: Settings → Sources', () => {
	let sources: Source[];
	let sent: unknown[];
	test.beforeEach(async ({ mock }) => {
		new SettingsBackend().attach(mock as unknown as MockLike);
		sources = [dht, nyaa, prowlarr];
		sent = [];
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		on('listSources', () => ({ status: 200, body: { sources } }));
		on('patchSource', (req: { url(): string; postDataJSON(): unknown }) => {
			const body = req.postDataJSON() as Record<string, unknown>;
			sent.push(body);
			const id = Number(new URL(req.url()).pathname.split('/').at(-1));
			sources = sources.map((s) =>
				s.id === id
					? ({ ...s, ...body, restart_required: 'enabled' in body || s.restart_required } as Source)
					: s
			);
			return { status: 200, body: sources.find((s) => s.id === id) };
		});
	});

	test('groups the sources, shows status and passes axe', async ({ page, go }) => {
		await go('/settings/sources');
		await expect(page.getByRole('heading', { level: 1, name: 'Sources' })).toBeVisible();
		await expect(page.getByTestId('source-0-status')).toContainText('Running');
		await expect(page.getByTestId('source-1-status')).toContainText('Needs setup');
		await expect(page.getByTestId('source-1-need')).toBeVisible();
		await expect(page.getByTestId('sources-group-importer')).toBeVisible();
		await expectNoA11yViolations(page);
	});

	test('changes priority and turns a source off by keyboard, keeping focus', async ({
		page,
		go,
		kbd
	}) => {
		await go('/settings/sources');
		// Priority sits in the row's detail: open it first (X-sources-restructure).
		const expand = page.getByTestId('source-0-expand');
		await expect(expand).toHaveAccessibleName('Mainline DHT');
		await kbd.tabTo(expand, 40);
		await kbd.press('Enter');
		await expect(expand).toHaveAttribute('aria-expanded', 'true');
		const toggle = page.getByRole('switch', { name: 'Mainline DHT: on' });
		await kbd.tabTo(toggle, 10);
		await kbd.press('Space');
		await expect(page.getByTestId('sources-announce')).toHaveText('Mainline DHT updated.');
		await expect(page.getByTestId('source-0-restart')).toBeVisible();
		await expect(toggle).toBeFocused();
		const priority = page.getByRole('combobox', { name: 'Mainline DHT: priority' });
		await kbd.tabTo(priority, 5);
		await priority.selectOption('high');
		await expect(priority).toHaveValue('high');
		await expect(priority).toBeFocused();
		expect(sent).toEqual([{ enabled: false }, { priority: 'high' }]);
		await expectNoA11yViolations(page);
	});

	test('a source that needs setup opens its consent dialog and sends nothing until asked', async ({
		page,
		go
	}) => {
		await go('/settings/sources');
		const toggle = page.getByRole('switch', { name: 'Nyaa: on' });
		await expect(toggle).toBeEnabled();
		await toggle.focus();
		await page.keyboard.press('Space');
		const dialog = page.getByRole('dialog', { name: 'Nyaa connects to outside services.' });
		await expect(dialog).toBeVisible();
		await expect(toggle).not.toBeChecked();
		await expectNoA11yViolations(page);
		await page.keyboard.press('Escape');
		await expect(dialog).toBeHidden();
		await expect(toggle).toBeFocused();
		await expect(toggle).not.toBeChecked();
		expect(sent).toEqual([]);
	});
});
