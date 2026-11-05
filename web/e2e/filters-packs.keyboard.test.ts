// RP-6, keyboard only: Settings → Filters & Safety → Packs. Accepting a pending weakening version
// asks for the password and sends the previewed versions with it; a strengthening one does not ask.
// A changelog is text. Axe checks the notice, both dialogs' states, the done state, the override
// editor, paused / changed / unreadable overrides and the corpus badges.
import { expectNoA11yViolations } from './a11y';
import type { OpenApiMock } from './mock/openapi-mock';
import { expect, test } from './support/test';

const preview = {
	quarantine: 0,
	tombstones: 0,
	stored: 120,
	removed_from_floor: 0,
	caught_30d: 14,
	core_caught_30d: 0,
	per_day: 1,
	estimate: false
};
const acceptance = {
	rule: 'MW-2',
	version: 2,
	applied: 1,
	direction: 'widens',
	disagreements: 0,
	heavier: 0,
	in_doubt: false,
	lighter: 0,
	newly_held: 4,
	no_longer_held: 0,
	review_quarantine: 0,
	review_tombstones: 0,
	strengthens: true
};
const strengthening = {
	weakening: false,
	confirm: false,
	warning: null,
	preview,
	neutralize: null,
	pack_versions: ['MW-2@2'],
	pack_acceptance: [acceptance]
};
const weakening = {
	...strengthening,
	weakening: true,
	confirm: true,
	warning: 'Accepting MW-2 version 2 lets some torrents through that version 1 held.',
	pack_acceptance: [{ ...acceptance, strengthens: false, in_doubt: true, direction: null }]
};

function serve(mock: OpenApiMock, assessment: typeof weakening | typeof strengthening) {
	mock.on('getFilters', {
		allow: [],
		core: [],
		cs1_entries: 0,
		exceptions: [],
		ip_ranges: 0,
		overrides: [],
		pack_conditions: [],
		packs: [
			{
				id: 'malware',
				on: true,
				version: 'MW-2@2',
				changelog: '## MW-2@2\nCatches <b>more</b> installers.',
				rules: [
					{
						id: 'MW-2',
						on: true,
						version: 2,
						applied: 1,
						pending: 2,
						action: 'hide',
						base_action: 'hide',
						base_changed: false
					}
				]
			}
		],
		packs_seeded_over_rules: null,
		pending_floor_removals: [],
		pending_pack_versions: [
			{
				pack: 'malware',
				rule: 'MW-2',
				applied: 1,
				version: 2,
				direction: 'widens',
				changelog: [{ version: 2, direction: 'widens', text: 'Catches <b>more</b> installers.' }]
			}
		],
		quarantine: 0,
		rules: [],
		subscriptions: [],
		version: 4
	});
	mock.on('getFilterQueue', { decisions_30d: [], next_cursor: null, quarantine: [] });
	mock.on('getFilterFixtures', {
		core: [],
		packs: {
			error: null,
			failures: [],
			passes: true,
			precision_failing: 0,
			precision_items: 10,
			rules: [
				{
					pack: 'malware',
					rule: 'MW-2',
					version: 2,
					must_match: 5,
					must_not_match: 5,
					failing: 0,
					gaps: 0,
					direction_disagreements: 0
				}
			]
		}
	});
	mock.on('getSettings', {
		section: 'content',
		restart_required: [],
		revision: 1,
		fields: {
			show_adult: {
				value: false,
				env: null,
				read_only: false,
				restart_required: false,
				source: 'default'
			}
		}
	});
	mock.on('startFilterPreviewJob', { status: 202, body: { job: 1 } });
	mock.on('getFilterPreviewJob', {
		state: 'done',
		progress: { phase: 'done', done: 1, total: 1 },
		assessment
	});
	mock.on('cancelFilterPreviewJob', { ok: true });
	mock.on('applyFilterChange', { applied: true, assessment, version: 5 });
}

test('a weakening version asks for the password, which goes with the previewed versions', async ({
	page,
	go,
	mock,
	kbd
}) => {
	serve(mock, weakening);
	await go('/settings/filters');
	await expect(page.getByTestId('pack-pending-MW-2')).toContainText('Version 1 to 2');
	// The changelog is text: the tag is shown, not run.
	await expect(page.getByTestId('pack-pending-MW-2')).toContainText('<b>more</b>');
	await expectNoA11yViolations(page);
	const accept = page.getByTestId('pack-accept-MW-2');
	await kbd.tabTo(accept, 80);
	await kbd.press('Enter');
	await expect(page.getByLabel('Password')).toBeFocused();
	await expect(page.getByTestId('pack-in-doubt')).toHaveText(
		"We can't confirm this change is safe, so it needs your password."
	);
	await expectNoA11yViolations(page);
	expect(mock.calls('applyFilterChange')).toHaveLength(0);
	await page.keyboard.type('keyboard-password');
	await kbd.press('Enter');
	await expect(
		page.getByTestId('change-dialog').getByText('The change was applied.')
	).toBeVisible();
	await expectNoA11yViolations(page);
	await expect.poll(() => mock.calls('applyFilterChange').length).toBe(1);
	const sent = mock.calls('applyFilterChange').map((r) => r.postDataJSON());
	expect(sent[0].change).toEqual({
		op: 'accept_pack_versions',
		rules: ['MW-2'],
		versions: ['MW-2@2']
	});
	expect(sent[0].password).toBe('keyboard-password');
});

