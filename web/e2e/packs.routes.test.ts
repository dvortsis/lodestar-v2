// RP-G, the rule-pack gate's e2e run: Settings → Filters & Safety → Packs, with a pointer, against
// the OpenAPI mock acting as a small server that keeps the pack state between requests (each
// apply changes what `GET /filters` returns next). It views the packs, accepts a pending version
// that strengthens (no password), sets an override that strengthens, then makes two weakening
// changes, a lighter override and a narrowing version: each asks for the password, a wrong one
// is refused and sends the change no further, and the right one goes with what the preview asked
// to be confirmed (`base`, `versions`). Whether a change weakens is the server's answer; this run
// checks the screen asks, sends and shows what the server says. The server side of the same gate
// is crates/api/tests/packs_gate.rs. Named `*.routes.test.ts` so the `core` project runs it.
import type { Page } from '@playwright/test';
import type { components } from '../src/lib/api/schema';
import { expectNoA11yViolations } from './a11y';
import type { OpenApiMock } from './mock/openapi-mock';
import { expect, test } from './support/test';

type Assessment = components['schemas']['Assessment'];
type PackRuleState = components['schemas']['PackRuleState'];
type PackAcceptance = components['schemas']['PackAcceptance'];

const PW = 'e2e-admin-password';

const preview = {
	quarantine: 0,
	tombstones: 0,
	stored: 40,
	removed_from_floor: 0,
	caught_30d: 12,
	core_caught_30d: 0,
	per_day: 1,
	estimate: false
};

/** The pack state the mock serves, changed by each apply. */
type State = {
	mw2: { applied: number; pending: number | null };
	sp4: { applied: number | null; pending: number | null };
	mw1: { action: string; unless: string | null } | null;
	version: number;
};

const rule = (over: Partial<PackRuleState> & { id: string }): PackRuleState => ({
	on: true,
	version: 1,
	applied: 1,
	pending: null,
	action: 'hide',
	base_action: 'hide',
	base_changed: false,
	...over
});

function filters(s: State) {
	const mw1 = rule({
		id: 'MW-1',
		action: s.mw1?.action ?? 'hide',
		...(s.mw1
			? {
					pack_override: {
						action: s.mw1.action,
						base: 1,
						changed: false,
						paused: false,
						unless: s.mw1.unless,
						unreadable: false
					}
				}
			: {})
	});
	const mw2 = rule({ id: 'MW-2', version: 2, applied: s.mw2.applied, pending: s.mw2.pending });
	const sp4 = rule({ id: 'SP-4', applied: s.sp4.applied, pending: s.sp4.pending });
	const pending = [
		...(s.mw2.pending
			? [
					{
						pack: 'malware',
						rule: 'MW-2',
						applied: s.mw2.applied,
						version: 2,
						direction: 'narrows',
						changelog: [
							{ version: 2, direction: 'narrows', text: 'Lets music production packs through.' }
						]
					}
				]
			: []),
		...(s.sp4.pending
			? [
					{
						pack: 'spam',
						rule: 'SP-4',
						applied: s.sp4.applied,
						version: 1,
						direction: 'widens',
						changelog: [{ version: 1, direction: 'widens', text: 'Hides clones of hidden spam.' }]
					}
				]
			: [])
	];
	return {
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
				version: 'MW-1@1 MW-2@2',
				changelog: '## MW-2@2\nLets music production packs through.',
				rules: [mw1, mw2]
			},
			{
				id: 'spam',
				on: true,
				version: 'SP-4@1',
				changelog: '## SP-4@1\nHides clones of hidden spam.',
				rules: [sp4]
			}
		],
		packs_seeded_over_rules: null,
		pending_floor_removals: [],
		pending_pack_versions: pending,
		quarantine: 0,
		rules: [],
		subscriptions: [],
		version: s.version
	};
}

const acceptance = (over: Partial<PackAcceptance>): PackAcceptance => ({
	rule: 'SP-4',
	version: 1,
	applied: null,
	direction: 'widens',
	disagreements: 0,
	heavier: 0,
	in_doubt: false,
	lighter: 0,
	newly_held: 3,
	no_longer_held: 0,
	review_quarantine: 0,
	review_tombstones: 0,
	strengthens: true,
	...over
});

