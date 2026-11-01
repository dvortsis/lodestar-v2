// Settings → Updates (S-updates) against the OpenAPI mock, by keyboard alone, with axe: the screen
// renders what the server says; Check now; Skip this version behind its confirmation, where Cancel
// sends nothing; and the security toggle. Waits are conditions only.
import { expectNoA11yViolations } from './a11y';
import { expect as baseExpect, test } from './support/test';
import {
	SettingsBackend,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';
import type { components } from '../src/lib/api/schema';

const expect = baseExpect.configure({ timeout: 10_000 });
type Index = components['schemas']['SettingsIndex'];
type Values = components['schemas']['SectionValues'];
type Status = components['schemas']['UpdatesStatus'];

const index = {
	sections: [
		{
			section: 'updates',
			label_key: 'settings.updates.label',
			help_key: 'settings.updates.help',
			fields: [],
			schema: { type: 'object', properties: {} }
		}
	]
} as unknown as Index;
const field = (value: unknown) => ({
	value,
	env: null,
	read_only: false,
	restart_required: false,
	source: 'default' as const
});
const values: Values[] = [
	{
		section: 'updates',
		revision: 1,
		restart_required: [],
		fields: {
			mode: field('notify'),
			level: field('fixes'),
			security_fixes: field('auto'),
			channel: field('stable'),
			wait_days: field(3),
			window: field('02:00-05:00'),
			allow_reindex: field(false),
			notify_before: field(false),
			notify_after: field(false),
			notify_failure: field(true)
		}
	}
];

const status = (): Status => ({
	capability: { can_install: true, reason: null },
	history: [
		{
			at: 1_699_000_000,
			from: '0.5.1',
			level: 'fix',
			lost_torrents: null,
			reason: null,
			result: 'installed',
			version: '0.5.2'
		}
	],
	notices: [],
	pending: null,
	policy: {
		allow_reindex: false,
		held_in_process: false,
		level: 'fixes',
		mode: 'notify',
		next_window_at: null,
		paused_until: null,
		security_fixes: false,
		wait_days: 3,
		window: '02:00-05:00',
		window_zone: 'UTC'
	},
	releases: [
		{
			blocked_by: [],
			date: 1_700_000_000,
			eligible: false,
			installs_at: null,
			level: 'fix',
			manual_steps: false,
			migrates: false,
			notes: 'Fixes a crash on start.',
			notes_url: 'https://github.com/dvortsis/lodestar-v2/releases/tag/v0.5.3',
			reindex: false,
			reindex_bytes: 0,
			security: true,
			version: '0.5.3'
		}
	],
	rollback: { available: false, lost_torrents: null, to_version: null },
	running: {
		channel: 'stable',
		image_version: null,
		overlay: false,
		revoked_key: false,
		version: '0.5.2',
		withdrawn: false
	},
	status: { checked_at: 1_699_990_000, error: null, next_check_at: null, state: 'ok' },
	unavailable: null
});

test.describe('keyboard: Settings → Updates', () => {
	let sent: { op: string; body: unknown }[];
	test.beforeEach(async ({ mock }) => {
		new SettingsBackend(index, values).attach(mock as unknown as MockLike);
		sent = [];
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		on('getUpdates', () => ({ status: 200, body: status() }));
		on('getSystem', () => ({ status: 200, body: { deployment: { kind: 'docker' } } }));
		for (const op of ['checkUpdates', 'skipUpdate'])
			on(op, (r: { postDataJSON(): unknown }) => {
				sent.push({ op, body: op === 'skipUpdate' ? r.postDataJSON() : null });
				return { status: 200, body: status() };
			});
	});

	test('renders what the server says, and passes axe', async ({ page, go }) => {
		await go('/settings/updates');
		await expect(page.getByRole('heading', { level: 1, name: 'Updates' })).toBeVisible();
		await expect(page.getByTestId('updates-available')).toHaveText('0.5.3 available');
		await expect(page.getByTestId('updates-mode-notify')).toBeChecked();
		await expect(page.getByTestId('updates-contacts')).toContainText('contacts GitHub');
		await expect(page.getByRole('link', { name: 'Read on GitHub' })).toHaveAttribute(
			'rel',
			'noopener noreferrer'
		);
		await expectNoA11yViolations(page);
	});

	test('checks now, and skips only after the confirmation', async ({ page, go, kbd }) => {
		await go('/settings/updates');
		await kbd.tabTo(page.getByTestId('updates-check'), 200);
		await kbd.press('Enter');
		await expect.poll(() => sent.map((s) => s.op)).toEqual(['checkUpdates']);

		await kbd.tabTo(page.getByTestId('updates-skip'), 200);
		await kbd.press('Enter');
		const dialog = page.getByRole('dialog', { name: 'Skip 0.5.3?' });
		await expect(dialog).toBeVisible();
		await expectNoA11yViolations(page);
		await kbd.press('Escape');
		await expect(dialog).toBeHidden();
		expect(sent.map((s) => s.op)).toEqual(['checkUpdates']);

		await kbd.tabTo(page.getByTestId('updates-skip'), 200);
		await kbd.press('Enter');
		await kbd.tabTo(dialog.getByRole('button', { name: 'Skip this version' }), 10);
		await kbd.press('Enter');
		await expect.poll(() => sent.map((s) => s.op)).toEqual(['checkUpdates', 'skipUpdate']);
		expect(sent[1]?.body).toEqual({ version: '0.5.3' });
	});

	test('Advanced: the channel dialog by keyboard, Cancel sends nothing', async ({
		page,
		go,
		kbd,
		mock
	}) => {
		const patches: unknown[] = [];
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		const inner = new SettingsBackend(index, values);
		inner.attach(mock as unknown as MockLike);
		on('getUpdates', () => ({ status: 200, body: status() }));
		on('getSystem', () => ({ status: 200, body: { deployment: { kind: 'docker' } } }));
		await go('/settings/updates');
		const summary = page.getByText('Advanced', { exact: true });
		await kbd.tabTo(summary, 300);
		await kbd.press('Enter');
		const nightly = page.getByTestId('updates-channel-nightly');
		await expect(nightly).toBeVisible();
		await kbd.tabTo(page.getByTestId('updates-channel-stable'), 20);
		await kbd.press('ArrowDown');
		await kbd.press('ArrowDown');
		const dialog = page.getByRole('dialog', { name: 'Switch to Nightly?' });
		await expect(dialog).toBeVisible();
		await expect(dialog).toContainText('for testing only');
		await expectNoA11yViolations(page);
		await kbd.press('Escape');
		await expect(dialog).toBeHidden();
		await expect(page.getByTestId('updates-channel-stable')).toBeChecked();
		expect(patches).toEqual([]);
		expect(inner.patches).toEqual([]);
		await expectNoA11yViolations(page);
	});
});