test('Esc on the acceptance sends nothing and returns focus', async ({ page, go, mock, kbd }) => {
	serve(mock, weakening);
	await go('/settings/filters');
	const accept = page.getByTestId('pack-accept-MW-2');
	await kbd.tabTo(accept, 80);
	await kbd.press('Enter');
	await expect(page.getByLabel('Password')).toBeFocused();
	await kbd.press('Escape');
	await expect(page.getByRole('dialog')).toHaveCount(0);
	await expect(accept).toBeFocused();
	expect(mock.calls('applyFilterChange')).toHaveLength(0);
});

test('a strengthening version is accepted without a password', async ({ page, go, mock, kbd }) => {
	serve(mock, strengthening);
	await go('/settings/filters');
	await kbd.tabTo(page.getByTestId('pack-accept-MW-2'), 80);
	await kbd.press('Enter');
	await expect(page.getByTestId('pack-impact-MW-2')).toBeVisible();
	await expect(page.getByLabel('Password')).toHaveCount(0);
	await expectNoA11yViolations(page);
	await kbd.tabTo(page.getByTestId('change-apply'), 20);
	await kbd.press('Enter');
	await expect(
		page.getByTestId('change-dialog').getByText('The change was applied.')
	).toBeVisible();
	await expectNoA11yViolations(page);
	await expect.poll(() => mock.calls('applyFilterChange').length).toBe(1);
	const sent = mock.calls('applyFilterChange').map((r) => r.postDataJSON());
	expect(sent[0].password).toBeUndefined();
	expect(sent[0].change.versions).toEqual(['MW-2@2']);
});

test('overrides and corpus states have no axe violations; Quarantine is not offered', async ({
	page,
	go,
	mock,
	kbd
}) => {
	serve(mock, strengthening);
	const base = {
		id: 'MW-2',
		on: true,
		version: 2,
		applied: 2,
		pending: null,
		action: 'hide',
		base_action: 'hide',
		base_changed: true
	};
	const ov = {
		action: 'drop',
		base: 1,
		changed: true,
		paused: false,
		unless: null,
		unreadable: false
	};
	const rule = (id: string, pack_override: object) => ({ ...base, id, pack_override });
	mock.on('getFilters', {
		allow: [],
		core: [],
		cs1_entries: 0,
		exceptions: [],
		ip_ranges: 0,
		overrides: [],
		pack_conditions: [],
		packs: [
			{
				id: 'malware',
				on: true,
				version: 'MW-2@2',
				changelog: '## MW-2@2\nText.',
				rules: [
					rule('MW-1', { ...ov, paused: true }),
					rule('MW-2', ov),
					rule('MW-3', { ...ov, unreadable: true, action: null, base: null })
				]
			},
			{ id: 'lures', on: true, version: 'LU-1@1', changelog: '', rules: [] }
		],
		packs_seeded_over_rules: null,
		pending_floor_removals: [],
		pending_pack_versions: [],
		quarantine: 0,
		rules: [],
		subscriptions: [],
		version: 4
	});
	mock.on('getFilterFixtures', {
		core: [],
		packs: {
			error: null,
			failures: ['x'],
			passes: false,
			precision_failing: 0,
			precision_items: 10,
			rules: [
				{
					pack: 'malware',
					rule: 'MW-2',
					version: 2,
					must_match: 5,
					must_not_match: 5,
					failing: 2,
					gaps: 1,
					direction_disagreements: 0
				}
			]
		}
	});
	await go('/settings/filters');
	await expect(page.getByTestId('pack-paused-MW-1')).toBeVisible();
	await expect(page.getByTestId('pack-changed-MW-2')).toBeVisible();
	await expect(page.getByTestId('pack-override-MW-3')).toContainText("can't be read");
	await expect(page.getByTestId('pack-corpus-malware')).toContainText('2 to look at');
	await expect(page.getByTestId('pack-corpus-global')).toBeVisible();
	await expect(page.getByTestId('pack-corpus-lures')).toContainText('Not known');
	await expect(page.getByTestId('pack-confirm-MW-1')).toBeVisible();
	await page.getByTestId('pack-edit-MW-2').focus();
	await kbd.press('Enter');
	const options = await page.getByTestId('pack-action-MW-2').locator('option').allTextContents();
	expect(options).toEqual(['Flag', 'Demote', 'Hide', 'Drop']);
	await expectNoA11yViolations(page);
});