/** The server's assessment of a change, as the filters API makes it. */
function assess(change: Record<string, unknown>): Assessment {
	const plain = {
		weakening: false,
		confirm: false,
		warning: null,
		preview,
		neutralize: null
	};
	const weakening = (warning: string) => ({ ...plain, weakening: true, confirm: true, warning });
	switch (change.op) {
		case 'accept_pack_versions':
			return (change.rules as string[]).includes('MW-2')
				? {
						...weakening(
							'Accepting MW-2 version 2 lets some torrents through that version 1 held.'
						),
						pack_versions: ['MW-2@2'],
						pack_acceptance: [
							acceptance({
								rule: 'MW-2',
								version: 2,
								applied: 1,
								direction: 'narrows',
								strengthens: false,
								newly_held: 0,
								no_longer_held: 4
							})
						]
					}
				: { ...plain, pack_versions: ['SP-4@1'], pack_acceptance: [acceptance({})] };
		case 'set_pack_override':
			return change.action === 'drop'
				? { ...plain, pack_base: 1 }
				: {
						...weakening('Your override makes MW-1 lighter: it flags what it hid.'),
						pack_base: 1
					};
		default:
			throw new Error(`no assessment for ${JSON.stringify(change)}`);
	}
}

function serve(mock: OpenApiMock): State {
	const s: State = {
		mw2: { applied: 1, pending: 2 },
		sp4: { applied: null, pending: 1 },
		mw1: null,
		version: 4
	};
	let previewed: Record<string, unknown> | null = null;
	mock.on('getFilters', () => filters(s));
	mock.on('getFilterQueue', { decisions_30d: [], next_cursor: null, quarantine: [] });
	mock.on('getFilterFixtures', {
		core: [],
		packs: {
			error: null,
			failures: [],
			passes: true,
			precision_failing: 0,
			precision_items: 38,
			rules: ['MW-1', 'MW-2', 'SP-4'].map((r) => ({
				pack: r === 'SP-4' ? 'spam' : 'malware',
				rule: r,
				version: r === 'MW-2' ? 2 : 1,
				must_match: 3,
				must_not_match: 3,
				failing: 0,
				gaps: 0,
				direction_disagreements: 0
			}))
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
	mock.on('startFilterPreviewJob', (req) => {
		previewed = req.postDataJSON().change;
		return { status: 202, body: { job: 1 } };
	});
	mock.on('getFilterPreviewJob', () => ({
		state: 'done' as const,
		progress: { phase: 'done' as const, done: 1, total: 1 },
		assessment: assess(previewed!)
	}));
	mock.on('cancelFilterPreviewJob', { ok: true });
	mock.on('applyFilterChange', (req) => {
		const { change, password } = req.postDataJSON();
		const a = assess(change);
		if (a.confirm && password === undefined) {
			return { status: 428, body: { error: { code: 'confirm_required', message: 'Confirm.' } } };
		}
		if (a.confirm && password !== PW) {
			return { status: 401, body: { error: { code: 'bad_password', message: 'Wrong.' } } };
		}
		// What the server checks before it applies a weakening change: a 409, which the spec's
		// own checks of what was sent then explain.
		if (
			a.confirm &&
			((change.op === 'accept_pack_versions' &&
				JSON.stringify(change.versions) !== JSON.stringify(a.pack_versions)) ||
				(change.op === 'set_pack_override' && change.base !== a.pack_base))
		) {
			return { status: 409, body: { error: { code: 'changed_since_preview', message: 'Moved.' } } };
		}
		switch (change.op) {
			case 'accept_pack_versions':
				for (const r of change.rules as string[]) {
					if (r === 'MW-2') s.mw2 = { applied: 2, pending: null };
					if (r === 'SP-4') s.sp4 = { applied: 1, pending: null };
				}
				break;
			case 'set_pack_override':
				s.mw1 = { action: change.action, unless: change.unless || null };
				break;
		}
		s.version += 1;
		return { applied: true, assessment: a, version: s.version };
	});
	return s;
}

const dialog = (page: Page) => page.getByTestId('change-dialog');
const applied = (page: Page) => dialog(page).getByText('The change was applied.');

async function close(page: Page) {
	await page.keyboard.press('Escape');
	await expect(page.getByRole('dialog')).toHaveCount(0);
}

const sent = (mock: OpenApiMock) =>
	mock.calls('applyFilterChange').map((r) => r.postDataJSON() as Record<string, unknown>);

test('view the packs, accept a pending version and set an override', async ({ page, go, mock }) => {
	serve(mock);
	await go('/settings/filters');
	// View: both pending versions, with their changelogs, and the corpus status.
	await expect(page.getByTestId('pack-pending-MW-2')).toContainText('Version 1 to 2');
	await expect(page.getByTestId('pack-pending-MW-2')).toContainText('music production');
	await expect(page.getByTestId('pack-pending-SP-4')).toBeVisible();
	await expect(page.getByTestId('pack-rule-pending-MW-2')).toHaveText('Version 2 waiting');
	await expect(page.getByTestId('pack-corpus-malware')).toContainText('All examples pass');
	await expect(page.getByTestId('pack-corpus-spam')).toContainText('All examples pass');
	await expect(page.getByTestId('pack-corpus-global')).toHaveCount(0);
	await expectNoA11yViolations(page);

	// SP-4 widens and its shadow agrees: accepted without a password.
	await page.getByTestId('pack-accept-SP-4').click();
	await expect(page.getByTestId('pack-impact-SP-4')).toBeVisible();
	await expect(page.getByLabel('Password')).toHaveCount(0);
	await page.getByTestId('change-apply').click();
	await expect(applied(page)).toBeVisible();
	expect(sent(mock)).toEqual([
		{ change: { op: 'accept_pack_versions', rules: ['SP-4'], versions: ['SP-4@1'] } }
	]);
	await close(page);
	// The screen reads the server again: SP-4 isn't waiting, MW-2 still is.
	await expect(page.getByTestId('pack-pending-SP-4')).toHaveCount(0);
	await expect(page.getByTestId('pack-rule-pending-SP-4')).toHaveCount(0);
	await expect(page.getByTestId('pack-pending-MW-2')).toBeVisible();

	// An override heavier than MW-1's own action: no password.
	await page.getByTestId('pack-edit-MW-1').click();
	await page.getByTestId('pack-action-MW-1').selectOption('drop');
	await page.getByTestId('pack-save-MW-1').click();
	await expect(page.getByLabel('Password')).toHaveCount(0);
	await page.getByTestId('change-apply').click();
	await expect(applied(page)).toBeVisible();
	expect(sent(mock)[1].change).toEqual({
		op: 'set_pack_override',
		rule: 'MW-1',
		action: 'drop',
		unless: '',
		base: 1
	});
	expect(sent(mock)[1].password).toBeUndefined();
	await close(page);
	// Base beside yours.
	await expect(page.getByTestId('pack-vs-MW-1')).toContainText('Base');
	await expect(page.getByTestId('pack-vs-MW-1')).toContainText('Hide');
	await expect(page.getByTestId('pack-vs-MW-1')).toContainText('Yours');
	await expect(page.getByTestId('pack-vs-MW-1')).toContainText('Drop');
	await expectNoA11yViolations(page);
});

test('a weakening change asks for the password; a wrong one sends nothing further', async ({
	page,
	go,
	mock
}) => {
	const s = serve(mock);
	await go('/settings/filters');
	await expect(page.getByTestId('pack-pending-MW-2')).toBeVisible();

	// A lighter override of MW-1: the warning and the password, then the previewed base.
	await page.getByTestId('pack-edit-MW-1').click();
	await page.getByTestId('pack-action-MW-1').selectOption('flag');
	await page.getByTestId('pack-save-MW-1').click();
	const password = page.getByLabel('Password');
	await expect(password).toBeVisible();
	await expect(dialog(page)).toContainText('it flags what it hid');
	await expectNoA11yViolations(page);
	expect(mock.calls('applyFilterChange')).toHaveLength(0);
	await password.fill('not the password');
	await page.getByTestId('change-apply').click();
	await expect(page.getByTestId('change-password')).toHaveAttribute('aria-invalid', 'true');
	await expect(page.getByRole('alert')).toContainText('That password is wrong');
	// The wrong one went once, with the previewed base, and changed nothing.
	expect(sent(mock)).toHaveLength(1);
	expect(sent(mock)[0].password).toBe('not the password');
	expect(sent(mock)[0].change).toMatchObject({ op: 'set_pack_override', base: 1 });
	expect(s.mw1).toBeNull();
	await password.fill(PW);
	await page.getByTestId('change-apply').click();
	await expect(applied(page)).toBeVisible();
	expect(sent(mock)).toHaveLength(2);
	const override = sent(mock)[1];
	expect(override.change).toEqual({
		op: 'set_pack_override',
		rule: 'MW-1',
		action: 'flag',
		unless: '',
		base: 1
	});
	expect(override.password).toBe(PW);
	await close(page);
	await expect(page.getByTestId('pack-vs-MW-1')).toContainText('Flag');

	// Accepting MW-2@2, which narrows: the password, then the previewed versions.
	await page.getByTestId('pack-accept-MW-2').click();
	await expect(page.getByLabel('Password')).toBeVisible();
	await expect(page.getByTestId('pack-impact-MW-2')).toBeVisible();
	await expect(dialog(page)).toContainText('lets some torrents through');
	await page.getByLabel('Password').fill(PW);
	await page.getByTestId('change-apply').click();
	await expect(applied(page)).toBeVisible();
	expect(sent(mock)).toHaveLength(3);
	const accept = sent(mock)[2];
	expect(accept.change).toEqual({
		op: 'accept_pack_versions',
		rules: ['MW-2'],
		versions: ['MW-2@2']
	});
	expect(accept.password).toBe(PW);
	await close(page);
	await expect(page.getByTestId('pack-pending-MW-2')).toHaveCount(0);
	expect(s.mw2).toEqual({ applied: 2, pending: null });
});
